# Proposed High-Value Analyzers

This document outlines high-value analyzer ideas with exact implementation details that could significantly improve the pg-plansight tool.

---

## 1. AggregationEfficiencyAnalyzer

**Value**: Detects inefficient GROUP BY and DISTINCT operations that could be optimized.

### What It Analyzes

- Expensive aggregations (HashAggregate, GroupAggregate)
- Inefficient sort-based aggregations vs hash aggregations
- GROUP BY operations on unsorted data
- DISTINCT operations that could use indexes

### Implementation Details

```rust
struct AggregationEfficiencyAnalyzer;

// Detection logic:
fn analyze_aggregation(&mut self, node: &PlanNode, path: &NodePath) {
    match &node.node_type {
        NodeType::Aggregate(AggregateType::HashAggregate) => {
            // 1. Check if hash aggregate is spilling to disk
            if let Some(batches_str) = node.get_property("HashAgg Batches") {
                if let Ok(batches) = batches_str.parse::<u32>() {
                    if batches > 1 {
                        // Hash aggregate spilled to disk (batches > 1)
                        severity = if batches > 10 { Critical } else { High };
                        finding = "Hash aggregation spilling to disk";
                        recommendation = "Increase work_mem or reduce grouping columns";
                    }
                }
            }

            // 2. Check hash aggregate memory usage
            if node.cost.estimated_rows > 1_000_000 {
                let estimated_mem_kb = (node.cost.estimated_rows as f64 *
                                       node.cost.estimated_width as f64) / 1024.0;
                if estimated_mem_kb > context.work_mem_kb as f64 {
                    finding = "Hash aggregate may exceed work_mem";
                    severity = High;
                }
            }
        }

        NodeType::Aggregate(AggregateType::GroupAggregate) => {
            // 3. Check if input is already sorted by group keys
            // GroupAggregate requires sorted input - check if child is Sort
            if !node.children.iter().any(|c| matches!(c.node_type, NodeType::Utility(UtilityType::Sort { .. }))) {
                // No sort child but using GroupAggregate - input must be sorted via index
                // This is good! Flag as efficient pattern
            } else {
                // Has explicit Sort child - check if sort is expensive
                if let Some(sort_child) = node.children.iter().find(|c| matches!(c.node_type, NodeType::Utility(UtilityType::Sort { .. }))) {
                    if sort_child.cost.startup_cost > 10000.0 {
                        finding = "Expensive sort before group aggregate";
                        recommendation = "Consider HashAggregate or index on GROUP BY columns";
                    }
                }
            }
        }

        NodeType::Utility(UtilityType::Unique) => {
            // 4. Check if DISTINCT could use indexes
            if node.cost.estimated_rows > 10000 {
                // Check if there's a sort child
                if node.children.iter().any(|c| matches!(c.node_type, NodeType::Utility(UtilityType::Sort { .. }))) {
                    finding = "DISTINCT operation requires sorting";
                    severity = Medium;
                    recommendation = "Consider index on DISTINCT columns or window functions";
                }
            }
        }
        _ => {}
    }
}
```

### Key Thresholds
- **Hash Batches > 1**: Spilling to disk (High severity)
- **Hash Batches > 10**: Severe spilling (Critical)
- **GroupAggregate with sort > 10K cost**: Inefficient (Medium)
- **Estimated memory > work_mem**: Potential spill (High)

---

## 2. SubqueryPatternAnalyzer

**Value**: Identifies inefficient subquery patterns that could be rewritten as JOINs or CTEs.

### What It Analyzes

- Correlated subqueries (SubPlan with parameters)
- Semi-joins that could be regular joins
- Scalar subqueries executed multiple times
- EXIST/NOT EXISTS that could be LEFT JOIN

### Implementation Details

```rust
struct SubqueryPatternAnalyzer;

fn analyze_subplan(&mut self, node: &PlanNode, path: &NodePath) {
    // 1. Detect correlated subqueries
    if let Some(subplan_name) = node.get_property("Subplan Name") {
        // Check if it's parameterized (correlated)
        if let Some(params) = node.get_property("Params Evaluated") {
            let parent_rows = self.get_parent_rows(path);
            let subplan_cost = node.cost.max_total_cost;

            // Multiply subplan cost by parent rows (executed per row)
            let total_cost = subplan_cost * parent_rows as f64;

            if total_cost > 50000.0 {
                finding = format!(
                    "Correlated subquery '{}' executed {} times with total cost {:.0}",
                    subplan_name, parent_rows, total_cost
                );
                severity = if total_cost > 500000.0 { Critical } else { High };
                recommendation = "Rewrite as JOIN, use LATERAL, or materialize subquery results";
            }
        }
    }

    // 2. Detect SubqueryScan (materialized subquery)
    if let NodeType::Scan(ScanType::SubqueryScan { .. }) = &node.node_type {
        // Check if it's filtering after subquery (inefficient)
        if let Some(filter) = node.get_property("Filter") {
            let filtered_rows = node.cost.estimated_rows;
            // Check child to see how many rows were produced
            if let Some(child) = node.children.first() {
                let child_rows = child.cost.estimated_rows;
                let selectivity = filtered_rows as f64 / child_rows as f64;

                if selectivity < 0.1 && child_rows > 10000 {
                    finding = format!(
                        "Subquery produces {} rows but only {} are used ({:.1}% selectivity)",
                        child_rows, filtered_rows, selectivity * 100.0
                    );
                    severity = High;
                    recommendation = "Move filter into subquery WHERE clause to reduce rows earlier";
                }
            }
        }
    }

    // 3. Detect InitPlan (uncorrelated subquery executed once)
    if let Some(initplan) = node.get_property("InitPlan") {
        // InitPlans are generally fine, but check if result is used efficiently
        if node.cost.max_total_cost > 10000.0 {
            finding = format!("Expensive InitPlan subquery: cost {:.0}", node.cost.max_total_cost);
            severity = Medium;
            recommendation = "Consider materializing result or using CTE";
        }
    }
}

fn detect_semi_join_opportunity(&mut self, node: &PlanNode, path: &NodePath) {
    // 4. Detect EXISTS/IN that could be semi-joins
    if let NodeType::Join(JoinType::NestedLoop { .. }) = &node.node_type {
        // Check if there's a SubPlan in join condition
        if let Some(join_filter) = node.get_property("Join Filter") {
            if join_filter.contains("SubPlan") || join_filter.contains("EXISTS") {
                let left_rows = node.children.get(0).map(|c| c.cost.estimated_rows).unwrap_or(0);
                let right_rows = node.children.get(1).map(|c| c.cost.estimated_rows).unwrap_or(0);

                if left_rows > 1000 && right_rows > 1000 {
                    finding = "Nested loop with subplan in join filter - consider hash semi-join";
                    severity = High;
                    recommendation = "Rewrite EXISTS/IN as explicit JOIN with appropriate indexes";
                }
            }
        }
    }
}
```

### Key Patterns
- **Correlated SubPlan cost * parent rows > 50K**: Inefficient (High)
- **SubqueryScan filter selectivity < 10%**: Filter too late (High)
- **InitPlan cost > 10K**: Could be optimized (Medium)
- **NestedLoop with SubPlan filter**: Could be semi-join (High)

---

## 3. StatisticsStaleAnalyzer

**Value**: Detects when planner estimates are significantly wrong, indicating stale statistics.

### What It Analyzes

- Large differences between estimated and actual rows (when EXPLAIN ANALYZE data available)
- Cardinality misestimation patterns
- Join result size misestimations

### Implementation Details

```rust
struct StatisticsStaleAnalyzer;

fn analyze_estimation_accuracy(&mut self, node: &PlanNode, path: &NodePath) {
    // Check if we have actual execution data (EXPLAIN ANALYZE)
    if let (Some(actual_rows_str), estimated_rows) = (
        node.get_property("Actual Rows"),
        node.cost.estimated_rows
    ) {
        if let Ok(actual_rows) = actual_rows_str.parse::<u64>() {
            // Calculate estimation error ratio
            let ratio = if estimated_rows > 0 {
                actual_rows as f64 / estimated_rows as f64
            } else if actual_rows > 0 {
                f64::INFINITY
            } else {
                1.0
            };

            // 1. Severe underestimation (actual >> estimated)
            if ratio > 10.0 && actual_rows > 1000 {
                finding = format!(
                    "Severe underestimation: estimated {} rows, actual {} ({}x off)",
                    estimated_rows, actual_rows, ratio as u64
                );
                severity = if ratio > 100.0 { Critical } else { High };
                recommendation = format!(
                    "Run ANALYZE on table '{}'. Underestimation can cause poor join methods.",
                    extract_table_name(node)
                );
            }

            // 2. Severe overestimation (estimated >> actual)
            if ratio < 0.1 && estimated_rows > 10000 {
                finding = format!(
                    "Severe overestimation: estimated {} rows, actual {} ({}x off)",
                    estimated_rows, actual_rows, (1.0/ratio) as u64
                );
                severity = High;
                recommendation = format!(
                    "Run ANALYZE on table '{}'. Overestimation wastes memory in hash joins.",
                    extract_table_name(node)
                );
            }

            // 3. Track misestimation pattern across plan
            self.estimation_errors.push(ratio);
        }
    }

    // 4. Detect correlation issues (no actual rows data)
    if let NodeType::Join(_) = &node.node_type {
        let left_rows = node.children.get(0).map(|c| c.cost.estimated_rows).unwrap_or(0);
        let right_rows = node.children.get(1).map(|c| c.cost.estimated_rows).unwrap_or(0);
        let join_rows = node.cost.estimated_rows;

        // Check for independence assumption violation
        let expected_cartesian = (left_rows as f64 * right_rows as f64).min(left_rows as f64);

        if join_rows as f64 > expected_cartesian * 0.9 {
            finding = "Join result nearly cartesian - may indicate missing foreign key stats";
            severity = Medium;
            recommendation = "Check for missing indexes or run ANALYZE with increased statistics target";
        }
    }
}

fn summarize_statistics_health(&mut self) -> AnalysisReport {
    // Calculate overall statistics health score
    if !self.estimation_errors.is_empty() {
        let avg_error: f64 = self.estimation_errors.iter()
            .map(|&ratio| if ratio > 1.0 { ratio } else { 1.0 / ratio })
            .sum::<f64>() / self.estimation_errors.len() as f64;

        if avg_error > 5.0 {
            finding = format!(
                "Overall poor cardinality estimates (avg {}x error across {} nodes)",
                avg_error as u64, self.estimation_errors.len()
            );
            severity = High;
            recommendation = "Run ANALYZE on all involved tables, consider increasing statistics target";
        }
    }
}
```

### Key Thresholds
- **Ratio > 10x with > 1K rows**: Severe underestimation (High)
- **Ratio > 100x**: Critical underestimation (Critical)
- **Ratio < 0.1x with > 10K est**: Severe overestimation (High)
- **Average error > 5x**: Poor statistics overall (High)

---

## 4. LockContentionAnalyzer

**Value**: Identifies query patterns that may cause lock contention.

### What It Analyzes

- Row-level locking patterns
- Sequential scans with row locks (LockRows node)
- Multiple tables accessed in different orders (deadlock risk)
- SELECT FOR UPDATE/SHARE without indexes

### Implementation Details

```rust
struct LockContentionAnalyzer;

fn analyze_locking_pattern(&mut self, node: &PlanNode, path: &NodePath) {
    if let NodeType::Utility(UtilityType::LockRows) = &node.node_type {
        // Check child nodes to see what's being locked
        if let Some(child) = node.children.first() {
            let locked_rows = child.cost.estimated_rows;

            // 1. Locking many rows
            if locked_rows > 10000 {
                finding = format!("SELECT FOR UPDATE/SHARE locking {} rows", locked_rows);
                severity = if locked_rows > 100000 { Critical } else { High };
                recommendation = "Consider batching, using LIMIT, or redesigning to avoid large locks";
            }

            // 2. Locking after sequential scan (no index used)
            if matches!(child.node_type, NodeType::Scan(ScanType::SeqScan { .. })) {
                finding = "Row locking requires sequential scan - may cause prolonged locks";
                severity = High;
                recommendation = "Add index on filter columns to quickly locate rows to lock";
            }

            // 3. Locking with expensive operation beforehand
            if child.cost.max_total_cost > 10000.0 {
                finding = format!(
                    "Expensive operation (cost {:.0}) before acquiring locks",
                    child.cost.max_total_cost
                );
                severity = Medium;
                recommendation = "Locks held during expensive operations can cause contention. Consider splitting query.";
            }
        }
    }
}

fn detect_deadlock_risk(&mut self, plan: &ParsedPlan) {
    // 4. Track table access order
    let table_access_order = self.extract_table_access_order(plan);

    // In a real implementation, you'd track this across multiple queries
    // and detect when the same tables are accessed in different orders
    if table_access_order.len() > 2 {
        finding = format!(
            "Query accesses {} tables. If similar queries access them in different order, deadlocks may occur",
            table_access_order.len()
        );
        severity = Low;
        recommendation = "Ensure all queries access tables in consistent order to prevent deadlocks";
    }
}
```

### Key Patterns
- **LockRows with > 10K rows**: High contention risk (High)
- **LockRows with > 100K rows**: Severe contention (Critical)
- **LockRows + SeqScan**: Prolonged locks (High)
- **LockRows with expensive child > 10K cost**: Long lock duration (Medium)

---

## 5. PartitionPruningAnalyzer

**Value**: Detects when partition pruning fails, causing unnecessary partition scans.

### What It Analyzes

- Append nodes scanning multiple partitions
- Partition filters that prevent pruning
- Function calls on partition keys

### Implementation Details

```rust
struct PartitionPruningAnalyzer;

fn analyze_partition_access(&mut self, node: &PlanNode, path: &NodePath) {
    if let NodeType::Utility(UtilityType::Append) = &node.node_type {
        let partition_count = node.children.len();

        if partition_count == 0 {
            return; // Not a partition scan
        }

        // 1. Scanning many partitions
        if partition_count > 10 {
            let total_rows = node.cost.estimated_rows;
            let avg_rows_per_partition = total_rows / partition_count as u64;

            finding = format!(
                "Query scans {} partitions (avg {} rows each)",
                partition_count, avg_rows_per_partition
            );
            severity = if partition_count > 50 { High } else { Medium };
            recommendation = "Check if partition pruning is working. Ensure WHERE clause matches partition key.";
        }

        // 2. Check if pruning could work but doesn't
        // Look for filters in parent nodes
        if let Some(parent) = self.get_parent_node(path) {
            if let Some(filter) = parent.get_property("Filter") {
                // Check if filter mentions date/partition columns with functions
                if filter.contains("DATE(") || filter.contains("EXTRACT(") || filter.contains("CAST(") {
                    finding = format!(
                        "Functions in WHERE clause on partition key prevent pruning ({} partitions scanned)",
                        partition_count
                    );
                    severity = High;
                    recommendation = "Remove functions from partition key filters. Use direct comparisons like 'date_col >= '2024-01-01''.";
                }
            }
        }

        // 3. All partitions return zero rows (partition key mismatch)
        let nonzero_partitions = node.children.iter()
            .filter(|c| c.cost.estimated_rows > 0)
            .count();

        if nonzero_partitions == 0 && partition_count > 1 {
            finding = format!("Scanning {} partitions but all return 0 rows", partition_count);
            severity = Medium;
            recommendation = "Partition key doesn't match query filter. Consider repartitioning strategy.";
        }

        // 4. Single partition but expensive setup
        if partition_count == 1 && node.cost.startup_cost > 1000.0 {
            finding = "High partition scan startup cost despite single partition access";
            severity = Low;
            recommendation = "Good partition pruning, but startup cost high. Check partition count and inheritance overhead.";
        }
    }
}
```

### Key Thresholds
- **> 10 partitions scanned**: Pruning may not be working (Medium)
- **> 50 partitions scanned**: Pruning definitely failing (High)
- **Functions on partition key**: Prevents pruning (High)
- **All partitions return 0 rows**: Configuration issue (Medium)

---

## Summary Table

| Analyzer | Primary Value | Complexity | Reliability |
|----------|--------------|------------|-------------|
| **AggregationEfficiencyAnalyzer** | Detects memory spills in GROUP BY/DISTINCT | Medium | High |
| **SubqueryPatternAnalyzer** | Identifies N+1 query patterns and correlated subqueries | High | High |
| **StatisticsStaleAnalyzer** | Finds stale stats causing bad plans | Low | High (with EXPLAIN ANALYZE) |
| **LockContentionAnalyzer** | Prevents lock contention and deadlocks | Medium | High |
| **PartitionPruningAnalyzer** | Ensures efficient partition access | Low | High |

## Recommended Implementation Order

1. **AggregationEfficiencyAnalyzer** - Common issue, clear metrics
2. **StatisticsStaleAnalyzer** - Easy to implement, high value
3. **SubqueryPatternAnalyzer** - Complex but very high value
4. **PartitionPruningAnalyzer** - Growing importance with partitioned tables
5. **LockContentionAnalyzer** - Specialized but critical for high-concurrency systems

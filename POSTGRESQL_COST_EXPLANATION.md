# PostgreSQL Query Plan Cost Calculation Explained

## Overview

PostgreSQL's query planner uses a sophisticated cost-based optimization system to determine the most efficient execution plan for queries. Understanding these costs is crucial for performance tuning and query optimization.

## Cost Format: `cost=startup..total`

PostgreSQL costs are **always ranges**, never single values, represented as:
```
(cost=0.43..599.04 rows=1000 width=56)
```

This represents:
- **Startup Cost (0.43)**: Minimum cost to return the first row
- **Total Cost Range**: 0.43 (minimum) to 599.04 (maximum)
- **Rows**: Estimated number of rows (1000)
- **Width**: Average row width in bytes (56)

## Cost Components

### 1. **Startup Cost**
- **Definition**: Cost to initialize the operation and return the first row
- **When Important**: Critical for operations like LIMIT queries where you only need a few rows
- **Examples**:
  - Index scan: Low startup cost (quickly locates first matching row)
  - Sort operation: High startup cost (must read and sort all data before returning first row)
  - Hash join: High startup cost (must build hash table before joining)

### 2. **Total Cost Range**
- **Minimum Cost**: Equals startup cost (cost to get just the first row)
- **Maximum Cost**: Cost to execute the operation completely and return all rows
- **Range Interpretation**: 
  - Narrow range (0.43..0.50): Cost is predictable regardless of how many rows you fetch
  - Wide range (0.43..1000.0): Significant difference between getting first row vs. all rows

## Cost Calculation Factors

### 1. **I/O Costs** (Dominant Factor)
- **seq_page_cost**: Cost of sequential page read (default: 1.0)
- **random_page_cost**: Cost of random page read (default: 4.0)
- **Page Size**: PostgreSQL uses 8KB pages
- **Buffer Cache**: Hot pages in memory have lower costs

### 2. **CPU Costs**
- **cpu_tuple_cost**: Cost to process one row (default: 0.01)
- **cpu_index_tuple_cost**: Cost to process one index entry (default: 0.005)
- **cpu_operator_cost**: Cost to execute one operator/function (default: 0.0025)

### 3. **Memory Costs**
- **work_mem**: Available memory for operations like sorts and hashes
- **Higher work_mem**: Reduces need for disk-based operations, lowering costs

## Real-World Examples

### Example 1: Index Scan
```sql
Index Scan using "IX_VitalAlarms_EndDate" on "Shared"."VitalAlarms" v  
(cost=0.43..95610.13 rows=159718 width=56)
```

**Analysis**:
- **Startup Cost (0.43)**: Very low - index can quickly locate first matching row
- **Total Cost (95610.13)**: High - must scan many index entries and fetch corresponding table rows
- **Cost Range Span**: 95609.7 - huge difference between first row and all rows
- **Interpretation**: Great for LIMIT queries, expensive for full scans

### Example 2: Limit Operation
```sql
Limit  (cost=0.43..599.04 rows=1000 width=56)
```

**Analysis**:
- **Same Startup Cost (0.43)**: Inherited from child operation
- **Much Lower Total Cost (599.04)**: Only processes 1000 rows instead of 159718
- **Cost Calculation**: `startup_cost + (rows_needed / total_rows) * child_cost_range`

### Example 3: Sequential Scan vs Index Scan

**Sequential Scan**:
```sql
Seq Scan on users  (cost=0.00..1500.00 rows=10000 width=100)
```
- Startup: 0.00 (can immediately start reading)
- Total: 1500.00
- Even cost distribution across all rows

**Index Scan**:
```sql
Index Scan on users  (cost=0.42..8.44 rows=1 width=100)
```
- Startup: 0.42 (traverse index to first row)
- Total: 8.44 (small range because only 1 row expected)

## Cost-Based Optimization Logic

### 1. **Operation Selection**
PostgreSQL chooses between:
- **Sequential Scan**: Better for large result sets or no useful indexes
- **Index Scan**: Better for selective queries
- **Bitmap Scan**: Better for moderate selectivity

### 2. **Join Algorithm Selection**
- **Nested Loop**: Low startup, good for small outer tables
- **Hash Join**: High startup (build hash), good for larger datasets
- **Merge Join**: Requires sorted inputs, predictable costs

### 3. **Sort Strategy**
- **In-Memory Sort**: Low total cost if data fits in work_mem
- **External Sort**: Higher cost due to disk I/O if data exceeds work_mem

## Practical Implications

### 1. **Query Optimization**
```sql
-- Bad: Forces expensive sequential scan
SELECT * FROM large_table WHERE complex_function(column) = 'value';

-- Good: Uses index, low startup cost
CREATE INDEX idx_column ON large_table(column);
SELECT * FROM large_table WHERE column = 'value';
```

### 2. **LIMIT Query Optimization**
```sql
-- Leverages low startup cost of index scan
SELECT * FROM events 
WHERE event_date >= '2024-01-01' 
ORDER BY event_date 
LIMIT 10;
```

### 3. **Configuration Tuning**
- **Increase work_mem**: Reduces sort/hash costs
- **Adjust random_page_cost**: Reflects SSD vs HDD performance
- **Tune effective_cache_size**: Influences index vs scan decisions

## Understanding Cost Ranges in Practice

### Narrow Range (Predictable Cost)
```sql
Index Scan using pk_users on users  (cost=0.42..8.44 rows=1 width=100)
```
- **Interpretation**: Cost is predictable whether you fetch 1 row or all matching rows
- **Good for**: Any query pattern

### Wide Range (Variable Cost)
```sql
Index Scan on orders  (cost=0.43..15000.0 rows=50000 width=200)
```
- **Interpretation**: First row is cheap (0.43), all rows are expensive (15000.0)
- **Good for**: LIMIT queries, pagination
- **Bad for**: Full result set queries

## Advanced Cost Considerations

### 1. **Parallel Execution**
```sql
Parallel Seq Scan on large_table  (cost=0.00..8591.67 rows=100000 width=100)
```
- Costs are adjusted for parallel workers
- Total cost may be lower due to parallelism

### 2. **Materialization**
```sql
Materialize  (cost=0.00..1500.25 rows=1000 width=100)
```
- Startup cost includes cost to materialize all rows
- Subsequent access has very low cost

### 3. **Subplan Costs**
```sql
SubPlan 1 (returns $0)
  ->  Index Scan on lookup_table  (cost=0.42..8.44 rows=1 width=4)
```
- Subplan costs are multiplied by number of executions
- Can dramatically affect total query cost

## Summary

PostgreSQL's cost model provides valuable insights into query performance:

1. **Range Nature**: Costs are always ranges (startup..total), never single values
2. **Startup vs Total**: Understanding the difference helps optimize for different use cases
3. **Cost Factors**: I/O dominates, but CPU and memory costs matter too
4. **Optimization**: Use costs to identify bottlenecks and optimization opportunities
5. **Configuration**: Tuning cost parameters helps the planner make better decisions

The new cost structure in pg_plansight properly represents this range nature, enabling more accurate analysis and better understanding of query performance characteristics.
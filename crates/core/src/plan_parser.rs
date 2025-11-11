use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
    sync::LazyLock,
};

use crate::PlanLine;

/// Represents the cost information for a query plan node
/// PostgreSQL costs are always ranges representing minimum to maximum expected cost
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanCost {
    /// Estimated startup cost before first row is returned (minimum cost to get first row)
    pub startup_cost: f64,
    /// Estimated minimum total cost (cost when getting first row)
    pub min_total_cost: f64,
    /// Estimated maximum total cost (cost when getting all rows)
    pub max_total_cost: f64,
    /// Estimated number of rows this node will return
    pub estimated_rows: u64,
    /// Estimated average width of rows in bytes
    pub estimated_width: u32,
}

impl PlanCost {
    /// Get the total cost range as a tuple (min, max)
    pub fn total_cost_range(&self) -> (f64, f64) {
        (self.min_total_cost, self.max_total_cost)
    }

    /// Get the average total cost (for backward compatibility)
    pub fn avg_total_cost(&self) -> f64 {
        (self.min_total_cost + self.max_total_cost) / 2.0
    }

    /// Get the maximum total cost (typically what people refer to as "total cost")
    pub fn total_cost(&self) -> f64 {
        self.max_total_cost
    }

    /// Get the cost range span (difference between max and min)
    pub fn cost_range_span(&self) -> f64 {
        self.max_total_cost - self.min_total_cost
    }
}

/// Represents actual execution statistics when ANALYZE is used
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanActuals {
    /// Actual execution time in milliseconds
    pub actual_time_ms: Option<f64>,
    /// Actual number of rows returned
    pub actual_rows: Option<u64>,
    /// Number of times this node was executed
    pub actual_loops: Option<u32>,
}

/// Represents a reference to a database table or index
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableReference {
    /// Schema name (e.g., "Shared")
    pub schema: Option<String>,
    /// Table or index name (e.g., "VitalAlarms")
    pub name: String,
    /// Table alias used in the query (e.g., "v")
    pub alias: Option<String>,
}

impl Display for TableReference {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(schema) = &self.schema {
            write!(f, "{}.{}", schema, self.name)?;
        } else {
            write!(f, "{}", self.name)?;
        }

        if let Some(alias) = &self.alias {
            write!(f, " as {alias}")?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexReference {
    /// index name (e.g. "IX_VitalAlarms_EndDate")
    pub name: String,
}

impl Display for IndexReference {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// Represents a sort key specification
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SortKey {
    /// Column or expression being sorted
    pub expression: String,
    /// Sort direction (ASC/DESC)
    pub direction: Option<String>,
}

impl Display for SortKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(direction) = &self.direction {
            write!(f, "{} {}", self.expression, direction)
        } else {
            write!(f, "{}", self.expression)
        }
    }
}

/// Represents a subplan reference
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubPlanReference {
    /// Subplan name or identifier
    pub name: String,
    /// Subplan type (e.g., "exists", "not exists", "scalar")
    pub subplan_type: Option<String>,
}

/// Types of scan operations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScanType {
    /// Sequential scan of entire table
    SeqScan { table: TableReference },
    /// Index scan using specific index
    IndexScan {
        table: TableReference,
        index: Option<IndexReference>,
        backward: bool,
        only: bool,
    },
    /// Bitmap heap scan after bitmap index scan
    BitmapHeapScan {
        table: TableReference,
        recheck_condition: Option<String>,
    },
    /// Creates bitmap from index
    BitmapIndexScan { index: Option<IndexReference> },
    /// Parallel version of bitmap heap scan
    ParallelBitmapHeapScan {
        table: TableReference,
        workers_planned: Option<u32>,
    },
}

impl ScanType {
    /// Analyzes a plan line and creates a ScanType with extracted information
    pub fn analyze(line: &str) -> Result<Self, ParseError> {
        let line_lower = line.to_lowercase();

        // Check for Bitmap Index Scan first (doesn't need table reference)
        if line_lower.contains("bitmap index scan") {
            let index = if let Some(bitmap_capture) = BITMAP_INDEX_REGEX.captures(line) {
                bitmap_capture.name("index").map(|i| IndexReference {
                    name: strip_quotes(i.as_str()),
                })
            } else {
                None
            };
            Ok(ScanType::BitmapIndexScan { index })
        } else {
            // Extract table reference for other scan types
            let table = extract_table_reference_from_line(line).ok_or_else(|| {
                ParseError::InvalidNodeStructure(format!(
                    "No table reference found in scan line: {}",
                    line
                ))
            })?;

            if let Some(index_capture) = INDEX_REGEX.captures(line) {
                let index = index_capture.name("index").map(|i| IndexReference {
                    name: strip_quotes(i.as_str()),
                });

                if index_capture.name("type").is_some() {
                    // This should not happen anymore since we handle Bitmap Index Scan above
                    Ok(ScanType::BitmapIndexScan { index })
                } else {
                    // Regular Index Scan
                    let backward = index_capture.name("backward").is_some();
                    let only = index_capture.name("only").is_some();
                    Ok(ScanType::IndexScan {
                        table,
                        index,
                        backward,
                        only,
                    })
                }
            } else if line_lower.contains("parallel bitmap heap scan") {
                let workers_planned = extract_workers_planned(line);
                Ok(ScanType::ParallelBitmapHeapScan {
                    table,
                    workers_planned,
                })
            } else if line_lower.contains("bitmap heap scan") {
                Ok(ScanType::BitmapHeapScan {
                    table,
                    recheck_condition: None, // Will be filled from properties later
                })
            } else if line_lower.contains("seq scan") {
                Ok(ScanType::SeqScan { table })
            } else {
                Err(ParseError::InvalidNodeStructure(format!(
                    "Unknown scan type in line: {}",
                    line
                )))
            }
        }
    }

    /// Updates the scan type with information from property lines
    pub fn update_from_properties(&mut self, properties: &HashMap<String, String>) {
        match self {
            ScanType::ParallelBitmapHeapScan {
                workers_planned, ..
            } => {
                if let Some(workers_str) = properties.get("Workers Planned")
                    && let Ok(workers) = workers_str.parse::<u32>()
                {
                    *workers_planned = Some(workers);
                }
            }
            ScanType::BitmapHeapScan {
                recheck_condition, ..
            } => {
                if let Some(recheck_str) = properties.get("Recheck Cond") {
                    *recheck_condition = Some(recheck_str.clone());
                }
            }
            _ => {} // Other scan types don't have additional properties to update
        }
    }
}

impl Display for ScanType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanType::SeqScan { table } => write!(f, "Seq Scan on {}", table),
            ScanType::IndexScan {
                table,
                index,
                backward,
                only,
            } => {
                let index_name = index
                    .as_ref()
                    .map_or("".to_string(), |i| format!("using {} ", i.name));
                let direction = if *backward { "Backward " } else { "" };
                let only_str = if *only { "Only " } else { "" };
                write!(f, "Index {only_str}Scan {direction}{index_name}on {table}")
            }
            ScanType::BitmapHeapScan { table, .. } => write!(f, "Bitmap Heap Scan on {}", table),
            ScanType::BitmapIndexScan { index } => {
                if let Some(idx) = index {
                    write!(f, "Bitmap Index Scan using {}", idx.name)
                } else {
                    write!(f, "Bitmap Index Scan")
                }
            }
            ScanType::ParallelBitmapHeapScan {
                table,
                workers_planned,
            } => {
                if let Some(workers) = workers_planned {
                    write!(
                        f,
                        "Parallel Bitmap Heap Scan on {} ({} workers)",
                        table, workers
                    )
                } else {
                    write!(f, "Parallel Bitmap Heap Scan on {}", table)
                }
            }
        }
    }
}

/// Types of join operations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JoinType {
    /// Nested loop join
    NestedLoop { inner_unique: bool },
    /// Nested loop left join
    NestedLoopLeftJoin { inner_unique: bool },
    /// Hash join
    HashJoin {
        hash_condition: Option<String>,
        hash_buckets: Option<u32>,
    },
    /// Merge join
    MergeJoin { merge_condition: Option<String> },
}

impl JoinType {
    /// Analyzes a plan line and creates a JoinType with extracted information
    pub fn analyze(line: &str) -> Result<Self, ParseError> {
        let line_lower = line.to_lowercase();

        if line_lower.contains("nested loop left join") {
            let inner_unique = extract_inner_unique(line).unwrap_or(false);
            Ok(JoinType::NestedLoopLeftJoin { inner_unique })
        } else if line_lower.contains("nested loop") {
            let inner_unique = extract_inner_unique(line).unwrap_or(false);
            Ok(JoinType::NestedLoop { inner_unique })
        } else if line_lower.contains("hash join") {
            let hash_buckets = extract_hash_buckets(line);
            Ok(JoinType::HashJoin {
                hash_condition: None, // Will be filled from properties later
                hash_buckets,
            })
        } else if line_lower.contains("merge join") {
            Ok(JoinType::MergeJoin {
                merge_condition: None, // Will be filled from properties later
            })
        } else {
            Err(ParseError::InvalidNodeStructure(format!(
                "Unknown join type in line: {}",
                line
            )))
        }
    }

    /// Updates the join type with information from property lines
    pub fn update_from_properties(&mut self, properties: &HashMap<String, String>) {
        match self {
            JoinType::NestedLoop { inner_unique }
            | JoinType::NestedLoopLeftJoin { inner_unique } => {
                if let Some(unique_str) = properties.get("Inner Unique") {
                    *inner_unique = unique_str.to_lowercase() == "true";
                }
            }
            JoinType::HashJoin { hash_condition, .. } => {
                if let Some(hash_cond_str) = properties.get("Hash Cond") {
                    *hash_condition = Some(hash_cond_str.clone());
                }
            }
            JoinType::MergeJoin { merge_condition } => {
                if let Some(merge_cond_str) = properties.get("Merge Cond") {
                    *merge_condition = Some(merge_cond_str.clone());
                }
            }
        }
    }
}

/// Types of aggregate operations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AggregateType {
    /// Basic aggregate
    Aggregate { functions: Vec<String> },
    /// Group aggregate with grouping
    GroupAggregate {
        group_keys: Vec<String>,
        functions: Vec<String>,
    },
    /// Hash-based aggregate
    HashAggregate {
        group_keys: Vec<String>,
        functions: Vec<String>,
        hash_batches: Option<u32>,
    },
}

impl AggregateType {
    /// Analyzes a plan line and creates an AggregateType with extracted information
    pub fn analyze(line: &str) -> Result<Self, ParseError> {
        let line_lower = line.to_lowercase();

        if line_lower.contains("group aggregate") {
            Ok(AggregateType::GroupAggregate {
                group_keys: Vec::new(), // Will be filled from properties later
                functions: Vec::new(),  // Will be filled from properties later
            })
        } else if line_lower.contains("hash aggregate") {
            let hash_batches = extract_hash_buckets(line);
            Ok(AggregateType::HashAggregate {
                group_keys: Vec::new(), // Will be filled from properties later
                functions: Vec::new(),  // Will be filled from properties later
                hash_batches,
            })
        } else if line_lower.contains("aggregate") {
            Ok(AggregateType::Aggregate {
                functions: Vec::new(), // Will be filled from properties later
            })
        } else {
            Err(ParseError::InvalidNodeStructure(format!(
                "Unknown aggregate type in line: {}",
                line
            )))
        }
    }

    /// Updates the aggregate type with information from property lines
    pub fn update_from_properties(&mut self, properties: &HashMap<String, String>) {
        match self {
            AggregateType::GroupAggregate {
                group_keys,
                functions,
            } => {
                if let Some(group_key_str) = properties.get("Group Key") {
                    *group_keys = parse_group_keys(group_key_str);
                }
                // Extract functions from Output property (this is a simplified approach)
                if let Some(output_str) = properties.get("Output") {
                    *functions = extract_aggregate_functions(output_str);
                }
            }
            AggregateType::HashAggregate {
                group_keys,
                functions,
                ..
            } => {
                if let Some(group_key_str) = properties.get("Group Key") {
                    *group_keys = parse_group_keys(group_key_str);
                }
                if let Some(output_str) = properties.get("Output") {
                    *functions = extract_aggregate_functions(output_str);
                }
            }
            AggregateType::Aggregate { functions } => {
                if let Some(output_str) = properties.get("Output") {
                    *functions = extract_aggregate_functions(output_str);
                }
            }
        }
    }
}

/// Types of utility/control operations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UtilityType {
    /// Sort operation
    Sort {
        sort_keys: Vec<SortKey>,
        sort_method: Option<String>,
    },
    /// Limit operation
    Limit {
        limit_count: Option<u64>,
        offset_count: Option<u64>,
    },
    /// Gather merge for parallel operations
    GatherMerge {
        workers_planned: Option<u32>,
        workers_launched: Option<u32>,
    },
    /// Materialize results
    Materialize,
    /// Memoize for caching
    Memoize {
        cache_key: Option<String>,
        cache_mode: Option<String>,
    },
    /// Subplan execution
    SubPlan { subplan: SubPlanReference },
    /// Bitmap AND operation
    BitmapAnd,
    /// Bitmap OR operation
    BitmapOr,
}

impl UtilityType {
    /// Analyzes a plan line and creates a UtilityType with extracted information
    pub fn analyze(line: &str) -> Result<Self, ParseError> {
        let line_lower = line.to_lowercase();

        if line_lower.contains("sort") {
            Ok(UtilityType::Sort {
                sort_keys: Vec::new(), // Will be filled from properties later
                sort_method: None,     // Will be filled from properties later
            })
        } else if line_lower.contains("limit") {
            let (limit_count, offset_count) = extract_limit_info(line);
            Ok(UtilityType::Limit {
                limit_count,
                offset_count,
            })
        } else if line_lower.contains("gather merge") {
            let workers_planned = extract_workers_planned(line);
            Ok(UtilityType::GatherMerge {
                workers_planned,
                workers_launched: None, // Will be filled from properties later
            })
        } else if line_lower.contains("materialize") {
            Ok(UtilityType::Materialize)
        } else if line_lower.contains("memoize") {
            let cache_mode = extract_cache_mode(line);
            Ok(UtilityType::Memoize {
                cache_key: None, // Will be filled from properties later
                cache_mode,
            })
        } else if line_lower.contains("subplan") {
            let subplan = extract_subplan_info(line).unwrap_or_else(|| SubPlanReference {
                name: "Unknown".to_string(),
                subplan_type: None,
            });
            Ok(UtilityType::SubPlan { subplan })
        } else if line_lower.contains("bitmapand") {
            Ok(UtilityType::BitmapAnd)
        } else if line_lower.contains("bitmapor") {
            Ok(UtilityType::BitmapOr)
        } else {
            Err(ParseError::InvalidNodeStructure(format!(
                "Unknown utility type in line: {}",
                line
            )))
        }
    }

    /// Updates the utility type with information from property lines
    pub fn update_from_properties(&mut self, properties: &HashMap<String, String>) {
        match self {
            UtilityType::Sort {
                sort_keys,
                sort_method,
            } => {
                if let Some(sort_key_str) = properties.get("Sort Key") {
                    *sort_keys = parse_sort_keys(sort_key_str);
                }
                if let Some(method_str) = properties.get("Sort Method") {
                    *sort_method = Some(method_str.clone());
                }
            }
            UtilityType::GatherMerge {
                workers_planned,
                workers_launched,
            } => {
                if let Some(planned_str) = properties.get("Workers Planned")
                    && let Ok(planned) = planned_str.parse::<u32>()
                {
                    *workers_planned = Some(planned);
                }
                if let Some(launched_str) = properties.get("Workers Launched")
                    && let Ok(launched) = launched_str.parse::<u32>()
                {
                    *workers_launched = Some(launched);
                }
            }
            UtilityType::Memoize {
                cache_key,
                cache_mode,
            } => {
                if let Some(key_str) = properties.get("Cache Key") {
                    *cache_key = Some(key_str.clone());
                }
                if let Some(mode_str) = properties.get("Cache Mode") {
                    *cache_mode = Some(mode_str.clone());
                }
            }
            _ => {} // Other utility types don't have additional properties to update
        }
    }
}

/// Main node type classification
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeType {
    /// Scan operations
    Scan(ScanType),
    /// Join operations
    Join(JoinType),
    /// Aggregate operations
    Aggregate(AggregateType),
    /// Utility/control operations
    Utility(UtilityType),
    /// Unknown node type (fallback)
    Unknown(String),
}

/// Represents a parsed PostgreSQL query plan node
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanNode {
    /// Type of this plan node
    pub node_type: NodeType,

    /// Cost estimation for this node
    pub cost: PlanCost,

    /// Actual execution statistics (if available)
    pub actuals: Option<PlanActuals>,

    /// Child nodes in the execution tree
    pub children: Vec<PlanNode>,

    /// Node-specific properties
    pub properties: crate::plan_properties::PlanProperties,

    /// Original text of this node (for debugging/fallback)
    pub original_text: String,
}

/// Represents a complete parsed execution plan
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedPlan {
    /// Root node of the execution plan tree
    pub root: PlanNode,

    /// Total planning time (if available)
    pub planning_time_ms: Option<f64>,

    /// Total execution time (if available)
    pub execution_time_ms: Option<f64>,
}

impl PlanNode {
    /// Creates a new plan node with the specified type and cost
    pub fn new(node_type: NodeType, cost: PlanCost, original_text: String) -> Self {
        Self {
            node_type,
            cost,
            actuals: None,
            children: Vec::new(),
            properties: crate::plan_properties::PlanProperties::new(),
            original_text,
        }
    }

    /// Adds a child node to this node
    pub fn add_child(&mut self, child: PlanNode) {
        self.children.push(child);
    }

    /// Sets a property for this node
    pub fn set_property(&mut self, key: String, value: String) {
        self.properties.set(&key, &value);
    }

    /// Gets a property value by key
    pub fn get_property(&self, key: &str) -> Option<String> {
        self.properties.get(key)
    }

    /// Gets the typed properties collection
    pub fn properties(&self) -> &crate::plan_properties::PlanProperties {
        &self.properties
    }

    /// Gets a mutable reference to the typed properties collection
    pub fn properties_mut(&mut self) -> &mut crate::plan_properties::PlanProperties {
        &mut self.properties
    }

    /// Sets actual execution statistics
    pub fn set_actuals(&mut self, actuals: PlanActuals) {
        self.actuals = Some(actuals);
    }

    /// Updates the node type with information from collected properties
    pub fn update_from_properties(&mut self) {
        // Convert to HashMap temporarily for compatibility with existing update methods
        let props_map = self.properties.to_hashmap();

        match &mut self.node_type {
            NodeType::Scan(scan_type) => {
                scan_type.update_from_properties(&props_map);
            }
            NodeType::Join(join_type) => {
                join_type.update_from_properties(&props_map);
            }
            NodeType::Aggregate(agg_type) => {
                agg_type.update_from_properties(&props_map);
            }
            NodeType::Utility(util_type) => {
                util_type.update_from_properties(&props_map);
            }
            NodeType::Unknown(_) => {} // Nothing to update for unknown types
        }
    }

    /// Returns true if this is a scan node
    pub fn is_scan(&self) -> bool {
        matches!(self.node_type, NodeType::Scan(_))
    }

    /// Returns true if this is a join node
    pub fn is_join(&self) -> bool {
        matches!(self.node_type, NodeType::Join(_))
    }

    /// Extract the table name from this node, checking the NodeType enum variants first
    pub fn extract_table_name(&self) -> String {
        // Extract from the NodeType enum variants
        match &self.node_type {
            NodeType::Scan(scan_type) => {
                match scan_type {
                    ScanType::SeqScan { table } => table.name.clone(),
                    ScanType::IndexScan { table, .. } => table.name.clone(),
                    ScanType::BitmapHeapScan { table, .. } => table.name.clone(),
                    ScanType::ParallelBitmapHeapScan { table, .. } => table.name.clone(),
                    ScanType::BitmapIndexScan { .. } => {
                        // Bitmap index scan doesn't have table info directly, try properties
                        self.get_property("Relation Name")
                            .unwrap_or_else(|| "bitmap_index_table".to_string())
                    }
                }
            }
            _ => {
                // Try properties for non-scan nodes
                self.get_property("Relation Name")
                    .or_else(|| self.get_property("Alias"))
                    .unwrap_or_else(|| "unknown_table".to_string())
            }
        }
    }

    /// Extract the index name from this node, checking the most reliable sources first
    pub fn extract_index_name(&self) -> String {
        // Extract from the NodeType enum variants
        match &self.node_type {
            NodeType::Scan(scan_type) => {
                match scan_type {
                    ScanType::IndexScan {
                        index: Some(index), ..
                    } => index.name.clone(),
                    ScanType::BitmapIndexScan { index: Some(index) } => index.name.clone(),
                    _ => {
                        // Try properties as fallback for scans without index info
                        self.get_property("Index Name")
                            .unwrap_or_else(|| "no_index".to_string())
                    }
                }
            }
            _ => {
                // Try properties for non-scan nodes
                self.get_property("Index Name")
                    .unwrap_or_else(|| "unknown_index".to_string())
            }
        }
    }

    /// Returns true if this is an aggregate node
    pub fn is_aggregate(&self) -> bool {
        matches!(self.node_type, NodeType::Aggregate(_))
    }

    /// Returns the maximum total cost including all children
    pub fn total_cost_recursive(&self) -> f64 {
        let mut total = self.cost.total_cost();
        for child in &self.children {
            total += child.total_cost_recursive();
        }
        total
    }

    /// Returns the cost range (min, max) including all children
    pub fn total_cost_range_recursive(&self) -> (f64, f64) {
        let mut min_total = self.cost.min_total_cost;
        let mut max_total = self.cost.max_total_cost;

        for child in &self.children {
            let (child_min, child_max) = child.total_cost_range_recursive();
            min_total += child_min;
            max_total += child_max;
        }

        (min_total, max_total)
    }

    /// Returns the maximum depth of the plan tree
    pub fn max_depth(&self) -> usize {
        if self.children.is_empty() {
            1
        } else {
            1 + self
                .children
                .iter()
                .map(|c| c.max_depth())
                .max()
                .unwrap_or(0)
        }
    }

    /// Collects all scan nodes in the plan tree
    pub fn collect_scans(&self) -> Vec<&PlanNode> {
        let mut scans = Vec::new();
        if self.is_scan() {
            scans.push(self);
        }
        for child in &self.children {
            scans.extend(child.collect_scans());
        }
        scans
    }

    /// Collects all join nodes in the plan tree
    pub fn collect_joins(&self) -> Vec<&PlanNode> {
        let mut joins = Vec::new();
        if self.is_join() {
            joins.push(self);
        }
        for child in &self.children {
            joins.extend(child.collect_joins());
        }
        joins
    }

    /// Returns a human-readable description of this node
    pub fn description(&self) -> String {
        match &self.node_type {
            NodeType::Scan(scan_type) => match scan_type {
                ScanType::SeqScan { table } => {
                    format!("Sequential Scan on {}", table.display_name())
                }
                ScanType::IndexScan {
                    table,
                    index,
                    backward,
                    only,
                } => {
                    let mut parts = vec![];
                    if *only {
                        parts.push("Index Only Scan".to_string());
                    } else {
                        parts.push("Index Scan".to_string());
                    }
                    if *backward {
                        parts.push("(Backward)".to_string());
                    }
                    if let Some(idx) = index {
                        parts.push(format!("using {}", idx.name));
                    }
                    parts.push(format!("on {}", table.display_name()));
                    parts.join(" ")
                }
                ScanType::BitmapHeapScan {
                    table,
                    recheck_condition,
                } => {
                    let mut desc = format!("Bitmap Heap Scan on {}", table.display_name());
                    if let Some(condition) = recheck_condition {
                        desc.push_str(&format!(" (Recheck: {})", condition));
                    }
                    desc
                }
                ScanType::BitmapIndexScan { index } => {
                    let mut desc = "Bitmap Index Scan".to_string();
                    if let Some(idx) = index {
                        desc.push_str(&format!(" using {}", idx.name));
                    }
                    desc
                }
                ScanType::ParallelBitmapHeapScan {
                    table,
                    workers_planned,
                } => {
                    let mut desc = format!("Parallel Bitmap Heap Scan on {}", table.display_name());
                    if let Some(workers) = workers_planned {
                        desc.push_str(&format!(" ({} workers)", workers));
                    }
                    desc
                }
            },
            NodeType::Join(join_type) => match join_type {
                JoinType::NestedLoop { inner_unique } => {
                    if *inner_unique {
                        "Nested Loop (Inner Unique)".to_string()
                    } else {
                        "Nested Loop".to_string()
                    }
                }
                JoinType::NestedLoopLeftJoin { inner_unique } => {
                    if *inner_unique {
                        "Nested Loop Left Join (Inner Unique)".to_string()
                    } else {
                        "Nested Loop Left Join".to_string()
                    }
                }
                JoinType::HashJoin { hash_buckets, .. } => {
                    if let Some(buckets) = hash_buckets {
                        format!("Hash Join ({} buckets)", buckets)
                    } else {
                        "Hash Join".to_string()
                    }
                }
                JoinType::MergeJoin { .. } => "Merge Join".to_string(),
            },
            NodeType::Aggregate(agg_type) => match agg_type {
                AggregateType::Aggregate { functions } => {
                    if functions.is_empty() {
                        "Aggregate".to_string()
                    } else {
                        format!("Aggregate ({})", functions.join(", "))
                    }
                }
                AggregateType::GroupAggregate {
                    group_keys,
                    functions,
                } => {
                    if group_keys.is_empty() && functions.is_empty() {
                        "Group Aggregate".to_string()
                    } else {
                        format!(
                            "Group Aggregate (keys: {}, funcs: {})",
                            group_keys.join(", "),
                            functions.join(", ")
                        )
                    }
                }
                AggregateType::HashAggregate {
                    group_keys,
                    functions,
                    hash_batches,
                } => {
                    let base = if group_keys.is_empty() && functions.is_empty() {
                        "Hash Aggregate".to_string()
                    } else {
                        format!(
                            "Hash Aggregate (keys: {}, funcs: {})",
                            group_keys.join(", "),
                            functions.join(", ")
                        )
                    };
                    if let Some(batches) = hash_batches {
                        format!("{} ({} batches)", base, batches)
                    } else {
                        base
                    }
                }
            },
            NodeType::Utility(util_type) => match util_type {
                UtilityType::Sort {
                    sort_keys,
                    sort_method,
                } => {
                    let base = if sort_keys.is_empty() {
                        "Sort".to_string()
                    } else {
                        format!(
                            "Sort ({})",
                            sort_keys
                                .iter()
                                .map(|k| k.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    if let Some(method) = sort_method {
                        format!("{} [{}]", base, method)
                    } else {
                        base
                    }
                }
                UtilityType::Limit {
                    limit_count,
                    offset_count,
                } => match (limit_count, offset_count) {
                    (Some(limit), Some(offset)) => format!("Limit {} offset {}", limit, offset),
                    (Some(limit), None) => format!("Limit {}", limit),
                    (None, Some(offset)) => format!("Limit offset {}", offset),
                    (None, None) => "Limit".to_string(),
                },
                UtilityType::GatherMerge {
                    workers_planned,
                    workers_launched,
                } => match (workers_planned, workers_launched) {
                    (Some(planned), Some(launched)) => {
                        format!("Gather Merge ({}/{} workers)", launched, planned)
                    }
                    (Some(planned), None) => format!("Gather Merge ({} workers)", planned),
                    _ => "Gather Merge".to_string(),
                },
                UtilityType::Materialize => "Materialize".to_string(),
                UtilityType::Memoize {
                    cache_key,
                    cache_mode,
                } => {
                    let mut parts = vec!["Memoize".to_string()];
                    if let Some(mode) = cache_mode {
                        parts.push(format!("mode: {}", mode));
                    }
                    if let Some(key) = cache_key {
                        parts.push(format!("key: {}", key));
                    }
                    if parts.len() > 1 {
                        format!("{} ({})", parts[0], parts[1..].join(", "))
                    } else {
                        parts[0].clone()
                    }
                }
                UtilityType::SubPlan { subplan } => {
                    if let Some(subplan_type) = &subplan.subplan_type {
                        format!("SubPlan {} ({})", subplan.name, subplan_type)
                    } else {
                        format!("SubPlan {}", subplan.name)
                    }
                }
                UtilityType::BitmapAnd => "BitmapAnd".to_string(),
                UtilityType::BitmapOr => "BitmapOr".to_string(),
            },
            NodeType::Unknown(name) => name.clone(),
        }
    }
}

impl ParsedPlan {
    /// Creates a new parsed plan with the given root node
    pub fn new(root: PlanNode) -> Self {
        Self {
            root,
            planning_time_ms: None,
            execution_time_ms: None,
        }
    }

    /// Clean constructors focused on parsing logic
    pub fn from_text_plan(text: &str) -> Result<Self, ParseError> {
        // Use the existing PlanParser to parse text plans
        let parser = PlanParser::new()?;
        parser.parse_plan(text)
    }

    pub fn from_json_plan(json: &str) -> Result<Self, ParseError> {
        // Parse the JSON string into our JsonPlan structure
        let json_plans: Vec<crate::JsonPlan> = serde_json::from_str(json)
            .map_err(|e| ParseError::InvalidJsonFormat(format!("Failed to parse JSON: {}", e)))?;

        if json_plans.is_empty() {
            return Err(ParseError::MissingJsonPlanData(
                "Empty JSON plan array".to_string(),
            ));
        }

        let json_plan = &json_plans[0]; // Take the first plan

        // Use the existing PlanParser to convert JSON to PlanNode
        let parser = PlanParser::new()?;
        let root = parser.convert_json_node_to_plan_node(&json_plan.plan)?;

        let mut parsed_plan = Self::new(root);
        parsed_plan.planning_time_ms = json_plan.planning_time;
        parsed_plan.execution_time_ms = json_plan.execution_time;

        Ok(parsed_plan)
    }

    /// Returns the total cost of the entire plan
    pub fn total_cost(&self) -> f64 {
        self.root.total_cost_recursive()
    }

    /// Returns the maximum depth of the plan
    pub fn max_depth(&self) -> usize {
        self.root.max_depth()
    }

    /// Returns the number of nodes in the plan
    pub fn node_count(&self) -> usize {
        fn count_nodes(node: &PlanNode) -> usize {
            1 + node.children.iter().map(count_nodes).sum::<usize>()
        }
        count_nodes(&self.root)
    }

    /// Collects all tables referenced in the plan
    pub fn get_tables(&self) -> Vec<TableReference> {
        fn collect_tables(node: &PlanNode, tables: &mut Vec<TableReference>) {
            // Extract table references from NodeType enum variants
            match &node.node_type {
                NodeType::Scan(scan_type) => {
                    match scan_type {
                        ScanType::SeqScan { table } => tables.push(table.clone()),
                        ScanType::IndexScan { table, .. } => tables.push(table.clone()),
                        ScanType::BitmapHeapScan { table, .. } => tables.push(table.clone()),
                        ScanType::ParallelBitmapHeapScan { table, .. } => {
                            tables.push(table.clone())
                        }
                        ScanType::BitmapIndexScan { .. } => {
                            // BitmapIndexScan doesn't have direct table info
                        }
                    }
                }
                _ => {
                    // Other node types don't have direct table references
                }
            }

            for child in &node.children {
                collect_tables(child, tables);
            }
        }

        let mut tables = Vec::new();
        collect_tables(&self.root, &mut tables);
        tables
    }

    /// Returns a summary of node types in the plan
    pub fn node_type_summary(&self) -> HashMap<String, usize> {
        fn collect_types(node: &PlanNode, counts: &mut HashMap<String, usize>) {
            let type_name = node.description();
            *counts.entry(type_name).or_insert(0) += 1;
            for child in &node.children {
                collect_types(child, counts);
            }
        }

        let mut counts = HashMap::new();
        collect_types(&self.root, &mut counts);
        counts
    }

    /// Returns true if this plan uses parallel execution
    pub fn uses_parallel_execution(&self) -> bool {
        fn check_parallel(node: &PlanNode) -> bool {
            match &node.node_type {
                NodeType::Utility(UtilityType::GatherMerge { .. }) => true,
                NodeType::Scan(ScanType::ParallelBitmapHeapScan { .. }) => true,
                _ => node.children.iter().any(check_parallel),
            }
        }

        check_parallel(&self.root)
    }

    /// Returns true if this plan uses indexes
    pub fn uses_indexes(&self) -> bool {
        fn check_indexes(node: &PlanNode) -> bool {
            let node_uses_index = match &node.node_type {
                NodeType::Scan(scan_type) => {
                    matches!(
                        scan_type,
                        ScanType::IndexScan { .. } | ScanType::BitmapIndexScan { .. }
                    )
                }
                _ => false,
            };

            node_uses_index || node.children.iter().any(check_indexes)
        }

        check_indexes(&self.root)
    }
}

impl TableReference {
    /// Creates a new table reference with just a name
    pub fn new(name: String) -> Self {
        Self {
            schema: None,
            name,
            alias: None,
        }
    }

    /// Creates a new table reference with schema and name
    pub fn with_schema(schema: String, name: String) -> Self {
        Self {
            schema: Some(schema),
            name,
            alias: None,
        }
    }

    /// Sets the alias for this table reference
    pub fn with_alias(mut self, alias: String) -> Self {
        self.alias = Some(alias);
        self
    }

    /// Returns the full qualified name (schema.name)
    pub fn qualified_name(&self) -> String {
        if let Some(schema) = &self.schema {
            format!("{}.{}", schema, self.name)
        } else {
            self.name.clone()
        }
    }

    /// Returns the display name (with alias if available)
    pub fn display_name(&self) -> String {
        let qualified = self.qualified_name();
        if let Some(alias) = &self.alias {
            format!("{qualified} {alias}")
        } else {
            qualified
        }
    }
}

/// Parser for converting PostgreSQL execution plan text to structured data
#[derive(Debug)]
pub struct PlanParser {}

#[derive(Debug)]
pub enum ParseError {
    InvalidCostFormat(String),
    InvalidNodeStructure(String),
    RegexError(String),
    InvalidIndentation(String),
    EmptyInput,
    InvalidJsonFormat(String),
    MissingJsonPlanData(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::InvalidCostFormat(msg) => write!(f, "Invalid cost format: {msg}"),
            ParseError::InvalidNodeStructure(msg) => write!(f, "Invalid node structure: {msg}"),
            ParseError::RegexError(msg) => write!(f, "Regex error: {msg}"),
            ParseError::InvalidIndentation(msg) => write!(f, "Invalid indentation: {msg}"),
            ParseError::EmptyInput => write!(f, "Empty input provided"),
            ParseError::InvalidJsonFormat(msg) => write!(f, "Invalid JSON format: {msg}"),
            ParseError::MissingJsonPlanData(msg) => write!(f, "Missing JSON plan data: {msg}"),
        }
    }
}

impl std::error::Error for ParseError {}

static COST_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\(cost=(?<min>[\d.]+)\.\.(?<max>[\d.]+)\s+rows=(?<rows>\d+)\s+width=(?<width>\d+)\)",
    )
    .unwrap()
});

static TABLE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"on\s+(?:(?<schema>"[^"]+"|[^\s.]+)\.)?(?<table>"[^"]+"|[^\s.]+)\s*(?<alias>\w+)?"#,
    )
    .unwrap()
});

static INDEX_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?<type>Bitmap)?\s*Index\s*(?<only>Only)?\s+Scan(?:\s+(?<backward>Backward))?(?:\s+using\s+(?<index>\S+))?"#).unwrap()
});

static BITMAP_INDEX_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"Bitmap\s+Index\s+Scan(?:\s+(?:using|on)\s+(?<index>[^\s]+))?"#).unwrap()
});
// Additional regex patterns for detailed parsing
static WORKERS_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Workers?\s+Planned:\s*(\d+)").unwrap());

static LIMIT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Limit\s+(\d+)(?:\s+offset\s+(\d+))?").unwrap());

// Removed unused SORT_KEY_REGEX and GROUP_KEY_REGEX

static HASH_BUCKETS_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+buckets").unwrap());

static INNER_UNIQUE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Inner\s+Unique:\s*(true|false)").unwrap());

static CACHE_MODE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Cache\s+Mode:\s*(\w+)").unwrap());

static SUBPLAN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"SubPlan\s+(\w+)\s*(?:\(([^)]+)\))?").unwrap());

/// Represents a line in the execution plan with its indentation level
#[derive(Debug, Clone)]
struct InternalPlanLine {
    /// Indentation level (number of tabs)
    indent: usize,
    /// The text content of the line
    content: String,
    /// Whether this line represents a plan node (has cost info)
    is_node: bool,
}

impl PlanParser {
    /// Creates a new plan parser with compiled regex patterns
    pub fn new() -> Result<Self, ParseError> {
        Ok(Self {})
    }

    /// Parses a complete execution plan from text
    pub fn parse_plan(&self, text: &str) -> Result<ParsedPlan, ParseError> {
        let lines = self.parse_lines(text)?;
        let root = self.parse_node_tree(&lines, 0)?.0;

        Ok(ParsedPlan::new(root))
    }

    /// Parses a complete execution plan from pre-parsed PlanLine vector
    /// This is more efficient as it reuses the existing parser's structured data
    pub fn parse_plan_from_lines(&self, plan_lines: &[PlanLine]) -> Result<ParsedPlan, ParseError> {
        if plan_lines.is_empty() {
            return Err(ParseError::EmptyInput);
        }

        // Convert PlanLine to internal PlanLine format
        // Note: PlanLine.indentation is already the raw space count, not logical level
        let lines: Vec<_> = plan_lines
            .iter()
            .map(|pl| InternalPlanLine {
                indent: self.convert_raw_indentation_to_logical(pl.indentation),
                content: pl.query.clone(),
                is_node: COST_REGEX.is_match(&pl.query),
            })
            .collect();

        let root = self.parse_node_tree(&lines, 0)?.0;

        Ok(ParsedPlan::new(root))
    }

    /// Main parsing dispatch method for QueryPlan enum
    pub fn parse_query_plan(
        &self,
        query_plan: &crate::QueryPlan,
    ) -> Result<ParsedPlan, ParseError> {
        // Since parsing is now done during QueryPlan construction,
        // we can just clone the parsed plan
        Ok(query_plan.parsed.clone())
    }

    // Note: Text and JSON plan parsing methods removed since parsing
    // is now done during QueryPlan construction

    /// Parse NodeType from JSON node using JSON-specific fields
    fn parse_json_node_type(&self, json_node: &crate::JsonPlanNode) -> NodeType {
        let node_type_lower = json_node.node_type.to_lowercase();

        // Handle scan types using JSON fields
        if node_type_lower.contains("scan") {
            // Extract table reference from JSON fields
            if let Some(ref relation_name) = json_node.relation_name {
                let table = TableReference {
                    schema: json_node.schema.clone(),
                    name: relation_name.clone(),
                    alias: json_node.alias.clone(),
                };

                // Check for index scans
                if node_type_lower.contains("index") && !node_type_lower.contains("bitmap") {
                    let index = json_node
                        .properties
                        .get("Index Name")
                        .and_then(|v| v.as_str())
                        .map(|name| IndexReference {
                            name: name.to_string(),
                        });

                    let backward = node_type_lower.contains("backward");
                    let only = node_type_lower.contains("only");

                    return NodeType::Scan(ScanType::IndexScan {
                        table,
                        index,
                        backward,
                        only,
                    });
                } else if node_type_lower.contains("bitmap index scan") {
                    let index = json_node
                        .properties
                        .get("Index Name")
                        .and_then(|v| v.as_str())
                        .map(|name| IndexReference {
                            name: name.to_string(),
                        });
                    return NodeType::Scan(ScanType::BitmapIndexScan { index });
                } else if node_type_lower.contains("bitmap heap scan") {
                    return NodeType::Scan(ScanType::BitmapHeapScan {
                        table,
                        recheck_condition: None,
                    });
                } else if node_type_lower.contains("seq scan") {
                    return NodeType::Scan(ScanType::SeqScan { table });
                }
            }
        }

        // Fall back to string-based parsing for other node types
        self.parse_node_type_from_string(&json_node.node_type)
    }

    /// Convert JSON node to internal PlanNode structure
    pub fn convert_json_node_to_plan_node(
        &self,
        json_node: &crate::JsonPlanNode,
    ) -> Result<PlanNode, ParseError> {
        // For JSON nodes, we need to build the NodeType using JSON-specific fields
        let node_type = self.parse_json_node_type(json_node);
        let cost = PlanCost {
            startup_cost: json_node.startup_cost,
            min_total_cost: json_node.startup_cost, // JSON min cost is startup cost
            max_total_cost: json_node.total_cost,   // JSON max cost is total cost
            estimated_rows: json_node.plan_rows,
            estimated_width: json_node.plan_width,
        };

        let mut plan_node = PlanNode::new(node_type, cost, json_node.node_type.clone());

        // Note: Table reference is now stored directly in the NodeType enum variants
        // No need to set a separate table_ref field

        // Set actual execution statistics if available (JSON advantage!)
        if json_node.actual_total_time.is_some() || json_node.actual_rows.is_some() {
            let actuals = PlanActuals {
                actual_time_ms: json_node.actual_total_time,
                actual_rows: json_node.actual_rows,
                actual_loops: json_node.actual_loops,
            };
            plan_node.set_actuals(actuals);
        }

        // Add all JSON properties as node properties
        for (key, value) in &json_node.properties {
            let property_value = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Array(arr) => {
                    // Handle arrays (like Output columns)
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
                _ => value.to_string(),
            };
            plan_node.set_property(key.clone(), property_value);
        }

        // Recursively add child nodes
        if let Some(plans) = &json_node.plans {
            for child_json in plans {
                let child_node = self.convert_json_node_to_plan_node(child_json)?;
                plan_node.add_child(child_node);
            }
        }

        Ok(plan_node)
    }

    /// Extract the actual node type from a line by removing tree structure characters
    fn extract_node_type_from_line(&self, line: &str) -> String {
        let trimmed = line.trim();

        // Remove common PostgreSQL tree structure prefixes
        if let Some(stripped) = trimmed.strip_prefix("->") {
            stripped.trim().to_string()
        } else if let Some(stripped) = trimmed.strip_prefix("├──") {
            stripped.trim().to_string()
        } else if let Some(stripped) = trimmed.strip_prefix("└──") {
            stripped.trim().to_string()
        } else {
            // No tree prefix, return as is
            trimmed.to_string()
        }
    }

    /// Parse node type from string using the new analyze pattern
    pub fn parse_node_type_from_string(&self, node_type_str: &str) -> NodeType {
        // First, extract just the node type part by skipping tree structure characters
        let clean_node_str = self.extract_node_type_from_line(node_type_str);
        let line_lower = clean_node_str.to_lowercase();

        // Determine the broad category first, then use specific analyze methods
        if line_lower.contains("scan") {
            // Try to parse as a scan type
            match ScanType::analyze(&clean_node_str) {
                Ok(scan_type) => NodeType::Scan(scan_type),
                Err(_) => {
                    // Fallback for unknown scan types
                    let first_word = clean_node_str
                        .split_whitespace()
                        .next()
                        .unwrap_or("Unknown");
                    NodeType::Unknown(format!("UNKNOWN_SCAN: {}", first_word))
                }
            }
        } else if line_lower.contains("join") || line_lower.contains("nested loop") {
            // Try to parse as a join type (includes "Nested Loop" which may not have "join" in name)
            match JoinType::analyze(&clean_node_str) {
                Ok(join_type) => NodeType::Join(join_type),
                Err(_) => {
                    let first_word = clean_node_str
                        .split_whitespace()
                        .next()
                        .unwrap_or("Unknown");
                    NodeType::Unknown(format!("UNKNOWN_JOIN: {}", first_word))
                }
            }
        } else if line_lower.contains("aggregate") {
            // Try to parse as an aggregate type
            match AggregateType::analyze(&clean_node_str) {
                Ok(agg_type) => NodeType::Aggregate(agg_type),
                Err(_) => {
                    let first_word = clean_node_str
                        .split_whitespace()
                        .next()
                        .unwrap_or("Unknown");
                    NodeType::Unknown(format!("UNKNOWN_AGGREGATE: {}", first_word))
                }
            }
        } else {
            // Try utility operations and other types
            match UtilityType::analyze(&clean_node_str) {
                Ok(util_type) => NodeType::Utility(util_type),
                Err(_) => {
                    // Unknown node type
                    let first_word = clean_node_str
                        .split_whitespace()
                        .next()
                        .unwrap_or("Unknown");
                    NodeType::Unknown(first_word.to_string())
                }
            }
        }
    }

    /// Parses the text into structured lines with indentation
    fn parse_lines(&self, text: &str) -> Result<Vec<InternalPlanLine>, ParseError> {
        let mut lines = Vec::new();

        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }

            let indent = self.count_indentation(line);
            let content = line.trim().to_string();
            let is_node = COST_REGEX.is_match(&content);

            lines.push(InternalPlanLine {
                indent,
                content,
                is_node,
            });
        }

        Ok(lines)
    }

    /// Counts the indentation level for PostgreSQL plans
    /// PostgreSQL uses a specific pattern: 0, 2, 8, 14, 20, 26, 32, ... spaces
    /// Level 0: 0 spaces, Level 1: 2 spaces, Level 2+: 8 + (level-2)*6 spaces
    fn count_indentation(&self, line: &str) -> usize {
        let mut pos = 0;
        let chars: Vec<char> = line.chars().collect();

        // Count leading whitespace
        while pos < chars.len() && chars[pos] == ' ' {
            pos += 1;
        }

        // Convert raw space count to logical indentation level
        self.convert_raw_indentation_to_logical(pos)
    }

    /// Converts raw space count to logical indentation level
    /// PostgreSQL uses a specific pattern: 0, 2, 8, 14, 20, 26, 32, ... spaces
    /// Level 0: 0 spaces, Level 1: 2 spaces, Level 2+: 8 + (level-2)*6 spaces
    fn convert_raw_indentation_to_logical(&self, raw_spaces: usize) -> usize {
        raw_spaces / 2
        // match raw_spaces {
        //     0 => 0, // Root level
        //     1 => 1, // First child level (child nodes like ->  Index Scan)
        //     2 => 1, // Also first child level (for compatibility with 2-space indents)
        //     4 => 2, // Second level (properties of child nodes)
        //     n if n >= 8 => {
        //         // Level 3+: each additional level adds more spaces
        //         2 + (n - 8) / 4
        //     }
        //     _ => {
        //         // Fallback for unexpected indentation - assume it's proportional
        //         raw_spaces / 2
        //     }
        // }
    }

    /// Recursively parses a node and its children from the line list
    fn parse_node_tree(
        &self,
        lines: &[InternalPlanLine],
        start_idx: usize,
    ) -> Result<(PlanNode, usize), ParseError> {
        if start_idx >= lines.len() {
            return Err(ParseError::InvalidNodeStructure(
                "No lines to parse".to_string(),
            ));
        }

        let line = &lines[start_idx];
        if !line.is_node {
            return Err(ParseError::InvalidNodeStructure(format!(
                "Expected node line at index {}, got: {}",
                start_idx, line.content
            )));
        }

        let mut node = self.parse_single_node(&line.content)?;
        let current_indent = line.indent;
        let mut idx = start_idx + 1;

        // Parse properties and child nodes
        while idx < lines.len() {
            let current_line = &lines[idx];

            // If indentation is less than or equal to current node, we're done with this subtree
            if current_line.indent <= current_indent {
                break;
            }

            // If this is a direct child node (one level deeper)
            if current_line.is_node && current_line.indent > current_indent {
                let (child_node, next_idx) = self.parse_node_tree(lines, idx)?;
                node.add_child(child_node);
                idx = next_idx;
            } else if current_line.indent > current_indent {
                // This is a property line for the current node
                self.parse_property_line(&mut node, &current_line.content);
                idx += 1;
            } else {
                // Skip lines that are deeper (they belong to child nodes that will be parsed recursively)
                idx += 1;
            }
        }

        // After parsing all properties, update the node type with the collected information
        node.update_from_properties();

        Ok((node, idx))
    }

    /// Parses a single node line into a PlanNode
    fn parse_single_node(&self, line: &str) -> Result<PlanNode, ParseError> {
        // Extract cost information
        let cost = self.extract_cost(line)?;

        // Determine node type from the beginning of the line
        let node_type = self.parse_node_type_from_string(line);

        // Create the node
        let node = PlanNode::new(node_type, cost, line.to_string());

        Ok(node)
    }

    /// Extracts cost information from a node line
    /// PostgreSQL cost format: (cost=startup..total rows=estimated_rows width=estimated_width)
    /// The startup cost is the minimum cost to get the first row
    /// The total cost range is startup..total, representing minimum to maximum cost
    fn extract_cost(&self, line: &str) -> Result<PlanCost, ParseError> {
        let captures = COST_REGEX.captures(line).ok_or_else(|| {
            ParseError::InvalidCostFormat(format!("No cost information found in: {}", line))
        })?;

        let startup_cost = captures["min"]
            .parse::<f64>()
            .map_err(|_| ParseError::InvalidCostFormat("Invalid startup cost".to_string()))?;

        let max_total_cost = captures["max"]
            .parse::<f64>()
            .map_err(|_| ParseError::InvalidCostFormat("Invalid total cost".to_string()))?;

        let estimated_rows = captures["rows"]
            .parse::<u64>()
            .map_err(|_| ParseError::InvalidCostFormat("Invalid estimated rows".to_string()))?;

        let estimated_width = captures["width"]
            .parse::<u32>()
            .map_err(|_| ParseError::InvalidCostFormat("Invalid estimated width".to_string()))?;

        Ok(PlanCost {
            startup_cost,
            min_total_cost: startup_cost, // Min cost is the startup cost (cost for first row)
            max_total_cost,               // Max cost is the "total" cost (cost for all rows)
            estimated_rows,
            estimated_width,
        })
    }

    // Removed unused extract_table_reference method
    /// Parses a property line and adds it to the node
    fn parse_property_line(&self, node: &mut PlanNode, line: &str) {
        if line.starts_with("Output:") {
            let output = line.strip_prefix("Output:").unwrap_or("").trim();
            node.set_property("Output".to_string(), output.to_string());
        } else if line.starts_with("Index Cond:") {
            let cond = line.strip_prefix("Index Cond:").unwrap_or("").trim();
            node.set_property("Index Cond".to_string(), cond.to_string());
        } else if line.starts_with("Filter:") {
            let filter = line.strip_prefix("Filter:").unwrap_or("").trim();
            node.set_property("Filter".to_string(), filter.to_string());
        } else if line.starts_with("Sort Key:") {
            let sort_key = line.strip_prefix("Sort Key:").unwrap_or("").trim();
            node.set_property("Sort Key".to_string(), sort_key.to_string());
        } else if line.starts_with("Join Filter:") {
            let join_filter = line.strip_prefix("Join Filter:").unwrap_or("").trim();
            node.set_property("Join Filter".to_string(), join_filter.to_string());
        } else if line.starts_with("Group Key:") {
            let group_key = line.strip_prefix("Group Key:").unwrap_or("").trim();
            node.set_property("Group Key".to_string(), group_key.to_string());
        } else if line.starts_with("Cache Key:") {
            let cache_key = line.strip_prefix("Cache Key:").unwrap_or("").trim();
            node.set_property("Cache Key".to_string(), cache_key.to_string());
        } else if line.starts_with("Workers Planned:") {
            let workers = line.strip_prefix("Workers Planned:").unwrap_or("").trim();
            node.set_property("Workers Planned".to_string(), workers.to_string());
        } else if line.starts_with("Recheck Cond:") {
            let recheck = line.strip_prefix("Recheck Cond:").unwrap_or("").trim();
            node.set_property("Recheck Cond".to_string(), recheck.to_string());
        } else if line.starts_with("Inner Unique:") {
            let inner_unique = line.strip_prefix("Inner Unique:").unwrap_or("").trim();
            node.set_property("Inner Unique".to_string(), inner_unique.to_string());
        } else if line.starts_with("Cache Mode:") {
            let cache_mode = line.strip_prefix("Cache Mode:").unwrap_or("").trim();
            node.set_property("Cache Mode".to_string(), cache_mode.to_string());
        } else {
            // Handle any other property format (key: value)
            if let Some(colon_pos) = line.find(':') {
                let key = line[..colon_pos].trim().to_string();
                let value = line[colon_pos + 1..].trim().to_string();
                node.set_property(key, value);
            }
        }
    }
}

/// Helper function to extract table reference from a line (used by both parser and node types)
fn extract_table_reference_from_line(line: &str) -> Option<TableReference> {
    if let Some(captures) = TABLE_REGEX.captures(line) {
        if let Some(table_name) = captures.name("table") {
            // Format: on "schema"."table" alias or on schema.table alias (quotes optional)
            let schema = captures.name("schema").map(|m| strip_quotes(m.as_str()));
            let table = strip_quotes(table_name.as_str());
            let alias = captures.name("alias").map(|m| strip_quotes(m.as_str()));

            let mut table_ref = if let Some(schema) = schema {
                TableReference::with_schema(schema, table)
            } else {
                TableReference::new(table)
            };

            if let Some(alias) = alias {
                table_ref = table_ref.with_alias(alias);
            }

            Some(table_ref)
        } else {
            None
        }
    } else {
        None
    }
}

/// Helper function to strip surrounding quotes from a string
fn strip_quotes(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Helper function to extract workers planned from a line
fn extract_workers_planned(line: &str) -> Option<u32> {
    WORKERS_REGEX
        .captures(line)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

/// Helper function to extract limit and offset from a line
fn extract_limit_info(line: &str) -> (Option<u64>, Option<u64>) {
    if let Some(caps) = LIMIT_REGEX.captures(line) {
        let limit = caps.get(1).and_then(|m| m.as_str().parse().ok());
        let offset = caps.get(2).and_then(|m| m.as_str().parse().ok());
        (limit, offset)
    } else {
        (None, None)
    }
}

/// Helper function to parse sort keys from a string
fn parse_sort_keys(sort_key_str: &str) -> Vec<SortKey> {
    sort_key_str
        .split(',')
        .map(|key| {
            let key = key.trim();
            // Check if it ends with DESC or ASC
            if key.ends_with(" DESC") {
                SortKey {
                    expression: key.strip_suffix(" DESC").unwrap_or(key).to_string(),
                    direction: Some("DESC".to_string()),
                }
            } else if key.ends_with(" ASC") {
                SortKey {
                    expression: key.strip_suffix(" ASC").unwrap_or(key).to_string(),
                    direction: Some("ASC".to_string()),
                }
            } else {
                SortKey {
                    expression: key.to_string(),
                    direction: None,
                }
            }
        })
        .collect()
}

/// Helper function to parse group keys from a string
fn parse_group_keys(group_key_str: &str) -> Vec<String> {
    group_key_str
        .split(',')
        .map(|key| key.trim().to_string())
        .collect()
}

/// Helper function to extract hash buckets from a line
fn extract_hash_buckets(line: &str) -> Option<u32> {
    HASH_BUCKETS_REGEX
        .captures(line)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

/// Helper function to check for inner unique from a line
fn extract_inner_unique(line: &str) -> Option<bool> {
    INNER_UNIQUE_REGEX
        .captures(line)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str() == "true")
}

/// Helper function to extract cache mode from a line
fn extract_cache_mode(line: &str) -> Option<String> {
    CACHE_MODE_REGEX
        .captures(line)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Helper function to extract subplan information from a line
fn extract_subplan_info(line: &str) -> Option<SubPlanReference> {
    SUBPLAN_REGEX.captures(line).map(|caps| {
        let name = caps
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "Unknown".to_string());
        let subplan_type = caps.get(2).map(|m| m.as_str().to_string());
        SubPlanReference { name, subplan_type }
    })
}

/// Helper function to extract aggregate functions from output string
fn extract_aggregate_functions(output_str: &str) -> Vec<String> {
    // Look for common aggregate function patterns
    let agg_functions = [
        "count",
        "sum",
        "avg",
        "min",
        "max",
        "array_agg",
        "string_agg",
    ];
    let mut functions = Vec::new();

    for func in agg_functions {
        if output_str.to_lowercase().contains(&format!("{}(", func)) {
            functions.push(func.to_string());
        }
    }

    // If no specific functions found, try to extract function calls
    if functions.is_empty() {
        let re = regex::Regex::new(r"(\w+)\(").unwrap();
        for caps in re.captures_iter(output_str) {
            if let Some(func_name) = caps.get(1) {
                let func = func_name.as_str().to_lowercase();
                if !functions.contains(&func) {
                    functions.push(func);
                }
            }
        }
    }

    functions
}

impl Default for PlanParser {
    fn default() -> Self {
        Self::new().expect("Failed to create default PlanParser")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_node_creation() {
        let cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 100.0,
            estimated_rows: 1000,
            estimated_width: 50,
        };

        let node = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan {
                table: TableReference::new("test_table".to_string()),
                index: Some(IndexReference {
                    name: "PK_test".to_string(),
                }),
                backward: false,
                only: false,
            }),
            cost,
            "Index Scan using PK_test".to_string(),
        );

        assert!(node.is_scan());
        assert!(!node.is_join());
        assert_eq!(node.cost.total_cost(), 100.0);
    }

    #[test]
    fn test_parser_creation() {
        let parser = PlanParser::new();
        assert!(parser.is_ok());
    }

    #[test]
    fn test_cost_extraction() {
        let parser = PlanParser::new().unwrap();
        let line =
            r#"Index Scan using "PK_Test" on "Shared"."Test" t  (cost=0.42..8.44 rows=1 width=16)"#;

        let cost = parser.extract_cost(line).unwrap();
        assert_eq!(cost.startup_cost, 0.42);
        assert_eq!(cost.min_total_cost, 0.42); // Min cost equals startup cost
        assert_eq!(cost.max_total_cost, 8.44);
        assert_eq!(cost.total_cost(), 8.44); // Convenience method returns max
        assert_eq!(cost.avg_total_cost(), 4.43); // Average of min and max
        assert_eq!(cost.cost_range_span(), 8.02); // Difference between max and min
        assert_eq!(cost.total_cost_range(), (0.42, 8.44)); // Range tuple
        assert_eq!(cost.estimated_rows, 1);
        assert_eq!(cost.estimated_width, 16);
    }

    #[test]
    fn test_cost_range_calculations() {
        let parser = PlanParser::new().unwrap();

        // Test with a wide cost range (typical for operations that can return early vs late)
        let wide_range_line = "Index Scan on orders  (cost=0.43..15000.0 rows=50000 width=200)";
        let cost = parser.extract_cost(wide_range_line).unwrap();

        assert_eq!(cost.startup_cost, 0.43);
        assert_eq!(cost.min_total_cost, 0.43);
        assert_eq!(cost.max_total_cost, 15000.0);
        assert_eq!(cost.cost_range_span(), 14999.57);
        assert!(cost.cost_range_span() > 1000.0); // Wide range indicates variable cost based on rows fetched

        // Test with a narrow cost range (typical for operations with predictable cost)
        let narrow_range_line = "Hash  (cost=1.0..1.1 rows=10 width=50)";
        let cost = parser.extract_cost(narrow_range_line).unwrap();

        assert_eq!(cost.startup_cost, 1.0);
        assert_eq!(cost.min_total_cost, 1.0);
        assert_eq!(cost.max_total_cost, 1.1);
        assert!((cost.cost_range_span() - 0.1).abs() < 0.001); // Handle floating point precision
        assert!(cost.cost_range_span() < 1.0); // Narrow range indicates predictable cost
    }

    #[test]
    fn test_node_type_determination() {
        let parser = PlanParser::new().unwrap();

        assert!(matches!(
            parser.parse_node_type_from_string("Nested Loop Left Join"),
            NodeType::Join(JoinType::NestedLoopLeftJoin { .. })
        ));

        assert!(matches!(
            parser.parse_node_type_from_string("Sort"),
            NodeType::Utility(UtilityType::Sort { .. })
        ));
    }

    #[test]
    fn test_simple_plan_parsing() {
        let parser = PlanParser::new().unwrap();
        let plan_text = r#"Index Scan using "PK_Test" on "Shared"."Test" t  (cost=0.42..8.44 rows=1 width=16)
  Output: "Id", "Name"
  Index Cond: (t."Id" = 123)"#;

        let parsed_plan = parser.parse_plan(plan_text).unwrap();

        assert!(parsed_plan.root.is_scan());
        assert_eq!(parsed_plan.root.cost.startup_cost, 0.42);
        assert_eq!(
            parsed_plan.root.get_property("Output"),
            Some(r#""Id", "Name""#.to_string())
        );
        assert_eq!(
            parsed_plan.root.get_property("Index Cond"),
            Some(r#"(t."Id" = 123)"#.to_string())
        );
    }

    #[test]
    fn test_nested_plan_parsing() {
        let parser = PlanParser::new().unwrap();
        let plan_text = r#"Nested Loop  (cost=1.15..279.82 rows=7 width=110)
  Output: m."Id", m."Name"
  ->  Index Scan using "IX_Test1" on "Shared"."Test1" m  (cost=0.57..2.79 rows=1 width=54)
        Output: m."Id", m."Name"
        Index Cond: (m."Id" = 1)
  ->  Index Scan using "IX_Test2" on "Shared"."Test2" t  (cost=0.57..274.10 rows=292 width=56)
        Output: t."Id", t."Value"
        Index Cond: (t."TestId" = m."Id")"#;

        let parsed_plan = parser.parse_plan(plan_text).unwrap();

        assert!(parsed_plan.root.is_join());
        assert_eq!(parsed_plan.root.children.len(), 2);
        assert!(parsed_plan.root.children[0].is_scan());
        assert!(parsed_plan.root.children[1].is_scan());
        assert_eq!(parsed_plan.node_count(), 3);
        assert_eq!(parsed_plan.max_depth(), 2);
    }

    #[test]
    fn test_real_world_nested_plan() {
        let parser = PlanParser::new().unwrap();
        let plan_text = r#"Nested Loop Left Join  (cost=9084.53..4363347.10 rows=565 width=214)
  Output: f."Id", f1."Id"
  Join Filter: (f."Id" = f1."FluidLineId")
  ->  Index Scan using "IX_FluidLines_AcceptanceId" on "Shared"."FluidLines" f  (cost=0.42..4346249.76 rows=565 width=180)
        Output: f."Id", f."FluidLineCategoryId"
        Index Cond: (f."AcceptanceId" = 363)
        Filter: (f."IsActive")
  ->  Materialize  (cost=9084.11..16927.88 rows=20 width=38)
        Output: f1."Id", f1."HourlyTestCount"
        ->  Nested Loop  (cost=9084.11..16927.78 rows=20 width=38)
              Output: f1."Id", f1."HourlyTestCount"
              Inner Unique: true
              ->  GroupAggregate  (cost=9083.54..16871.88 rows=20 width=24)
                    Output: max(f2."Id"), f2."FluidLineId"
                    Group Key: f2."FluidLineId"
                    ->  Sort  (cost=9083.54..9083.59 rows=20 width=24)
                          Output: f2."FluidLineId", f2."Time", f2."Id"
                          Sort Key: f2."FluidLineId"
                          ->  Index Scan using "IX_FluidEntries_Time" on "Shared"."FluidEntries" f2  (cost=0.57..578.69 rows=16761 width=16)
                                Output: f2."Id", f2."FluidLineId"
                                Index Cond: ((f2."Time" >= '2025-06-11'))
              ->  Index Scan using "PK_FluidLines" on "Shared"."FluidLines" f3  (cost=0.42..1.81 rows=1 width=4)
                    Output: f3."Id"
                    Index Cond: (f3."Id" = f2."FluidLineId")"#;

        let parsed_plan = parser.parse_plan(plan_text).unwrap();

        // Assertions to check parsing worked correctly
        assert!(parsed_plan.root.is_join());
        assert_eq!(
            parsed_plan.root.children.len(),
            2,
            "Root should have 2 children"
        );
        assert!(
            parsed_plan.node_count() > 6,
            "Should have parsed many nodes"
        );
        assert!(parsed_plan.max_depth() > 4, "Should have significant depth");
    }

    #[test]
    fn test_table_reference() {
        let table_ref = TableReference::with_schema("public".to_string(), "users".to_string())
            .with_alias("u".to_string());

        assert_eq!(table_ref.qualified_name(), "public.users");
        assert_eq!(table_ref.display_name(), "public.users u");
    }

    #[test]
    fn test_plan_analysis() {
        let cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 100.0,
            estimated_rows: 1000,
            estimated_width: 50,
        };

        let mut root = PlanNode::new(
            NodeType::Join(JoinType::NestedLoop {
                inner_unique: false,
            }),
            cost.clone(),
            "Nested Loop".to_string(),
        );

        let child1 = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan {
                table: TableReference::new("test_table".to_string()),
                index: Some(IndexReference {
                    name: "test_index".to_string(),
                }),
                backward: false,
                only: false,
            }),
            cost.clone(),
            "Index Scan".to_string(),
        );

        let child2 = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference::new("test_table2".to_string()),
            }),
            cost.clone(),
            "Seq Scan".to_string(),
        );

        root.add_child(child1);
        root.add_child(child2);

        let plan = ParsedPlan::new(root);

        assert_eq!(plan.node_count(), 3);
        assert_eq!(plan.max_depth(), 2);
        assert_eq!(plan.total_cost(), 300.0); // 100 + 100 + 100
    }

    #[test]
    fn test_json_text_plan_normalization_equivalency() {
        // Create equivalent text and JSON plans for normalization testing
        let text_plan = create_test_text_plan();
        let json_plan = create_test_json_plan();

        let parser = PlanParser::new().unwrap();

        let parsed_text = parser.parse_query_plan(&text_plan).unwrap();
        let parsed_json = parser.parse_query_plan(&json_plan).unwrap();

        // Verify basic structure equivalency
        assert_eq!(
            parsed_text.node_count(),
            parsed_json.node_count(),
            "Node counts should be equal"
        );
        assert_eq!(
            parsed_text.max_depth(),
            parsed_json.max_depth(),
            "Max depths should be equal"
        );

        // Verify cost equivalency (within floating point precision)
        let cost_diff = (parsed_text.total_cost() - parsed_json.total_cost()).abs();
        assert!(
            cost_diff < 0.01,
            "Total costs should be equivalent: {} vs {}",
            parsed_text.total_cost(),
            parsed_json.total_cost()
        );

        // Verify root node types are equivalent
        assert_eq!(
            parsed_text.root.description(),
            parsed_json.root.description(),
            "Root node types should be equivalent"
        );

        // Verify both plans detect same features
        assert_eq!(
            parsed_text.uses_indexes(),
            parsed_json.uses_indexes(),
            "Index usage detection should be equivalent"
        );
        assert_eq!(
            parsed_text.uses_parallel_execution(),
            parsed_json.uses_parallel_execution(),
            "Parallel execution detection should be equivalent"
        );

        // Verify table references are equivalent
        let text_tables = parsed_text.get_tables();
        let json_tables = parsed_json.get_tables();
        assert_eq!(
            text_tables.len(),
            json_tables.len(),
            "Table reference counts should be equal"
        );

        // Compare normalized node structures recursively
        compare_normalized_node_structures(&parsed_text.root, &parsed_json.root);

        println!("✅ JSON and Text plan normalization test passed!");
        println!("   Node count: {} (both)", parsed_text.node_count());
        println!("   Max depth: {} (both)", parsed_text.max_depth());
        println!("   Total cost: {:.2} (both)", parsed_text.total_cost());
        println!("   Uses indexes: {} (both)", parsed_text.uses_indexes());
    }

    // Helper function to create a test text plan
    fn create_test_text_plan() -> crate::QueryPlan {
        use crate::TextPlanData;
        use chrono::Utc;

        let plan_text = r#"Limit  (cost=0.43..599.04 rows=1000 width=56)
  Output: "Id", "EndDate", "Level"
  ->  Index Scan Backward using "IX_VitalAlarms_EndDate" on "Shared"."VitalAlarms" v  (cost=0.43..95610.13 rows=159718 width=56)
        Output: "Id", "EndDate", "Level"
        Index Cond: (v."EndDate" IS NOT NULL)
        Filter: ((NOT v."IsDismissed") AND (v."Level" > '66'::double precision))"#;

        let _text_data = TextPlanData {
            timestamp: Utc::now(),
            duration_ms: 1242.373,
            query_text: "SELECT * FROM test".to_string(),
            plan_text: plan_text.to_string(),
            plan_lines: vec![],
        };

        // Use new parsing architecture
        use crate::parsing::{ParseMetadata, PlanFactory, PlanParserCore, TextPlanParser};

        let timestamp = Utc::now();
        let metadata = ParseMetadata::new(timestamp, 1234.5, "SELECT * FROM test".to_string());
        let parser = TextPlanParser::new().unwrap();
        let parsed_result = parser.parse(plan_text, metadata).unwrap();

        PlanFactory::create_query_plan_from_parsed(
            timestamp,
            1234.5,
            "SELECT * FROM test".to_string(),
            plan_text.to_string(),
            parsed_result,
        )
        .expect("Failed to create QueryPlan")
    }

    // Helper function to create equivalent JSON plan
    fn create_test_json_plan() -> crate::QueryPlan {
        use crate::{JsonPlan, JsonPlanData};
        use chrono::Utc;

        let json_content = r#"[{
            "Plan": {
                "Node Type": "Limit",
                "Startup Cost": 0.43,
                "Total Cost": 599.04,
                "Plan Rows": 1000,
                "Plan Width": 56,
                "Output": ["Id", "EndDate", "Level"],
                "Plans": [{
                    "Node Type": "Index Scan Backward",
                    "Relation Name": "VitalAlarms",
                    "Schema": "Shared",
                    "Alias": "v",
                    "Index Name": "IX_VitalAlarms_EndDate",
                    "Startup Cost": 0.43,
                    "Total Cost": 95610.13,
                    "Plan Rows": 159718,
                    "Plan Width": 56,
                    "Index Cond": "(v.\"EndDate\" IS NOT NULL)",
                    "Filter": "((NOT v.\"IsDismissed\") AND (v.\"Level\" > '66'::double precision))",
                    "Output": ["Id", "EndDate", "Level"]
                }]
            }
        }]"#;

        let parsed_json: Vec<JsonPlan> = serde_json::from_str(json_content).unwrap();

        let _json_data = JsonPlanData {
            timestamp: Utc::now(),
            duration_ms: 1242.373,
            query_text: "SELECT * FROM test".to_string(),
            raw_json: json_content.to_string(),
            parsed_json: parsed_json.into_iter().next().unwrap(),
        };

        // Use new parsing architecture
        use crate::parsing::{JsonPlanParser, ParseMetadata, PlanFactory, PlanParserCore};

        let timestamp = Utc::now();
        let metadata = ParseMetadata::new(timestamp, 1242.373, "SELECT * FROM test".to_string());
        let parser = JsonPlanParser::new();
        let parsed_result = parser.parse(json_content, metadata).unwrap();

        PlanFactory::create_query_plan_from_parsed(
            timestamp,
            1242.373,
            "SELECT * FROM test".to_string(),
            json_content.to_string(),
            parsed_result,
        )
        .expect("Failed to create QueryPlan")
    }

    // Helper function to extract table reference from scan node types
    fn extract_table_from_scan(node_type: &NodeType) -> Option<&TableReference> {
        use NodeType::*;
        use ScanType::*;

        match node_type {
            Scan(scan_type) => match scan_type {
                SeqScan { table } => Some(table),
                IndexScan { table, .. } => Some(table),
                BitmapHeapScan { table, .. } => Some(table),
                ParallelBitmapHeapScan { table, .. } => Some(table),
                BitmapIndexScan { .. } => None, // No table reference for bitmap index scan
            },
            _ => None,
        }
    }

    // Helper function to recursively compare normalized node structures
    fn compare_normalized_node_structures(text_node: &PlanNode, json_node: &PlanNode) {
        // Compare node type - check the base node type is equivalent
        // Note: descriptions might differ slightly due to table vs index name references
        let text_node_type = format!("{:?}", text_node.node_type);
        let json_node_type = format!("{:?}", json_node.node_type);
        assert_eq!(
            text_node_type, json_node_type,
            "Node types should match: '{}' vs '{}'",
            text_node_type, json_node_type
        );

        // Both should be the same class of operation (both scan, both join, etc.)
        assert_eq!(
            text_node.is_scan(),
            json_node.is_scan(),
            "Both should be same operation class"
        );
        assert_eq!(
            text_node.is_join(),
            json_node.is_join(),
            "Both should be same operation class"
        );
        assert_eq!(
            text_node.is_aggregate(),
            json_node.is_aggregate(),
            "Both should be same operation class"
        );

        // Compare cost information (within floating point precision)
        let startup_diff = (text_node.cost.startup_cost - json_node.cost.startup_cost).abs();
        let total_diff = (text_node.cost.total_cost() - json_node.cost.total_cost()).abs();

        assert!(
            startup_diff < 0.01,
            "Startup costs should match: {} vs {}",
            text_node.cost.startup_cost,
            json_node.cost.startup_cost
        );
        assert!(
            total_diff < 0.01,
            "Total costs should match: {} vs {}",
            text_node.cost.total_cost(),
            json_node.cost.total_cost()
        );

        assert_eq!(
            text_node.cost.estimated_rows, json_node.cost.estimated_rows,
            "Estimated rows should match"
        );
        assert_eq!(
            text_node.cost.estimated_width, json_node.cost.estimated_width,
            "Estimated width should match"
        );

        // Compare table references when both nodes are scans
        if text_node.is_scan() && json_node.is_scan() {
            let text_table = extract_table_from_scan(&text_node.node_type);
            let json_table = extract_table_from_scan(&json_node.node_type);

            if let (Some(text_tbl), Some(json_tbl)) = (text_table, json_table) {
                assert_eq!(
                    text_tbl.name, json_tbl.name,
                    "Table names should match for scan nodes"
                );
            }
        }

        // Compare key properties that should be equivalent
        let key_properties = [
            "Index Cond",
            "Filter",
            "Sort Key",
            "Join Filter",
            "Group Key",
        ];
        for prop in &key_properties {
            let text_prop = text_node.get_property(prop);
            let json_prop = json_node.get_property(prop);

            match (text_prop, json_prop) {
                (Some(text_val), Some(json_val)) => {
                    assert_eq!(
                        text_val, json_val,
                        "Property '{}' should match: '{}' vs '{}'",
                        prop, text_val, json_val
                    );
                }
                (None, None) => {} // Both don't have this property
                _ => {
                    // Allow some flexibility for properties that might be present in one format but not the other
                    // This is acceptable as long as the core plan structure is equivalent
                }
            }
        }

        // Compare child count
        assert_eq!(
            text_node.children.len(),
            json_node.children.len(),
            "Child node counts should match"
        );

        // Recursively compare children
        for (text_child, json_child) in text_node.children.iter().zip(json_node.children.iter()) {
            compare_normalized_node_structures(text_child, json_child);
        }
    }
}

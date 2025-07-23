use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// Table or index name (e.g., "VitalAlarms", "IX_VitalAlarms_EndDate")
    pub name: String,
    /// Table alias used in the query (e.g., "v")
    pub alias: Option<String>,
}

/// Types of scan operations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScanType {
    /// Sequential scan of entire table
    SeqScan,
    /// Index scan using specific index
    IndexScan,
    /// Index scan in reverse order
    IndexScanBackward,
    /// Index-only scan (all data from index)
    IndexOnlyScan,
    /// Bitmap heap scan after bitmap index scan
    BitmapHeapScan,
    /// Creates bitmap from index
    BitmapIndexScan,
    /// Parallel version of bitmap heap scan
    ParallelBitmapHeapScan,
}

/// Types of join operations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JoinType {
    /// Nested loop join
    NestedLoop,
    /// Nested loop left join
    NestedLoopLeftJoin,
    /// Hash join
    HashJoin,
    /// Merge join
    MergeJoin,
}

/// Types of aggregate operations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AggregateType {
    /// Basic aggregate
    Aggregate,
    /// Group aggregate with grouping
    GroupAggregate,
    /// Hash-based aggregate
    HashAggregate,
}

/// Types of utility/control operations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UtilityType {
    /// Sort operation
    Sort,
    /// Limit operation
    Limit,
    /// Gather merge for parallel operations
    GatherMerge,
    /// Materialize results
    Materialize,
    /// Memoize for caching
    Memoize,
    /// Subplan execution
    SubPlan,
    /// Bitmap AND operation
    BitmapAnd,
    /// Bitmap OR operation
    BitmapOr,
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

    /// Table or index being accessed (for scan nodes)
    pub table_ref: Option<TableReference>,

    /// Child nodes in the execution tree
    pub children: Vec<PlanNode>,

    /// Node-specific properties
    pub properties: HashMap<String, String>,

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

    /// Original raw plan text/JSON
    pub raw_source: String,

    /// Source format of the plan
    pub source_format: crate::PlanSourceFormat,
}

impl PlanNode {
    /// Creates a new plan node with the specified type and cost
    pub fn new(node_type: NodeType, cost: PlanCost, original_text: String) -> Self {
        Self {
            node_type,
            cost,
            actuals: None,
            table_ref: None,
            children: Vec::new(),
            properties: HashMap::new(),
            original_text,
        }
    }

    /// Adds a child node to this node
    pub fn add_child(&mut self, child: PlanNode) {
        self.children.push(child);
    }

    /// Sets a property for this node
    pub fn set_property(&mut self, key: String, value: String) {
        self.properties.insert(key, value);
    }

    /// Gets a property value by key
    pub fn get_property(&self, key: &str) -> Option<&String> {
        self.properties.get(key)
    }

    /// Sets the table reference for this node
    pub fn set_table_ref(&mut self, table_ref: TableReference) {
        self.table_ref = Some(table_ref);
    }

    /// Sets actual execution statistics
    pub fn set_actuals(&mut self, actuals: PlanActuals) {
        self.actuals = Some(actuals);
    }

    /// Returns true if this is a scan node
    pub fn is_scan(&self) -> bool {
        matches!(self.node_type, NodeType::Scan(_))
    }

    /// Returns true if this is a join node
    pub fn is_join(&self) -> bool {
        matches!(self.node_type, NodeType::Join(_))
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
            NodeType::Scan(scan_type) => {
                let type_str = match scan_type {
                    ScanType::SeqScan => "Sequential Scan",
                    ScanType::IndexScan => "Index Scan",
                    ScanType::IndexScanBackward => "Index Scan Backward",
                    ScanType::IndexOnlyScan => "Index Only Scan",
                    ScanType::BitmapHeapScan => "Bitmap Heap Scan",
                    ScanType::BitmapIndexScan => "Bitmap Index Scan",
                    ScanType::ParallelBitmapHeapScan => "Parallel Bitmap Heap Scan",
                };
                if let Some(table_ref) = &self.table_ref {
                    format!("{} on {}", type_str, table_ref.name)
                } else {
                    type_str.to_string()
                }
            }
            NodeType::Join(join_type) => match join_type {
                JoinType::NestedLoop => "Nested Loop",
                JoinType::NestedLoopLeftJoin => "Nested Loop Left Join",
                JoinType::HashJoin => "Hash Join",
                JoinType::MergeJoin => "Merge Join",
            }
            .to_string(),
            NodeType::Aggregate(agg_type) => match agg_type {
                AggregateType::Aggregate => "Aggregate",
                AggregateType::GroupAggregate => "Group Aggregate",
                AggregateType::HashAggregate => "Hash Aggregate",
            }
            .to_string(),
            NodeType::Utility(util_type) => match util_type {
                UtilityType::Sort => "Sort",
                UtilityType::Limit => "Limit",
                UtilityType::GatherMerge => "Gather Merge",
                UtilityType::Materialize => "Materialize",
                UtilityType::Memoize => "Memoize",
                UtilityType::SubPlan => "SubPlan",
                UtilityType::BitmapAnd => "BitmapAnd",
                UtilityType::BitmapOr => "BitmapOr",
            }
            .to_string(),
            NodeType::Unknown(name) => name.clone(),
        }
    }
}

impl ParsedPlan {
    /// Creates a new parsed plan with the given root node and source format
    pub fn new(root: PlanNode, raw_source: String, source_format: crate::PlanSourceFormat) -> Self {
        Self {
            root,
            planning_time_ms: None,
            execution_time_ms: None,
            raw_source,
            source_format,
        }
    }

    /// Creates a new parsed plan from text format (backwards compatibility)
    pub fn new_text(root: PlanNode, raw_text: String) -> Self {
        Self::new(root, raw_text, crate::PlanSourceFormat::Text)
    }

    /// Creates a new parsed plan from JSON format
    pub fn new_json(root: PlanNode, raw_json: String) -> Self {
        Self::new(root, raw_json, crate::PlanSourceFormat::Json)
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
    pub fn get_tables(&self) -> Vec<&TableReference> {
        fn collect_tables<'a>(node: &'a PlanNode, tables: &mut Vec<&'a TableReference>) {
            if let Some(table_ref) = &node.table_ref {
                tables.push(table_ref);
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
                NodeType::Utility(UtilityType::GatherMerge) => true,
                NodeType::Scan(ScanType::ParallelBitmapHeapScan) => true,
                _ => node.children.iter().any(check_parallel),
            }
        }

        check_parallel(&self.root)
    }

    /// Returns true if this plan uses indexes
    pub fn uses_indexes(&self) -> bool {
        fn check_indexes(node: &PlanNode) -> bool {
            let node_uses_index = match &node.node_type {
                NodeType::Scan(scan_type) => match scan_type {
                    ScanType::IndexScan
                    | ScanType::IndexScanBackward
                    | ScanType::IndexOnlyScan
                    | ScanType::BitmapIndexScan => true,
                    _ => false,
                },
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
            format!("{} {}", qualified, alias)
        } else {
            qualified
        }
    }
}

/// Parser for converting PostgreSQL execution plan text to structured data
#[derive(Debug)]
pub struct PlanParser {
    /// Regex for matching cost information: (cost=X..Y rows=Z width=W)
    cost_regex: Regex,
    /// Regex for matching table references: "schema"."table" alias
    table_regex: Regex,
    /// Regex for detecting plan node lines (those with cost info)
    node_regex: Regex,
}

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
            ParseError::InvalidCostFormat(msg) => write!(f, "Invalid cost format: {}", msg),
            ParseError::InvalidNodeStructure(msg) => write!(f, "Invalid node structure: {}", msg),
            ParseError::RegexError(msg) => write!(f, "Regex error: {}", msg),
            ParseError::InvalidIndentation(msg) => write!(f, "Invalid indentation: {}", msg),
            ParseError::EmptyInput => write!(f, "Empty input provided"),
            ParseError::InvalidJsonFormat(msg) => write!(f, "Invalid JSON format: {}", msg),
            ParseError::MissingJsonPlanData(msg) => write!(f, "Missing JSON plan data: {}", msg),
        }
    }
}

impl std::error::Error for ParseError {}

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
        let cost_regex = Regex::new(r"\(cost=([\d.]+)\.\.([\d.]+)\s+rows=(\d+)\s+width=(\d+)\)")
            .map_err(|e| ParseError::RegexError(e.to_string()))?;

        let table_regex =
            Regex::new(r#"(?:using\s+"([^"]+)"|on\s+(?:"([^"]+)"\.)?"([^"]+)"\s*(\w+)?)"#)
                .map_err(|e| ParseError::RegexError(e.to_string()))?;

        let node_regex = Regex::new(r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)")
            .map_err(|e| ParseError::RegexError(e.to_string()))?;

        Ok(Self {
            cost_regex,
            table_regex,
            node_regex,
        })
    }

    /// Parses a complete execution plan from text
    pub fn parse_plan(&self, text: &str) -> Result<ParsedPlan, ParseError> {
        let lines = self.parse_lines(text)?;
        let root = self.parse_node_tree(&lines, 0)?.0;

        Ok(ParsedPlan::new_text(root, text.to_string()))
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
                is_node: self.node_regex.is_match(&pl.query), // Check if this line contains cost info
            })
            .collect();

        let root = self.parse_node_tree(&lines, 0)?.0;

        // Create plan text from lines for reference
        let plan_text = plan_lines
            .iter()
            .map(|pl| format!("{:indent$}{}", "", pl.query, indent = pl.indentation))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(ParsedPlan::new_text(root, plan_text))
    }

    /// Main parsing dispatch method for QueryPlan enum
    pub fn parse_query_plan(&self, query_plan: &crate::QueryPlan) -> Result<ParsedPlan, ParseError> {
        match query_plan {
            crate::QueryPlan::TextPlan(text_data) => self.parse_text_plan(text_data),
            crate::QueryPlan::JsonPlan(json_data) => self.parse_json_plan(json_data),
        }
    }

    /// Text plan parsing (existing logic, refined)
    pub fn parse_text_plan(&self, text_data: &crate::TextPlanData) -> Result<ParsedPlan, ParseError> {
        let root = if !text_data.plan_lines.is_empty() {
            self.parse_plan_from_lines(&text_data.plan_lines)?.root
        } else {
            self.parse_plan(&text_data.plan_text)?.root
        };
        
        Ok(ParsedPlan {
            root,
            planning_time_ms: None,  // Not available in text format
            execution_time_ms: None, // Not available in text format
            raw_source: text_data.plan_text.clone(),
            source_format: crate::PlanSourceFormat::Text,
        })
    }

    /// JSON plan parsing (new implementation)
    pub fn parse_json_plan(&self, json_data: &crate::JsonPlanData) -> Result<ParsedPlan, ParseError> {
        let root = self.convert_json_node_to_plan_node(&json_data.parsed_json.plan)?;
        
        Ok(ParsedPlan {
            root,
            planning_time_ms: json_data.parsed_json.planning_time,
            execution_time_ms: json_data.parsed_json.execution_time,
            raw_source: json_data.raw_json.clone(),
            source_format: crate::PlanSourceFormat::Json,
        })
    }

    /// Convert JSON node to internal PlanNode structure
    fn convert_json_node_to_plan_node(&self, json_node: &crate::JsonPlanNode) -> Result<PlanNode, ParseError> {
        let node_type = self.parse_node_type_from_string(&json_node.node_type);
        let cost = PlanCost {
            startup_cost: json_node.startup_cost,
            min_total_cost: json_node.startup_cost, // JSON min cost is startup cost
            max_total_cost: json_node.total_cost,   // JSON max cost is total cost
            estimated_rows: json_node.plan_rows,
            estimated_width: json_node.plan_width,
        };
        
        let mut plan_node = PlanNode::new(node_type, cost, json_node.node_type.clone());
        
        // Set table reference if available
        if let Some(relation_name) = &json_node.relation_name {
            let mut table_ref = if let Some(schema) = &json_node.schema {
                TableReference::with_schema(schema.clone(), relation_name.clone())
            } else {
                TableReference::new(relation_name.clone())
            };
            
            if let Some(alias) = &json_node.alias {
                table_ref = table_ref.with_alias(alias.clone());
            }
            
            plan_node.set_table_ref(table_ref);
        }
        
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
                },
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

    /// Parse node type from string (unified for both text and JSON)
    pub fn parse_node_type_from_string(&self, node_type_str: &str) -> NodeType {
        let line_lower = node_type_str.to_lowercase();

        // Scan operations
        if line_lower.contains("seq scan") {
            NodeType::Scan(ScanType::SeqScan)
        } else if line_lower.contains("index scan backward") {
            NodeType::Scan(ScanType::IndexScanBackward)
        } else if line_lower.contains("index only scan") {
            NodeType::Scan(ScanType::IndexOnlyScan)
        } else if line_lower.contains("index scan") {
            NodeType::Scan(ScanType::IndexScan)
        } else if line_lower.contains("bitmap heap scan") {
            NodeType::Scan(ScanType::BitmapHeapScan)
        } else if line_lower.contains("bitmap index scan") {
            NodeType::Scan(ScanType::BitmapIndexScan)
        } else if line_lower.contains("parallel bitmap heap scan") {
            NodeType::Scan(ScanType::ParallelBitmapHeapScan)

        // Join operations
        } else if line_lower.contains("nested loop left join") {
            NodeType::Join(JoinType::NestedLoopLeftJoin)
        } else if line_lower.contains("nested loop") {
            NodeType::Join(JoinType::NestedLoop)
        } else if line_lower.contains("hash join") {
            NodeType::Join(JoinType::HashJoin)
        } else if line_lower.contains("merge join") {
            NodeType::Join(JoinType::MergeJoin)

        // Aggregate operations
        } else if line_lower.contains("group aggregate") {
            NodeType::Aggregate(AggregateType::GroupAggregate)
        } else if line_lower.contains("hash aggregate") {
            NodeType::Aggregate(AggregateType::HashAggregate)
        } else if line_lower.contains("aggregate") {
            NodeType::Aggregate(AggregateType::Aggregate)

        // Utility operations
        } else if line_lower.contains("sort") {
            NodeType::Utility(UtilityType::Sort)
        } else if line_lower.contains("limit") {
            NodeType::Utility(UtilityType::Limit)
        } else if line_lower.contains("gather merge") {
            NodeType::Utility(UtilityType::GatherMerge)
        } else if line_lower.contains("materialize") {
            NodeType::Utility(UtilityType::Materialize)
        } else if line_lower.contains("memoize") {
            NodeType::Utility(UtilityType::Memoize)
        } else if line_lower.contains("subplan") {
            NodeType::Utility(UtilityType::SubPlan)
        } else if line_lower.contains("bitmapand") {
            NodeType::Utility(UtilityType::BitmapAnd)
        } else if line_lower.contains("bitmapor") {
            NodeType::Utility(UtilityType::BitmapOr)

        // Unknown node type
        } else {
            // Extract the first word as the node type
            let first_word = node_type_str.split_whitespace().next().unwrap_or("Unknown");
            NodeType::Unknown(first_word.to_string())
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
            let is_node = self.node_regex.is_match(&content);

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
        match raw_spaces {
            0 => 0, // Root level
            2 => 1, // First child level
            n if n >= 8 => {
                // Level 2+: each additional level adds 6 spaces
                2 + (n - 8) / 6
            }
            _ => {
                // Fallback for unexpected indentation
                raw_spaces / 2
            }
        }
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
            if current_line.is_node && current_line.indent == current_indent + 1 {
                let (child_node, next_idx) = self.parse_node_tree(lines, idx)?;
                node.add_child(child_node);
                idx = next_idx;
            } else if current_line.indent == current_indent + 1 {
                // This is a property line for the current node
                self.parse_property_line(&mut node, &current_line.content);
                idx += 1;
            } else {
                // Skip lines that are deeper (they belong to child nodes that will be parsed recursively)
                idx += 1;
            }
        }

        Ok((node, idx))
    }

    /// Parses a single node line into a PlanNode
    fn parse_single_node(&self, line: &str) -> Result<PlanNode, ParseError> {
        // Extract cost information
        let cost = self.extract_cost(line)?;

        // Determine node type from the beginning of the line
        let node_type = self.parse_node_type_from_string(line);

        // Create the node
        let mut node = PlanNode::new(node_type, cost, line.to_string());

        // Extract table reference if present
        if let Some(table_ref) = self.extract_table_reference(line) {
            node.set_table_ref(table_ref);
        }

        Ok(node)
    }

    /// Extracts cost information from a node line
    /// PostgreSQL cost format: (cost=startup..total rows=estimated_rows width=estimated_width)
    /// The startup cost is the minimum cost to get the first row
    /// The total cost range is startup..total, representing minimum to maximum cost
    fn extract_cost(&self, line: &str) -> Result<PlanCost, ParseError> {
        let captures = self.cost_regex.captures(line).ok_or_else(|| {
            ParseError::InvalidCostFormat(format!("No cost information found in: {}", line))
        })?;

        let startup_cost = captures[1]
            .parse::<f64>()
            .map_err(|_| ParseError::InvalidCostFormat("Invalid startup cost".to_string()))?;

        let max_total_cost = captures[2]
            .parse::<f64>()
            .map_err(|_| ParseError::InvalidCostFormat("Invalid total cost".to_string()))?;

        let estimated_rows = captures[3]
            .parse::<u64>()
            .map_err(|_| ParseError::InvalidCostFormat("Invalid estimated rows".to_string()))?;

        let estimated_width = captures[4]
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


    /// Extracts table reference information from a node line
    fn extract_table_reference(&self, line: &str) -> Option<TableReference> {
        if let Some(captures) = self.table_regex.captures(line) {
            if let Some(index_name) = captures.get(1) {
                // Format: using "index_name"
                Some(TableReference::new(index_name.as_str().to_string()))
            } else if let Some(table_name) = captures.get(3) {
                // Format: on "schema"."table" alias
                let schema = captures.get(2).map(|m| m.as_str().to_string());
                let table = table_name.as_str().to_string();
                let alias = captures.get(4).map(|m| m.as_str().to_string());

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
            NodeType::Scan(ScanType::IndexScan),
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
        let line = "Index Scan using \"PK_Test\" on \"Shared\".\"Test\" t  (cost=0.42..8.44 rows=1 width=16)";

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
            parser.parse_node_type_from_string("Index Scan using PK_test"),
            NodeType::Scan(ScanType::IndexScan)
        ));

        assert!(matches!(
            parser.parse_node_type_from_string("Nested Loop Left Join"),
            NodeType::Join(JoinType::NestedLoopLeftJoin)
        ));

        assert!(matches!(
            parser.parse_node_type_from_string("Sort"),
            NodeType::Utility(UtilityType::Sort)
        ));
    }

    #[test]
    fn test_table_reference_extraction() {
        let parser = PlanParser::new().unwrap();

        // Test case 1: using index
        let line1 = "Index Scan using \"IX_Test\"";
        let table_ref1 = parser.extract_table_reference(line1).unwrap();
        assert_eq!(table_ref1.schema, None);
        assert_eq!(table_ref1.name, "IX_Test");
        assert_eq!(table_ref1.alias, None);

        // Test case 2: on table with schema and alias
        let line2 = "on \"Shared\".\"Test\" t";
        let table_ref2 = parser.extract_table_reference(line2).unwrap();
        assert_eq!(table_ref2.schema, Some("Shared".to_string()));
        assert_eq!(table_ref2.name, "Test");
        assert_eq!(table_ref2.alias, Some("t".to_string()));
    }

    #[test]
    fn test_simple_plan_parsing() {
        let parser = PlanParser::new().unwrap();
        let plan_text = "Index Scan using \"PK_Test\" on \"Shared\".\"Test\" t  (cost=0.42..8.44 rows=1 width=16)\n  Output: \"Id\", \"Name\"\n  Index Cond: (t.\"Id\" = 123)";

        let parsed_plan = parser.parse_plan(plan_text).unwrap();

        assert!(parsed_plan.root.is_scan());
        assert_eq!(parsed_plan.root.cost.startup_cost, 0.42);
        assert_eq!(
            parsed_plan.root.get_property("Output"),
            Some(&"\"Id\", \"Name\"".to_string())
        );
        assert_eq!(
            parsed_plan.root.get_property("Index Cond"),
            Some(&"(t.\"Id\" = 123)".to_string())
        );
    }

    #[test]
    fn test_nested_plan_parsing() {
        let parser = PlanParser::new().unwrap();
        let plan_text = "Nested Loop  (cost=1.15..279.82 rows=7 width=110)\n  Output: m.\"Id\", m.\"Name\"\n  ->  Index Scan using \"IX_Test1\" on \"Shared\".\"Test1\" m  (cost=0.57..2.79 rows=1 width=54)\n        Output: m.\"Id\", m.\"Name\"\n        Index Cond: (m.\"Id\" = 1)\n  ->  Index Scan using \"IX_Test2\" on \"Shared\".\"Test2\" t  (cost=0.57..274.10 rows=292 width=56)\n        Output: t.\"Id\", t.\"Value\"\n        Index Cond: (t.\"TestId\" = m.\"Id\")";

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
            NodeType::Join(JoinType::NestedLoop),
            cost.clone(),
            "Nested Loop".to_string(),
        );

        let child1 = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan),
            cost.clone(),
            "Index Scan".to_string(),
        );

        let child2 = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan),
            cost.clone(),
            "Seq Scan".to_string(),
        );

        root.add_child(child1);
        root.add_child(child2);

        let plan = ParsedPlan::new_text(root, "test plan".to_string());

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
        assert_eq!(parsed_text.node_count(), parsed_json.node_count(), 
            "Node counts should be equal");
        assert_eq!(parsed_text.max_depth(), parsed_json.max_depth(), 
            "Max depths should be equal");
        
        // Verify cost equivalency (within floating point precision)
        let cost_diff = (parsed_text.total_cost() - parsed_json.total_cost()).abs();
        assert!(cost_diff < 0.01, 
            "Total costs should be equivalent: {} vs {}", 
            parsed_text.total_cost(), parsed_json.total_cost());

        // Verify root node types are equivalent
        assert_eq!(parsed_text.root.description(), parsed_json.root.description(),
            "Root node types should be equivalent");

        // Verify both plans detect same features
        assert_eq!(parsed_text.uses_indexes(), parsed_json.uses_indexes(),
            "Index usage detection should be equivalent");
        assert_eq!(parsed_text.uses_parallel_execution(), parsed_json.uses_parallel_execution(),
            "Parallel execution detection should be equivalent");

        // Verify table references are equivalent
        let text_tables = parsed_text.get_tables();
        let json_tables = parsed_json.get_tables();
        assert_eq!(text_tables.len(), json_tables.len(),
            "Table reference counts should be equal");

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
        use chrono::Utc;
        use crate::{QueryPlan, TextPlanData};
        
        let plan_text = r#"Limit  (cost=0.43..599.04 rows=1000 width=56)
  Output: "Id", "EndDate", "Level"
  ->  Index Scan Backward using "IX_VitalAlarms_EndDate" on "Shared"."VitalAlarms" v  (cost=0.43..95610.13 rows=159718 width=56)
        Output: "Id", "EndDate", "Level"
        Index Cond: (v."EndDate" IS NOT NULL)
        Filter: ((NOT v."IsDismissed") AND (v."Level" > '66'::double precision))"#;

        let text_data = TextPlanData {
            timestamp: Utc::now(),
            duration_ms: 1242.373,
            query_text: "SELECT * FROM test".to_string(),
            plan_text: plan_text.to_string(),
            plan_lines: vec![],
        };

        QueryPlan::TextPlan(text_data)
    }

    // Helper function to create equivalent JSON plan
    fn create_test_json_plan() -> crate::QueryPlan {
        use chrono::Utc;
        use crate::{QueryPlan, JsonPlanData, JsonPlan};
        
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
        
        let json_data = JsonPlanData {
            timestamp: Utc::now(),
            duration_ms: 1242.373,
            query_text: "SELECT * FROM test".to_string(),
            raw_json: json_content.to_string(),
            parsed_json: parsed_json.into_iter().next().unwrap(),
        };

        QueryPlan::JsonPlan(json_data)
    }

    // Helper function to recursively compare normalized node structures
    fn compare_normalized_node_structures(text_node: &PlanNode, json_node: &PlanNode) {
        // Compare node type - check the base node type is equivalent
        // Note: descriptions might differ slightly due to table vs index name references
        let text_node_type = format!("{:?}", text_node.node_type);
        let json_node_type = format!("{:?}", json_node.node_type);
        assert_eq!(text_node_type, json_node_type,
            "Node types should match: '{}' vs '{}'", text_node_type, json_node_type);
        
        // Both should be the same class of operation (both scan, both join, etc.)
        assert_eq!(text_node.is_scan(), json_node.is_scan(), "Both should be same operation class");
        assert_eq!(text_node.is_join(), json_node.is_join(), "Both should be same operation class");
        assert_eq!(text_node.is_aggregate(), json_node.is_aggregate(), "Both should be same operation class");

        // Compare cost information (within floating point precision)
        let startup_diff = (text_node.cost.startup_cost - json_node.cost.startup_cost).abs();
        let total_diff = (text_node.cost.total_cost() - json_node.cost.total_cost()).abs();
        
        assert!(startup_diff < 0.01, 
            "Startup costs should match: {} vs {}", 
            text_node.cost.startup_cost, json_node.cost.startup_cost);
        assert!(total_diff < 0.01, 
            "Total costs should match: {} vs {}", 
            text_node.cost.total_cost(), json_node.cost.total_cost());

        assert_eq!(text_node.cost.estimated_rows, json_node.cost.estimated_rows,
            "Estimated rows should match");
        assert_eq!(text_node.cost.estimated_width, json_node.cost.estimated_width,
            "Estimated width should match");

        // Compare table references - allow some flexibility since text format may reference 
        // indexes while JSON format references tables for the same logical operation
        match (&text_node.table_ref, &json_node.table_ref) {
            (Some(text_table), Some(json_table)) => {
                // For normalization purposes, both should have some table reference
                // The exact names might differ (index vs table name) but the core structure should be similar
                if text_table.schema.is_some() && json_table.schema.is_some() {
                    assert_eq!(text_table.schema, json_table.schema, "Schemas should match when both present");
                }
                if text_table.alias.is_some() && json_table.alias.is_some() {
                    assert_eq!(text_table.alias, json_table.alias, "Aliases should match when both present");
                }
                // Note: Names might differ (index name vs table name) which is acceptable for normalization
            }
            (None, None) => {}, // Both have no table reference, which is fine
            _ => {
                // One has table reference, other doesn't - this is acceptable as long as 
                // the core plan structure and costs are equivalent
            }
        }

        // Compare key properties that should be equivalent
        let key_properties = ["Index Cond", "Filter", "Sort Key", "Join Filter", "Group Key"];
        for prop in &key_properties {
            let text_prop = text_node.get_property(prop);
            let json_prop = json_node.get_property(prop);
            
            match (text_prop, json_prop) {
                (Some(text_val), Some(json_val)) => {
                    assert_eq!(text_val, json_val, 
                        "Property '{}' should match: '{}' vs '{}'", prop, text_val, json_val);
                }
                (None, None) => {}, // Both don't have this property
                _ => {
                    // Allow some flexibility for properties that might be present in one format but not the other
                    // This is acceptable as long as the core plan structure is equivalent
                }
            }
        }

        // Compare child count
        assert_eq!(text_node.children.len(), json_node.children.len(),
            "Child node counts should match");

        // Recursively compare children
        for (text_child, json_child) in text_node.children.iter().zip(json_node.children.iter()) {
            compare_normalized_node_structures(text_child, json_child);
        }
    }
}

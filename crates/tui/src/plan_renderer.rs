use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

use pg_loganalyze_core::{
    AggregateType, JoinType, NodeType, ParsedPlan, PlanNode, ScanType, UtilityType,
};

/// Renders execution plans as ASCII tree graphs
pub struct PlanRenderer {
    /// Show cost information in the tree
    pub show_costs: bool,
    /// Show table names in scan nodes
    pub show_tables: bool,
    /// Use colored output
    pub use_colors: bool,
}

impl Default for PlanRenderer {
    fn default() -> Self {
        Self {
            show_costs: true,
            show_tables: true,
            use_colors: true,
        }
    }
}

impl PlanRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Render a parsed plan as ASCII tree
    pub fn render_plan(&self, plan: &ParsedPlan) -> Text<'static> {
        let mut lines = Vec::new();

        // Plan summary header
        lines.push(Line::from(vec![
            Span::styled(
                "Plan Summary: ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} nodes", plan.node_count()),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(", depth ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}", plan.max_depth()),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(", cost ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{:.2}", plan.total_cost()),
                Style::default().fg(Color::Yellow),
            ),
        ]));

        // Add feature indicators
        let mut features = Vec::new();
        if plan.uses_indexes() {
            features.push(Span::styled("INDEX", Style::default().fg(Color::Green)));
        }
        if plan.uses_parallel_execution() {
            features.push(Span::styled("PARALLEL", Style::default().fg(Color::Blue)));
        }

        if !features.is_empty() {
            let mut feature_line = vec![Span::styled(
                "Features: ",
                Style::default().fg(Color::White),
            )];
            for (i, feature) in features.iter().enumerate() {
                if i > 0 {
                    feature_line.push(Span::styled(" | ", Style::default().fg(Color::Gray)));
                }
                feature_line.push(feature.clone());
            }
            lines.push(Line::from(feature_line));
        }

        lines.push(Line::from(""));

        // Render the tree starting from root
        self.render_node(&plan.root, &mut lines, "", true, true);

        Text::from(lines)
    }

    /// Render a single node and its children recursively
    fn render_node(
        &self,
        node: &PlanNode,
        lines: &mut Vec<Line<'static>>,
        prefix: &str,
        is_last: bool,
        is_root: bool,
    ) {
        // Build the tree line components
        let mut line_spans = Vec::new();

        // Add tree structure
        if !is_root {
            line_spans.push(Span::styled(
                prefix.to_string(),
                Style::default().fg(Color::Gray),
            ));
            let connector = if is_last { "└── " } else { "├── " };
            line_spans.push(Span::styled(
                connector.to_string(),
                Style::default().fg(Color::Gray),
            ));
        }

        // Add node type with color coding
        let node_text = node.description();
        let (_, node_color) = self.get_node_display(&node.node_type);
        line_spans.push(Span::styled(
            node_text,
            Style::default().fg(node_color).add_modifier(Modifier::BOLD),
        ));

        // Add table reference if available and enabled
        if self.show_tables {
            if let Some(table_ref) = &node.table_ref {
                line_spans.push(Span::styled(
                    " on ".to_string(),
                    Style::default().fg(Color::Gray),
                ));
                line_spans.push(Span::styled(
                    table_ref.display_name(),
                    Style::default().fg(Color::Magenta),
                ));
            }
        }

        // Add cost information if enabled - now showing the full range
        if self.show_costs {
            line_spans.push(Span::styled(" ".to_string(), Style::default()));
            line_spans.push(Span::styled(
                format!(
                    "(cost={:.2}..{:.2}, rows={}, width={})",
                    node.cost.startup_cost,
                    node.cost.max_total_cost,
                    node.cost.estimated_rows,
                    node.cost.estimated_width
                ),
                Style::default().fg(Color::Yellow),
            ));
        }

        lines.push(Line::from(line_spans));

        // Add node properties as sub-lines using typed property system
        if !node.properties.is_empty() {
            let property_prefix = if is_root {
                "      "  // Align with root children + tree connector
            } else {
                &format!("{}{}    ", prefix, if is_last { "    " } else { "│   " })
            };

            // Show key properties using typed accessors for better performance and type safety
            self.render_typed_properties(node, &property_prefix, lines);
        }

        // Render children
        for (i, child) in node.children.iter().enumerate() {
            let is_last_child = i == node.children.len() - 1;
            let child_prefix = if is_root {
                "  ".to_string()  // Give root children a small indent
            } else {
                format!("{}{}", prefix, if is_last { "    " } else { "│   " })
            };

            self.render_node(child, lines, &child_prefix, is_last_child, false);
        }
    }

    /// Get display text and color for node types
    fn get_node_display(&self, node_type: &NodeType) -> (String, Color) {
        match node_type {
            NodeType::Scan(scan_type) => {
                let text = format!("{scan_type}");
                let color = match scan_type {
                    ScanType::SeqScan { .. } => Color::Red,
                    ScanType::IndexScan { .. } => Color::Green,
                    ScanType::BitmapHeapScan { .. } => Color::Yellow,
                    ScanType::BitmapIndexScan { .. } => Color::Yellow,
                    ScanType::ParallelBitmapHeapScan { .. } => Color::Blue,
                };
                (text, color)
            }
            NodeType::Join(join_type) => {
                let text = match join_type {
                    JoinType::NestedLoop { .. } => "Nested Loop",
                    JoinType::NestedLoopLeftJoin { .. } => "Nested Loop Left Join",
                    JoinType::HashJoin { .. } => "Hash Join",
                    JoinType::MergeJoin { .. } => "Merge Join",
                };
                (text.to_string(), Color::Magenta)
            }
            NodeType::Aggregate(agg_type) => {
                let text = match agg_type {
                    AggregateType::Aggregate { .. } => "Aggregate",
                    AggregateType::GroupAggregate { .. } => "Group Aggregate",
                    AggregateType::HashAggregate { .. } => "Hash Aggregate",
                };
                (text.to_string(), Color::Cyan)
            }
            NodeType::Utility(util_type) => {
                let text = match util_type {
                    UtilityType::Sort { .. } => "Sort",
                    UtilityType::Limit { .. } => "Limit",
                    UtilityType::GatherMerge { .. } => "Gather Merge",
                    UtilityType::Materialize => "Materialize",
                    UtilityType::Memoize { .. } => "Memoize",
                    UtilityType::SubPlan { .. } => "SubPlan",
                    UtilityType::BitmapAnd => "BitmapAnd",
                    UtilityType::BitmapOr => "BitmapOr",
                };
                (text.to_string(), Color::LightBlue)
            }
            NodeType::Unknown(name) => (name.clone(), Color::Gray),
        }
    }

    /// Render typed properties for a node using the new property system
    fn render_typed_properties(&self, node: &PlanNode, prefix: &str, lines: &mut Vec<Line<'static>>) {
        let props = node.properties();
        
        // Index Condition - high priority for scan nodes
        if let Some(index_cond) = props.index_condition() {
            self.add_property_line(prefix, "Index Cond", index_cond, lines);
        }
        
        // Filter conditions - important for selectivity
        if let Some(filter) = props.filter() {
            self.add_property_line(prefix, "Filter", filter, lines);
        }
        
        // Join conditions - critical for join nodes  
        if let Some(join_filter) = props.join_filter() {
            self.add_property_line(prefix, "Join Filter", join_filter, lines);
        }
        
        // Sort key - important for sort operations
        if let Some(sort_key) = props.sort_key() {
            self.add_property_line(prefix, "Sort Key", sort_key, lines);
        }
        
        // Group key - important for aggregation
        if let Some(group_key) = props.group_key() {
            self.add_property_line(prefix, "Group Key", group_key, lines);
        }
        
        // Parallel execution info - shows parallelization
        if let Some(workers_planned) = props.workers_planned() {
            self.add_property_line(prefix, "Workers Planned", &workers_planned.to_string(), lines);
        }
        
        if let Some(workers_launched) = props.workers_launched() {
            self.add_property_line(prefix, "Workers Launched", &workers_launched.to_string(), lines);
        }
        
        // Join optimization info
        if let Some(inner_unique) = props.inner_unique() {
            if inner_unique {
                self.add_property_line(prefix, "Inner Unique", "true", lines);
            }
        }
        
        // Cache information
        if let Some(cache_key) = props.get("Cache Key") {
            self.add_property_line(prefix, "Cache Key", &cache_key, lines);
        }
        
        if let Some(cache_mode) = props.get("Cache Mode") {
            self.add_property_line(prefix, "Cache Mode", &cache_mode, lines);
        }
        
        // Recheck condition for bitmap scans
        if let Some(recheck_cond) = props.get("Recheck Cond") {
            self.add_property_line(prefix, "Recheck Cond", &recheck_cond, lines);
        }
        
        // Table and index names
        if let Some(relation_name) = props.relation_name() {
            self.add_property_line(prefix, "Relation", relation_name, lines);
        }
        
        if let Some(index_name) = props.index_name() {
            self.add_property_line(prefix, "Index", index_name, lines);
        }
        
        // Show any custom properties that aren't covered above
        for property in props.iter() {
            if let pg_loganalyze_core::PlanProperty::Custom { key, value } = property {
                if self.should_show_custom_property(key) {
                    self.add_property_line(prefix, key, value, lines);
                }
            }
        }
    }
    
    /// Add a single property line to the output
    fn add_property_line(&self, prefix: &str, key: &str, value: &str, lines: &mut Vec<Line<'static>>) {
        let mut prop_spans = Vec::new();
        prop_spans.push(Span::styled(
            prefix.to_string(),
            Style::default().fg(Color::Gray),
        ));
        prop_spans.push(Span::styled(
            format!("{}: ", key),
            Style::default().fg(Color::Blue),
        ));
        
        // Truncate long values but be more generous than before
        let display_value = if value.len() > 120 {
            format!("{}...", &value[..117])
        } else {
            value.to_string()
        };
        
        prop_spans.push(Span::styled(
            display_value,
            Style::default().fg(Color::White),
        ));
        
        lines.push(Line::from(prop_spans));
    }
    
    /// Determine which custom properties to show
    fn should_show_custom_property(&self, key: &str) -> bool {
        match key {
            "Output" => false, // Too verbose for tree view
            "Sort Method" => true,
            "Sort Space Used" => true,
            "Function" => true,
            "Subplan Name" => true,
            _ => true,
        }
    }

    /// Render plan with minimal information for compact display
    pub fn render_plan_compact(&self, plan: &ParsedPlan) -> Text<'static> {
        let mut lines = Vec::new();

        // Just show the node types in a simple tree
        self.render_node_compact(&plan.root, &mut lines, "", true, true);

        Text::from(lines)
    }

    /// Render node in compact mode
    fn render_node_compact(
        &self,
        node: &PlanNode,
        lines: &mut Vec<Line<'static>>,
        prefix: &str,
        is_last: bool,
        is_root: bool,
    ) {
        let mut line_spans = Vec::new();

        if !is_root {
            line_spans.push(Span::styled(
                prefix.to_string(),
                Style::default().fg(Color::Gray),
            ));
            let connector = if is_last { "└── " } else { "├── " };
            line_spans.push(Span::styled(
                connector.to_string(),
                Style::default().fg(Color::Gray),
            ));
        }

        let node_text = node.description();
        let (_, node_color) = self.get_node_display(&node.node_type);
        line_spans.push(Span::styled(node_text, Style::default().fg(node_color)));

        // Show cost in compact format
        line_spans.push(Span::styled(
            format!(" ({:.1})", node.cost.total_cost()),
            Style::default().fg(Color::Yellow),
        ));

        lines.push(Line::from(line_spans));

        // Render children
        for (i, child) in node.children.iter().enumerate() {
            let is_last_child = i == node.children.len() - 1;
            let child_prefix = if is_root {
                "  ".to_string()  // Give root children a small indent
            } else {
                format!("{}{}", prefix, if is_last { "    " } else { "│   " })
            };

            self.render_node_compact(child, lines, &child_prefix, is_last_child, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pg_loganalyze_core::{
        NodeType, ParsedPlan, PlanCost, PlanNode, ScanType, TableReference, IndexReference,
    };

    #[test]
    fn test_plan_rendering() {
        let renderer = PlanRenderer::new();

        // Create a simple test plan
        let cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 100.0,
            max_total_cost: 100.0,
            estimated_rows: 1000,
            estimated_width: 50,
        };

        let mut root = PlanNode::new(
            NodeType::Join(JoinType::NestedLoop { inner_unique: false }),
            cost.clone(),
            "Nested Loop".to_string(),
        );

        let mut child1 = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan { 
                table: TableReference::new("test_table".to_string()), 
                index: Some(IndexReference { name: "test_index".to_string() }), 
                backward: false, 
                only: false 
            }),
            cost.clone(),
            "Index Scan".to_string(),
        );
        child1.set_table_ref(TableReference::with_schema(
            "public".to_string(),
            "users".to_string(),
        ));

        let child2 = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan { table: TableReference::new("test_table".to_string()) }),
            cost.clone(),
            "Seq Scan".to_string(),
        );

        root.add_child(child1);
        root.add_child(child2);

        let plan = ParsedPlan::new(root);

        let rendered = renderer.render_plan(&plan);

        // Basic check that something was rendered
        assert!(!rendered.lines.is_empty());

        // Check that it contains expected node types
        let text_content = format!("{:?}", rendered);
        assert!(text_content.contains("Nested Loop"));
        assert!(text_content.contains("Index Scan"));
        assert!(text_content.contains("Sequential Scan"));  // Updated to match description() output
    }

    #[test]
    fn test_compact_rendering() {
        let renderer = PlanRenderer::new();

        let cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 50.0,
            max_total_cost: 50.0,
            estimated_rows: 100,
            estimated_width: 25,
        };

        let root = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan { 
                table: TableReference::new("test_table".to_string()), 
                index: Some(IndexReference { name: "test_index".to_string() }), 
                backward: false, 
                only: false 
            }),
            cost,
            "Index Scan".to_string(),
        );

        let plan = ParsedPlan::new(root);

        let rendered = renderer.render_plan_compact(&plan);

        assert!(!rendered.lines.is_empty());

        let text_content = format!("{:?}", rendered);
        assert!(text_content.contains("Index Scan"));
        assert!(text_content.contains("50")); // Cost should be shown
    }
}

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
        let (node_text, node_color) = self.get_node_display(&node.node_type);
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

        // Add node properties as sub-lines
        if !node.properties.is_empty() {
            let property_prefix = if is_root {
                "    "
            } else {
                &format!("{}{}    ", prefix, if is_last { "    " } else { "│   " })
            };

            // Show key properties
            for (key, value) in node.properties.iter() {
                if self.should_show_property(key) {
                    let mut prop_spans = Vec::new();
                    prop_spans.push(Span::styled(
                        property_prefix.to_string(),
                        Style::default().fg(Color::Gray),
                    ));
                    prop_spans.push(Span::styled(
                        format!("{}: ", key),
                        Style::default().fg(Color::Blue),
                    ));

                    // Truncate long values
                    let display_value = if value.len() > 80 {
                        format!("{}...", &value[..77])
                    } else {
                        value.clone()
                    };
                    prop_spans.push(Span::styled(
                        display_value,
                        Style::default().fg(Color::White),
                    ));

                    lines.push(Line::from(prop_spans));
                }
            }
        }

        // Render children
        for (i, child) in node.children.iter().enumerate() {
            let is_last_child = i == node.children.len() - 1;
            let child_prefix = if is_root {
                "".to_string()
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
                let (text, color) = match scan_type {
                    ScanType::SeqScan => ("Seq Scan", Color::Red),
                    ScanType::IndexScan => ("Index Scan", Color::Green),
                    ScanType::IndexScanBackward => ("Index Scan Backward", Color::Green),
                    ScanType::IndexOnlyScan => ("Index Only Scan", Color::Green),
                    ScanType::BitmapHeapScan => ("Bitmap Heap Scan", Color::Yellow),
                    ScanType::BitmapIndexScan => ("Bitmap Index Scan", Color::Yellow),
                    ScanType::ParallelBitmapHeapScan => ("Parallel Bitmap Heap Scan", Color::Blue),
                };
                (text.to_string(), color)
            }
            NodeType::Join(join_type) => {
                let text = match join_type {
                    JoinType::NestedLoop => "Nested Loop",
                    JoinType::NestedLoopLeftJoin => "Nested Loop Left Join",
                    JoinType::HashJoin => "Hash Join",
                    JoinType::MergeJoin => "Merge Join",
                };
                (text.to_string(), Color::Magenta)
            }
            NodeType::Aggregate(agg_type) => {
                let text = match agg_type {
                    AggregateType::Aggregate => "Aggregate",
                    AggregateType::GroupAggregate => "Group Aggregate",
                    AggregateType::HashAggregate => "Hash Aggregate",
                };
                (text.to_string(), Color::Cyan)
            }
            NodeType::Utility(util_type) => {
                let text = match util_type {
                    UtilityType::Sort => "Sort",
                    UtilityType::Limit => "Limit",
                    UtilityType::GatherMerge => "Gather Merge",
                    UtilityType::Materialize => "Materialize",
                    UtilityType::Memoize => "Memoize",
                    UtilityType::SubPlan => "SubPlan",
                    UtilityType::BitmapAnd => "BitmapAnd",
                    UtilityType::BitmapOr => "BitmapOr",
                };
                (text.to_string(), Color::LightBlue)
            }
            NodeType::Unknown(name) => (name.clone(), Color::Gray),
        }
    }

    /// Determine which properties to show in the tree
    fn should_show_property(&self, key: &str) -> bool {
        match key {
            "Output" => false, // Too verbose for tree view
            "Index Cond" => true,
            "Filter" => true,
            "Sort Key" => true,
            "Join Filter" => true,
            "Group Key" => true,
            "Cache Key" => true,
            "Workers Planned" => true,
            "Recheck Cond" => true,
            "Inner Unique" => true,
            "Cache Mode" => true,
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

        let (node_text, node_color) = self.get_node_display(&node.node_type);
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
                "".to_string()
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
    use pg_loganalyze_core::{PlanCost, TableReference, ParsedPlan, PlanNode, NodeType, ScanType, PlanSourceFormat};

    #[test]
    fn test_plan_rendering() {
        let renderer = PlanRenderer::new();

        // Create a simple test plan
        let cost = PlanCost {
            startup_cost: 0.0,
            total_cost: 100.0,
            estimated_rows: 1000,
            estimated_width: 50,
        };

        let mut root = PlanNode::new(
            NodeType::Join(JoinType::NestedLoop),
            cost.clone(),
            "Nested Loop".to_string(),
        );

        let mut child1 = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan),
            cost.clone(),
            "Index Scan".to_string(),
        );
        child1.set_table_ref(TableReference::with_schema(
            "public".to_string(),
            "users".to_string(),
        ));

        let child2 = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan),
            cost.clone(),
            "Seq Scan".to_string(),
        );

        root.add_child(child1);
        root.add_child(child2);

        let plan = ParsedPlan::new_text(root, "test plan".to_string());

        let rendered = renderer.render_plan(&plan);

        // Basic check that something was rendered
        assert!(!rendered.lines.is_empty());

        // Check that it contains expected node types
        let text_content = format!("{:?}", rendered);
        assert!(text_content.contains("Nested Loop"));
        assert!(text_content.contains("Index Scan"));
        assert!(text_content.contains("Seq Scan"));
    }

    #[test]
    fn test_compact_rendering() {
        let renderer = PlanRenderer::new();

        let cost = PlanCost {
            startup_cost: 0.0,
            total_cost: 50.0,
            estimated_rows: 100,
            estimated_width: 25,
        };

        let root = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan),
            cost,
            "Index Scan".to_string(),
        );

        let plan = ParsedPlan::new_text(root, "simple plan".to_string());

        let rendered = renderer.render_plan_compact(&plan);

        assert!(!rendered.lines.is_empty());

        let text_content = format!("{:?}", rendered);
        assert!(text_content.contains("Index Scan"));
        assert!(text_content.contains("50")); // Cost should be shown
    }
}

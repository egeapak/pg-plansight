use super::{AnalysisContext, NodePath};
use crate::{ParsedPlan, PlanNode};

/// Trait for visiting plan nodes during traversal
pub trait NodeVisitor {
    /// Visit a single node with its path and context
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, context: &AnalysisContext);

    /// Called before visiting children (optional override)
    fn enter_node(&mut self, _node: &PlanNode, _path: &NodePath, _context: &AnalysisContext) {}

    /// Called after visiting children (optional override)  
    fn exit_node(&mut self, _node: &PlanNode, _path: &NodePath, _context: &AnalysisContext) {}
}

/// Trait for collecting results during traversal
pub trait NodeCollector<T> {
    /// Process a node and optionally return a result
    fn collect_from_node(
        &mut self,
        node: &PlanNode,
        path: &NodePath,
        context: &AnalysisContext,
    ) -> Option<T>;
}

/// Utility functions for traversing plan trees
pub struct PlanTraversal;

impl PlanTraversal {
    /// Traverse the plan tree depth-first, visiting each node
    /// Uses iterative implementation to avoid stack overflow on deep plans
    pub fn depth_first<V: NodeVisitor>(
        plan: &ParsedPlan,
        visitor: &mut V,
        context: &AnalysisContext,
    ) {
        use std::collections::VecDeque;

        #[derive(Debug)]
        enum StackFrame<'a> {
            Enter(&'a PlanNode, NodePath),
            Visit(&'a PlanNode, NodePath),
            Exit(&'a PlanNode, NodePath),
        }

        let mut stack = VecDeque::new();
        let root_path = NodePath::root();

        // Push in reverse order: Exit, Visit, Enter (so Enter is processed first)
        stack.push_back(StackFrame::Exit(&plan.root, root_path.clone()));
        stack.push_back(StackFrame::Visit(&plan.root, root_path.clone()));
        stack.push_back(StackFrame::Enter(&plan.root, root_path));

        while let Some(frame) = stack.pop_back() {
            match frame {
                StackFrame::Enter(node, path) => {
                    visitor.enter_node(node, &path, context);

                    // Push children in reverse order (last child first) to maintain depth-first order
                    for (index, child) in node.children.iter().enumerate().rev() {
                        let child_path = path.child_of(index, child.description());
                        stack.push_back(StackFrame::Exit(child, child_path.clone()));
                        stack.push_back(StackFrame::Visit(child, child_path.clone()));
                        stack.push_back(StackFrame::Enter(child, child_path));
                    }
                }
                StackFrame::Visit(node, path) => {
                    visitor.visit_node(node, &path, context);
                }
                StackFrame::Exit(node, path) => {
                    visitor.exit_node(node, &path, context);
                }
            }
        }
    }

    /// Traverse the plan tree breadth-first, visiting each node
    pub fn breadth_first<V: NodeVisitor>(
        plan: &ParsedPlan,
        visitor: &mut V,
        context: &AnalysisContext,
    ) {
        use std::collections::VecDeque;

        let mut queue = VecDeque::new();
        queue.push_back((&plan.root, NodePath::root()));

        while let Some((node, path)) = queue.pop_front() {
            visitor.enter_node(node, &path, context);
            visitor.visit_node(node, &path, context);

            // Add children to queue
            for (index, child) in node.children.iter().enumerate() {
                let child_path = path.child_of(index, child.description());
                queue.push_back((child, child_path));
            }

            visitor.exit_node(node, &path, context);
        }
    }

    /// Collect results from nodes using a collector
    /// Uses iterative implementation to avoid stack overflow on deep plans
    pub fn collect<T, C: NodeCollector<T>>(
        plan: &ParsedPlan,
        collector: &mut C,
        context: &AnalysisContext,
    ) -> Vec<T> {
        use std::collections::VecDeque;

        let mut results = Vec::new();
        let mut stack = VecDeque::new();
        stack.push_back((&plan.root, NodePath::root()));

        while let Some((node, path)) = stack.pop_back() {
            if let Some(result) = collector.collect_from_node(node, &path, context) {
                results.push(result);
            }

            // Add children to stack in reverse order to maintain depth-first order
            for (index, child) in node.children.iter().enumerate().rev() {
                let child_path = path.child_of(index, child.description());
                stack.push_back((child, child_path));
            }
        }

        results
    }

    /// Find all nodes matching a predicate
    /// Uses iterative implementation to avoid stack overflow on deep plans
    pub fn find_nodes<F>(plan: &ParsedPlan, predicate: F) -> Vec<(&PlanNode, NodePath)>
    where
        F: Fn(&PlanNode) -> bool,
    {
        use std::collections::VecDeque;

        let mut results = Vec::new();
        let mut stack = VecDeque::new();
        stack.push_back((&plan.root, NodePath::root()));

        while let Some((node, path)) = stack.pop_back() {
            if predicate(node) {
                results.push((node, path.clone()));
            }

            // Add children to stack in reverse order to maintain depth-first order
            for (index, child) in node.children.iter().enumerate().rev() {
                let child_path = path.child_of(index, child.description());
                stack.push_back((child, child_path));
            }
        }

        results
    }

    /// Get all leaf nodes (nodes with no children)
    pub fn get_leaf_nodes(plan: &ParsedPlan) -> Vec<(&PlanNode, NodePath)> {
        Self::find_nodes(plan, |node| node.children.is_empty())
    }

    /// Get all nodes at a specific depth level
    /// Uses iterative implementation to avoid stack overflow on deep plans
    pub fn get_nodes_at_depth(
        plan: &ParsedPlan,
        target_depth: usize,
    ) -> Vec<(&PlanNode, NodePath)> {
        use std::collections::VecDeque;

        let mut results = Vec::new();
        let mut stack = VecDeque::new();
        stack.push_back((&plan.root, NodePath::root(), 0usize));

        while let Some((node, path, current_depth)) = stack.pop_back() {
            if current_depth == target_depth {
                results.push((node, path));
                continue;
            }

            if current_depth < target_depth {
                // Add children to stack in reverse order to maintain depth-first order
                for (index, child) in node.children.iter().enumerate().rev() {
                    let child_path = path.child_of(index, child.description());
                    stack.push_back((child, child_path, current_depth + 1));
                }
            }
        }

        results
    }

    /// Get the parent-child relationships in the plan
    /// Uses iterative implementation to avoid stack overflow on deep plans
    #[allow(clippy::type_complexity)]
    pub fn get_parent_child_pairs(
        plan: &ParsedPlan,
    ) -> Vec<((&PlanNode, NodePath), (&PlanNode, NodePath))> {
        use std::collections::VecDeque;

        let mut results = Vec::new();
        let mut stack = VecDeque::new();
        stack.push_back((&plan.root, NodePath::root()));

        while let Some((node, path)) = stack.pop_back() {
            for (index, child) in node.children.iter().enumerate() {
                let child_path = path.child_of(index, child.description());
                results.push(((node, path.clone()), (child, child_path.clone())));

                // Add child to stack for further processing (in reverse order for depth-first)
                stack.push_back((child, child_path));
            }
        }

        results
    }
}

/// Helper struct for nodes that want to analyze relationships between nodes
pub struct NodeRelationshipAnalyzer<'a> {
    #[allow(dead_code)]
    plan: &'a ParsedPlan,
    #[allow(dead_code)]
    context: &'a AnalysisContext,
}

impl<'a> NodeRelationshipAnalyzer<'a> {
    pub fn new(plan: &'a ParsedPlan, context: &'a AnalysisContext) -> Self {
        Self { plan, context }
    }

    /// Get the parent of a node at the given path
    pub fn get_parent(&self, node_path: &NodePath) -> Option<&PlanNode> {
        if node_path.path.is_empty() {
            return None; // Root has no parent
        }

        let parent_path = &node_path.path[..node_path.path.len() - 1];
        self.get_node_at_path(parent_path)
    }

    /// Get siblings of a node (other children of the same parent)
    pub fn get_siblings(&self, node_path: &NodePath) -> Vec<&PlanNode> {
        if let Some(parent) = self.get_parent(node_path) {
            let self_index = node_path.path.last().unwrap_or(&0);
            parent
                .children
                .iter()
                .enumerate()
                .filter(|(i, _)| i != self_index)
                .map(|(_, child)| child)
                .collect()
        } else {
            vec![]
        }
    }

    /// Get all ancestors of a node (parents, grandparents, etc.)
    pub fn get_ancestors(&self, node_path: &NodePath) -> Vec<&PlanNode> {
        let mut ancestors = Vec::new();
        let mut current_path = node_path.path.clone();

        while !current_path.is_empty() {
            current_path.pop(); // Remove last element to get parent path
            if let Some(ancestor) = self.get_node_at_path(&current_path) {
                ancestors.push(ancestor);
            }
        }

        ancestors
    }

    /// Get all descendants of a node (children, grandchildren, etc.)
    #[allow(clippy::only_used_in_recursion)]
    pub fn get_descendants<'b>(&self, node: &'b PlanNode) -> Vec<&'b PlanNode> {
        let mut descendants = Vec::new();
        for child in &node.children {
            descendants.push(child);
            descendants.extend(self.get_descendants(child));
        }
        descendants
    }

    /// Check if one node is an ancestor of another
    pub fn is_ancestor(
        &self,
        potential_ancestor_path: &NodePath,
        descendant_path: &NodePath,
    ) -> bool {
        if potential_ancestor_path.path.len() >= descendant_path.path.len() {
            return false;
        }

        descendant_path.path[..potential_ancestor_path.path.len()] == potential_ancestor_path.path
    }

    /// Get a node at a specific path
    fn get_node_at_path(&self, path: &[usize]) -> Option<&PlanNode> {
        let mut current_node = &self.plan.root;

        for &index in path {
            if index >= current_node.children.len() {
                return None;
            }
            current_node = &current_node.children[index];
        }

        Some(current_node)
    }
}
/// Common visitor implementations for typical analysis patterns
pub struct CountingVisitor {
    pub count: usize,
    predicate: Box<dyn Fn(&PlanNode) -> bool>,
}

impl CountingVisitor {
    pub fn new<F>(predicate: F) -> Self
    where
        F: Fn(&PlanNode) -> bool + 'static,
    {
        Self {
            count: 0,
            predicate: Box::new(predicate),
        }
    }
}

impl NodeVisitor for CountingVisitor {
    fn visit_node(&mut self, node: &PlanNode, _path: &NodePath, _context: &AnalysisContext) {
        if (self.predicate)(node) {
            self.count += 1;
        }
    }
}

/// Visitor that collects metrics from nodes
pub struct MetricsCollector {
    pub metrics: Vec<(NodePath, String, f64)>, // (path, metric_name, value)
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            metrics: Vec::new(),
        }
    }
}

impl NodeVisitor for MetricsCollector {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        // Collect common metrics
        self.metrics.push((
            path.clone(),
            "total_cost".to_string(),
            node.cost.total_cost(),
        ));
        self.metrics.push((
            path.clone(),
            "startup_cost".to_string(),
            node.cost.startup_cost,
        ));
        self.metrics.push((
            path.clone(),
            "estimated_rows".to_string(),
            node.cost.estimated_rows as f64,
        ));
        self.metrics.push((
            path.clone(),
            "estimated_width".to_string(),
            node.cost.estimated_width as f64,
        ));

        // Add actual metrics if available
        if let Some(actuals) = &node.actuals {
            if let Some(actual_time) = actuals.actual_time_ms {
                self.metrics
                    .push((path.clone(), "actual_time_ms".to_string(), actual_time));
            }
            if let Some(actual_rows) = actuals.actual_rows {
                self.metrics
                    .push((path.clone(), "actual_rows".to_string(), actual_rows as f64));
            }
            if let Some(actual_loops) = actuals.actual_loops {
                self.metrics.push((
                    path.clone(),
                    "actual_loops".to_string(),
                    actual_loops as f64,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeType, ParsedPlan, PlanCost, PlanNode, ScanType, TableReference};

    fn create_deep_plan(depth: usize) -> ParsedPlan {
        // Create a deeply nested plan for testing stack overflow resistance
        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: format!("table_{}", depth),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            format!("Leaf node {}", depth),
        );

        // Build a chain: root -> child1 -> child2 -> ... -> leaf
        for i in (0..depth).rev() {
            let parent = PlanNode::new(
                NodeType::Scan(ScanType::SeqScan {
                    table: TableReference {
                        schema: None,
                        name: format!("table_{}", i),
                        alias: None,
                    },
                }),
                PlanCost {
                    startup_cost: 0.0,
                    min_total_cost: 0.0,
                    max_total_cost: 100.0,
                    estimated_rows: 1000,
                    estimated_width: 50,
                },
                format!("Node {}", i),
            );

            let mut new_parent = parent;
            new_parent.add_child(node);
            node = new_parent;
        }

        ParsedPlan::new(node)
    }

    struct TestVisitor {
        visited_nodes: Vec<String>,
        enter_count: usize,
        visit_count: usize,
        exit_count: usize,
    }

    impl TestVisitor {
        fn new() -> Self {
            Self {
                visited_nodes: Vec::new(),
                enter_count: 0,
                visit_count: 0,
                exit_count: 0,
            }
        }
    }

    impl NodeVisitor for TestVisitor {
        fn visit_node(&mut self, node: &PlanNode, _path: &NodePath, _context: &AnalysisContext) {
            self.visited_nodes.push(node.description());
            self.visit_count += 1;
        }

        fn enter_node(&mut self, _node: &PlanNode, _path: &NodePath, _context: &AnalysisContext) {
            self.enter_count += 1;
        }

        fn exit_node(&mut self, _node: &PlanNode, _path: &NodePath, _context: &AnalysisContext) {
            self.exit_count += 1;
        }
    }

    #[test]
    fn test_stack_safe_depth_first_traversal() {
        // Test with a moderately deep plan (1000 levels should not cause stack overflow)
        let plan = create_deep_plan(1000);
        let context = AnalysisContext::default();
        let mut visitor = TestVisitor::new();

        // This should complete without stack overflow
        PlanTraversal::depth_first(&plan, &mut visitor, &context);

        assert_eq!(visitor.visit_count, 1001); // root + 1000 nested nodes
        assert_eq!(visitor.enter_count, 1001);
        assert_eq!(visitor.exit_count, 1001);
        assert_eq!(visitor.visited_nodes.len(), 1001);
    }

    #[test]
    fn test_stack_safe_find_nodes() {
        // Test finding nodes in a deep plan
        let plan = create_deep_plan(500);

        let found_nodes =
            PlanTraversal::find_nodes(&plan, |node| node.description().contains("Node"));

        assert_eq!(found_nodes.len(), 500); // Should find all non-leaf nodes
    }

    #[test]
    fn test_stack_safe_collect() {
        struct TestCollector;

        impl NodeCollector<String> for TestCollector {
            fn collect_from_node(
                &mut self,
                node: &PlanNode,
                _path: &NodePath,
                _context: &AnalysisContext,
            ) -> Option<String> {
                if node.description().starts_with("Node") {
                    Some(node.description())
                } else {
                    None
                }
            }
        }

        let plan = create_deep_plan(200);
        let context = AnalysisContext::default();
        let mut collector = TestCollector;

        let results = PlanTraversal::collect(&plan, &mut collector, &context);
        assert_eq!(results.len(), 200); // Should collect all non-leaf nodes
    }

    #[test]
    fn test_stack_safe_get_nodes_at_depth() {
        let plan = create_deep_plan(100);

        // Test getting nodes at various depths
        let nodes_at_depth_0 = PlanTraversal::get_nodes_at_depth(&plan, 0);
        assert_eq!(nodes_at_depth_0.len(), 1); // Root only

        let nodes_at_depth_50 = PlanTraversal::get_nodes_at_depth(&plan, 50);
        assert_eq!(nodes_at_depth_50.len(), 1); // One node at depth 50

        let nodes_at_depth_100 = PlanTraversal::get_nodes_at_depth(&plan, 100);
        assert_eq!(nodes_at_depth_100.len(), 1); // Leaf node
    }

    #[test]
    fn test_stack_safe_parent_child_pairs() {
        let plan = create_deep_plan(50);

        let pairs = PlanTraversal::get_parent_child_pairs(&plan);
        assert_eq!(pairs.len(), 50); // Each node has exactly one child except leaf
    }

    #[test]
    fn test_breadth_first_vs_depth_first_ordering() {
        // Create a simple tree: root with 2 children, each child has 1 child
        let mut root = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "root".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Root".to_string(),
        );

        for i in 0..2 {
            let mut child = PlanNode::new(
                NodeType::Scan(ScanType::SeqScan {
                    table: TableReference {
                        schema: None,
                        name: format!("child_{}", i),
                        alias: None,
                    },
                }),
                PlanCost {
                    startup_cost: 0.0,
                    min_total_cost: 0.0,
                    max_total_cost: 100.0,
                    estimated_rows: 1000,
                    estimated_width: 50,
                },
                format!("Child {}", i),
            );

            let grandchild = PlanNode::new(
                NodeType::Scan(ScanType::SeqScan {
                    table: TableReference {
                        schema: None,
                        name: format!("grandchild_{}", i),
                        alias: None,
                    },
                }),
                PlanCost {
                    startup_cost: 0.0,
                    min_total_cost: 0.0,
                    max_total_cost: 100.0,
                    estimated_rows: 1000,
                    estimated_width: 50,
                },
                format!("Grandchild {}", i),
            );

            child.add_child(grandchild);
            root.add_child(child);
        }

        let plan = ParsedPlan::new(root);
        let context = AnalysisContext::default();

        // Test depth-first
        let mut depth_visitor = TestVisitor::new();
        PlanTraversal::depth_first(&plan, &mut depth_visitor, &context);

        // Test breadth-first
        let mut breadth_visitor = TestVisitor::new();
        PlanTraversal::breadth_first(&plan, &mut breadth_visitor, &context);

        // Both should visit the same number of nodes
        assert_eq!(
            depth_visitor.visited_nodes.len(),
            breadth_visitor.visited_nodes.len()
        );
        assert_eq!(depth_visitor.visited_nodes.len(), 5); // root + 2 children + 2 grandchildren

        // But the order should be different
        assert_ne!(depth_visitor.visited_nodes, breadth_visitor.visited_nodes);
    }
}

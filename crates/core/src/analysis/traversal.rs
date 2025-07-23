use crate::{PlanNode, ParsedPlan};
use super::{AnalysisContext, NodePath};

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
    fn collect_from_node(&mut self, node: &PlanNode, path: &NodePath, context: &AnalysisContext) -> Option<T>;
}

/// Utility functions for traversing plan trees
pub struct PlanTraversal;

impl PlanTraversal {
    /// Traverse the plan tree depth-first, visiting each node
    pub fn depth_first<V: NodeVisitor>(
        plan: &ParsedPlan,
        visitor: &mut V,
        context: &AnalysisContext,
    ) {
        let root_path = NodePath::root();
        Self::depth_first_recursive(&plan.root, &root_path, visitor, context);
    }
    
    fn depth_first_recursive<V: NodeVisitor>(
        node: &PlanNode,
        path: &NodePath,
        visitor: &mut V,
        context: &AnalysisContext,
    ) {
        visitor.enter_node(node, path, context);
        visitor.visit_node(node, path, context);
        
        // Visit children
        for (index, child) in node.children.iter().enumerate() {
            let child_path = path.child_of(index, child.description());
            Self::depth_first_recursive(child, &child_path, visitor, context);
        }
        
        visitor.exit_node(node, path, context);
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
    pub fn collect<T, C: NodeCollector<T>>(
        plan: &ParsedPlan,
        collector: &mut C,
        context: &AnalysisContext,
    ) -> Vec<T> {
        let mut results = Vec::new();
        Self::collect_recursive(&plan.root, &NodePath::root(), collector, context, &mut results);
        results
    }
    
    fn collect_recursive<T, C: NodeCollector<T>>(
        node: &PlanNode,
        path: &NodePath,
        collector: &mut C,
        context: &AnalysisContext,
        results: &mut Vec<T>,
    ) {
        if let Some(result) = collector.collect_from_node(node, path, context) {
            results.push(result);
        }
        
        // Collect from children
        for (index, child) in node.children.iter().enumerate() {
            let child_path = path.child_of(index, child.description());
            Self::collect_recursive(child, &child_path, collector, context, results);
        }
    }
    
    /// Find all nodes matching a predicate
    pub fn find_nodes<F>(
        plan: &ParsedPlan,
        predicate: F,
    ) -> Vec<(&PlanNode, NodePath)>
    where
        F: Fn(&PlanNode) -> bool,
    {
        let mut results = Vec::new();
        Self::find_nodes_recursive(&plan.root, &NodePath::root(), &predicate, &mut results);
        results
    }
    
    fn find_nodes_recursive<'a, F>(
        node: &'a PlanNode,
        path: &NodePath,
        predicate: &F,
        results: &mut Vec<(&'a PlanNode, NodePath)>,
    )
    where
        F: Fn(&PlanNode) -> bool,
    {
        if predicate(node) {
            results.push((node, path.clone()));
        }
        
        // Search children
        for (index, child) in node.children.iter().enumerate() {
            let child_path = path.child_of(index, child.description());
            Self::find_nodes_recursive(child, &child_path, predicate, results);
        }
    }
    
    /// Get all leaf nodes (nodes with no children)
    pub fn get_leaf_nodes(plan: &ParsedPlan) -> Vec<(&PlanNode, NodePath)> {
        Self::find_nodes(plan, |node| node.children.is_empty())
    }
    
    /// Get all nodes at a specific depth level
    pub fn get_nodes_at_depth(plan: &ParsedPlan, target_depth: usize) -> Vec<(&PlanNode, NodePath)> {
        let mut results = Vec::new();
        Self::get_nodes_at_depth_recursive(&plan.root, &NodePath::root(), 0, target_depth, &mut results);
        results
    }
    
    fn get_nodes_at_depth_recursive<'a>(
        node: &'a PlanNode,
        path: &NodePath,
        current_depth: usize,
        target_depth: usize,
        results: &mut Vec<(&'a PlanNode, NodePath)>,
    ) {
        if current_depth == target_depth {
            results.push((node, path.clone()));
            return;
        }
        
        if current_depth < target_depth {
            for (index, child) in node.children.iter().enumerate() {
                let child_path = path.child_of(index, child.description());
                Self::get_nodes_at_depth_recursive(child, &child_path, current_depth + 1, target_depth, results);
            }
        }
    }
    
    /// Get the parent-child relationships in the plan
    pub fn get_parent_child_pairs(plan: &ParsedPlan) -> Vec<((&PlanNode, NodePath), (&PlanNode, NodePath))> {
        let mut results = Vec::new();
        Self::get_parent_child_pairs_recursive(&plan.root, &NodePath::root(), &mut results);
        results
    }
    
    fn get_parent_child_pairs_recursive<'a>(
        node: &'a PlanNode,
        path: &NodePath,
        results: &mut Vec<((&'a PlanNode, NodePath), (&'a PlanNode, NodePath))>,
    ) {
        for (index, child) in node.children.iter().enumerate() {
            let child_path = path.child_of(index, child.description());
            results.push(((node, path.clone()), (child, child_path.clone())));
            Self::get_parent_child_pairs_recursive(child, &child_path, results);
        }
    }
}

/// Helper struct for nodes that want to analyze relationships between nodes
pub struct NodeRelationshipAnalyzer<'a> {
    plan: &'a ParsedPlan,
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
            parent.children.iter()
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
    pub fn get_descendants<'b>(&self, node: &'b PlanNode) -> Vec<&'b PlanNode> {
        let mut descendants = Vec::new();
        for child in &node.children {
            descendants.push(child);
            descendants.extend(self.get_descendants(child));
        }
        descendants
    }
    
    /// Check if one node is an ancestor of another
    pub fn is_ancestor(&self, potential_ancestor_path: &NodePath, descendant_path: &NodePath) -> bool {
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
        self.metrics.push((path.clone(), "total_cost".to_string(), node.cost.total_cost()));
        self.metrics.push((path.clone(), "startup_cost".to_string(), node.cost.startup_cost));
        self.metrics.push((path.clone(), "estimated_rows".to_string(), node.cost.estimated_rows as f64));
        self.metrics.push((path.clone(), "estimated_width".to_string(), node.cost.estimated_width as f64));
        
        // Add actual metrics if available
        if let Some(actuals) = &node.actuals {
            if let Some(actual_time) = actuals.actual_time_ms {
                self.metrics.push((path.clone(), "actual_time_ms".to_string(), actual_time));
            }
            if let Some(actual_rows) = actuals.actual_rows {
                self.metrics.push((path.clone(), "actual_rows".to_string(), actual_rows as f64));
            }
            if let Some(actual_loops) = actuals.actual_loops {
                self.metrics.push((path.clone(), "actual_loops".to_string(), actual_loops as f64));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlanNode, NodeType, ScanType, PlanCost, ParsedPlan, PlanSourceFormat};
    
    fn create_test_plan() -> ParsedPlan {
        let cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 100.0,
            estimated_rows: 1000,
            estimated_width: 50,
        };
        
        let mut root = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan),
            cost.clone(),
            "Seq Scan on table".to_string(),
        );
        
        let child1 = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan),
            cost.clone(),
            "Index Scan".to_string(),
        );
        
        let child2 = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan),
            cost.clone(),
            "Index Scan".to_string(),
        );
        
        root.add_child(child1);
        root.add_child(child2);
        
        ParsedPlan::new(root, "test plan".to_string(), PlanSourceFormat::Text)
    }
    
    #[test]
    fn test_depth_first_traversal() {
        let plan = create_test_plan();
        let context = AnalysisContext::new();
        let mut visitor = CountingVisitor::new(|_| true);
        
        PlanTraversal::depth_first(&plan, &mut visitor, &context);
        
        assert_eq!(visitor.count, 3); // Root + 2 children
    }
    
    #[test]
    fn test_find_nodes() {
        let plan = create_test_plan();
        
        let index_scans = PlanTraversal::find_nodes(&plan, |node| {
            matches!(node.node_type, NodeType::Scan(ScanType::IndexScan))
        });
        
        assert_eq!(index_scans.len(), 2);
    }
    
    #[test]
    fn test_leaf_nodes() {
        let plan = create_test_plan();
        let leaf_nodes = PlanTraversal::get_leaf_nodes(&plan);
        
        assert_eq!(leaf_nodes.len(), 2); // Both children are leaves
    }
    
    #[test]
    fn test_nodes_at_depth() {
        let plan = create_test_plan();
        
        let depth_0 = PlanTraversal::get_nodes_at_depth(&plan, 0);
        let depth_1 = PlanTraversal::get_nodes_at_depth(&plan, 1);
        
        assert_eq!(depth_0.len(), 1); // Root only
        assert_eq!(depth_1.len(), 2); // Two children
    }
    
    #[test]
    fn test_relationship_analyzer() {
        let plan = create_test_plan();
        let context = AnalysisContext::new();
        let analyzer = NodeRelationshipAnalyzer::new(&plan, &context);
        
        let child_path = NodePath::root().child_of(0, "Child".to_string());
        let parent = analyzer.get_parent(&child_path);
        
        assert!(parent.is_some());
        
        let siblings = analyzer.get_siblings(&child_path);
        assert_eq!(siblings.len(), 1); // One sibling
    }
    
    #[test]
    fn test_metrics_collector() {
        let plan = create_test_plan();
        let context = AnalysisContext::new();
        let mut collector = MetricsCollector::new();
        
        PlanTraversal::depth_first(&plan, &mut collector, &context);
        
        // Should have metrics for all 3 nodes
        let total_cost_metrics: Vec<_> = collector.metrics.iter()
            .filter(|(_, name, _)| name == "total_cost")
            .collect();
        
        assert_eq!(total_cost_metrics.len(), 3);
    }
}
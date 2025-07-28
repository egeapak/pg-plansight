//! Query complexity analysis and scoring
//! 
//! This module provides sophisticated analysis of SQL query complexity,
//! including join complexity, subquery depth, function usage, and more.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    Expr, Function, JoinOperator, Query, Select, SelectItem, SetExpr, Statement,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use std::collections::HashSet;

/// Overall complexity score and breakdown
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityScore {
    /// Overall complexity score (0-100)
    pub total_score: f64,
    /// Individual component scores
    pub components: ComplexityComponents,
    /// Complexity classification
    pub classification: ComplexityClass,
    /// Detailed breakdown for analysis
    pub breakdown: ComplexityBreakdown,
}

/// Individual complexity component scores
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityComponents {
    /// Join complexity (0-25)
    pub join_complexity: f64,
    /// Subquery complexity (0-20)
    pub subquery_complexity: f64,
    /// Function complexity (0-15)
    pub function_complexity: f64,
    /// Condition complexity (0-15)
    pub condition_complexity: f64,
    /// Aggregation complexity (0-10)
    pub aggregation_complexity: f64,
    /// Window function complexity (0-10)
    pub window_complexity: f64,
    /// CTE complexity (0-5)
    pub cte_complexity: f64,
}

/// Complexity classification levels
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ComplexityClass {
    Simple,      // 0-25
    Moderate,    // 26-50
    Complex,     // 51-75
    VeryComplex, // 76-100
}

/// Detailed breakdown of complexity factors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityBreakdown {
    /// Number of tables involved
    pub table_count: usize,
    /// Number and types of joins
    pub join_info: JoinInfo,
    /// Subquery information
    pub subquery_info: SubqueryInfo,
    /// Function usage
    pub function_info: FunctionInfo,
    /// Condition complexity details
    pub condition_info: ConditionInfo,
    /// Aggregation details
    pub aggregation_info: AggregationInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinInfo {
    pub total_joins: usize,
    pub inner_joins: usize,
    pub outer_joins: usize,
    pub cross_joins: usize,
    pub self_joins: usize,
    pub max_join_depth: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubqueryInfo {
    pub total_subqueries: usize,
    pub correlated_subqueries: usize,
    pub max_nesting_level: usize,
    pub exists_subqueries: usize,
    pub in_subqueries: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionInfo {
    pub total_functions: usize,
    pub aggregate_functions: usize,
    pub window_functions: usize,
    pub scalar_functions: usize,
    pub unique_functions: HashSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionInfo {
    pub where_conditions: usize,
    pub having_conditions: usize,
    pub join_conditions: usize,
    pub complex_expressions: usize,
    pub case_statements: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregationInfo {
    pub group_by_columns: usize,
    pub aggregate_functions: usize,
    pub having_clause: bool,
    pub distinct_aggregates: usize,
}

/// Query complexity analyzer
pub struct ComplexityAnalyzer {
    // Configuration for scoring weights
    join_weight: f64,
    subquery_weight: f64,
    function_weight: f64,
    condition_weight: f64,
    aggregation_weight: f64,
    window_weight: f64,
    cte_weight: f64,
}

impl Default for ComplexityAnalyzer {
    fn default() -> Self {
        Self {
            join_weight: 25.0,
            subquery_weight: 20.0,
            function_weight: 15.0,
            condition_weight: 15.0,
            aggregation_weight: 10.0,
            window_weight: 10.0,
            cte_weight: 5.0,
        }
    }
}

impl ComplexityAnalyzer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Analyze the complexity of a SQL query
    pub fn analyze(&self, sql: &str) -> Result<ComplexityScore> {
        let dialect = PostgreSqlDialect {};
        let statements = Parser::parse_sql(&dialect, sql)
            .with_context(|| format!("Failed to parse SQL for complexity analysis: {}", sql))?;

        if statements.is_empty() {
            return Ok(self.create_simple_score());
        }

        // Analyze the first statement (most common case)
        let breakdown = self.analyze_statement(&statements[0])?;
        let components = self.calculate_component_scores(&breakdown);
        let total_score = self.calculate_total_score(&components);
        let classification = self.classify_complexity(total_score);

        Ok(ComplexityScore {
            total_score,
            components,
            classification,
            breakdown,
        })
    }

    /// Analyze a single SQL statement
    fn analyze_statement(&self, statement: &Statement) -> Result<ComplexityBreakdown> {
        match statement {
            Statement::Query(query) => self.analyze_query(query),
            _ => Ok(self.create_simple_breakdown()),
        }
    }

    /// Analyze a query for complexity
    fn analyze_query(&self, query: &Query) -> Result<ComplexityBreakdown> {
        let mut breakdown = ComplexityBreakdown {
            table_count: 0,
            join_info: JoinInfo {
                total_joins: 0,
                inner_joins: 0,
                outer_joins: 0,
                cross_joins: 0,
                self_joins: 0,
                max_join_depth: 0,
            },
            subquery_info: SubqueryInfo {
                total_subqueries: 0,
                correlated_subqueries: 0,
                max_nesting_level: 0,
                exists_subqueries: 0,
                in_subqueries: 0,
            },
            function_info: FunctionInfo {
                total_functions: 0,
                aggregate_functions: 0,
                window_functions: 0,
                scalar_functions: 0,
                unique_functions: HashSet::new(),
            },
            condition_info: ConditionInfo {
                where_conditions: 0,
                having_conditions: 0,
                join_conditions: 0,
                complex_expressions: 0,
                case_statements: 0,
            },
            aggregation_info: AggregationInfo {
                group_by_columns: 0,
                aggregate_functions: 0,
                having_clause: false,
                distinct_aggregates: 0,
            },
        };

        self.analyze_query_body(&query.body, &mut breakdown, 1)?;
        Ok(breakdown)
    }

    /// Analyze query body recursively
    fn analyze_query_body(&self, body: &SetExpr, breakdown: &mut ComplexityBreakdown, depth: usize) -> Result<()> {
        breakdown.subquery_info.max_nesting_level = breakdown.subquery_info.max_nesting_level.max(depth);

        match body {
            SetExpr::Select(select) => {
                self.analyze_select(select, breakdown, depth)?;
            }
            SetExpr::Query(query) => {
                self.analyze_query_body(&query.body, breakdown, depth + 1)?;
            }
            SetExpr::SetOperation { left, right, .. } => {
                self.analyze_query_body(left, breakdown, depth)?;
                self.analyze_query_body(right, breakdown, depth)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Analyze SELECT statement
    fn analyze_select(&self, select: &Select, breakdown: &mut ComplexityBreakdown, depth: usize) -> Result<()> {
        // Count tables and analyze joins
        for table_with_joins in &select.from {
            breakdown.table_count += 1; // Count the main table
            breakdown.table_count += table_with_joins.joins.len(); // Count joined tables
            
            // Analyze joins
            self.analyze_table_joins(table_with_joins, breakdown)?;
        }

        // Analyze projections
        for item in &select.projection {
            self.analyze_select_item(item, breakdown, depth)?;
        }

        // Analyze WHERE clause
        if let Some(ref where_expr) = select.selection {
            breakdown.condition_info.where_conditions += self.count_conditions(where_expr);
            self.analyze_expression(where_expr, breakdown, depth + 1)?;
        }

        // Analyze GROUP BY
        breakdown.aggregation_info.group_by_columns = match &select.group_by {
            sqlparser::ast::GroupByExpr::All(_) => 1,
            sqlparser::ast::GroupByExpr::Expressions(exprs, _) => exprs.len(),
        };

        // Analyze HAVING clause
        if let Some(ref having_expr) = select.having {
            breakdown.aggregation_info.having_clause = true;
            breakdown.condition_info.having_conditions += self.count_conditions(having_expr);
            self.analyze_expression(having_expr, breakdown, depth + 1)?;
        }

        Ok(())
    }

    /// Analyze SELECT item (column, expression, etc.)
    fn analyze_select_item(&self, item: &SelectItem, breakdown: &mut ComplexityBreakdown, depth: usize) -> Result<()> {
        match item {
            SelectItem::UnnamedExpr(expr) => {
                self.analyze_expression(expr, breakdown, depth)?;
            }
            SelectItem::ExprWithAlias { expr, .. } => {
                self.analyze_expression(expr, breakdown, depth)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Analyze expression for complexity
    fn analyze_expression(&self, expr: &Expr, breakdown: &mut ComplexityBreakdown, depth: usize) -> Result<()> {
        match expr {
            Expr::Function(func) => {
                self.analyze_function(func, breakdown)?;
            }
            Expr::Subquery(query) => {
                breakdown.subquery_info.total_subqueries += 1;
                self.analyze_query_body(&query.body, breakdown, depth + 1)?;
            }
            Expr::Exists { subquery, .. } => {
                breakdown.subquery_info.exists_subqueries += 1;
                breakdown.subquery_info.total_subqueries += 1;
                self.analyze_query_body(&subquery.body, breakdown, depth + 1)?;
            }
            Expr::InSubquery { expr, subquery, .. } => {
                breakdown.subquery_info.in_subqueries += 1;
                breakdown.subquery_info.total_subqueries += 1;
                self.analyze_expression(expr, breakdown, depth)?;
                self.analyze_query_body(subquery, breakdown, depth + 1)?;
            }
            Expr::Case { conditions, else_result, .. } => {
                breakdown.condition_info.case_statements += 1;
                for case_when in conditions {
                    self.analyze_expression(&case_when.condition, breakdown, depth)?;
                    self.analyze_expression(&case_when.result, breakdown, depth)?;
                }
                if let Some(else_expr) = else_result {
                    self.analyze_expression(else_expr, breakdown, depth)?;
                }
            }
            Expr::BinaryOp { left, right, .. } => {
                self.analyze_expression(left, breakdown, depth)?;
                self.analyze_expression(right, breakdown, depth)?;
            }
            Expr::UnaryOp { expr, .. } => {
                self.analyze_expression(expr, breakdown, depth)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Analyze function for complexity
    fn analyze_function(&self, func: &Function, breakdown: &mut ComplexityBreakdown) -> Result<()> {
        breakdown.function_info.total_functions += 1;
        
        let func_name = func.name.to_string().to_lowercase();
        breakdown.function_info.unique_functions.insert(func_name.clone());

        // Classify function type
        if self.is_aggregate_function(&func_name) {
            breakdown.function_info.aggregate_functions += 1;
            breakdown.aggregation_info.aggregate_functions += 1;
        } else if self.is_window_function(&func_name) {
            breakdown.function_info.window_functions += 1;
        } else {
            breakdown.function_info.scalar_functions += 1;
        }

        // Analyze function arguments
        if let sqlparser::ast::FunctionArguments::List(args) = &func.args {
            for arg in &args.args {
                match arg {
                    sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(expr)) => {
                        self.analyze_expression(expr, breakdown, 0)?;
                    }
                    sqlparser::ast::FunctionArg::Named { arg: sqlparser::ast::FunctionArgExpr::Expr(expr), .. } => {
                        self.analyze_expression(expr, breakdown, 0)?;
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    /// Analyze joins in a table with joins
    fn analyze_table_joins(&self, table_with_joins: &sqlparser::ast::TableWithJoins, breakdown: &mut ComplexityBreakdown) -> Result<()> {
        breakdown.join_info.total_joins += table_with_joins.joins.len();
        
        for join in &table_with_joins.joins {
            match &join.join_operator {
                JoinOperator::Inner(_) => breakdown.join_info.inner_joins += 1,
                JoinOperator::LeftOuter(_) | JoinOperator::RightOuter(_) | JoinOperator::FullOuter(_) => {
                    breakdown.join_info.outer_joins += 1
                }
                JoinOperator::CrossJoin => breakdown.join_info.cross_joins += 1,
                _ => {}
            }

            // Count join conditions
            if let JoinOperator::Inner(constraint) | JoinOperator::LeftOuter(constraint) | 
               JoinOperator::RightOuter(constraint) | JoinOperator::FullOuter(constraint) = &join.join_operator {
                if let sqlparser::ast::JoinConstraint::On(expr) = constraint {
                    breakdown.condition_info.join_conditions += self.count_conditions(expr);
                }
            }
        }
        Ok(())
    }

    /// Count conditions in an expression
    fn count_conditions(&self, expr: &Expr) -> usize {
        match expr {
            Expr::BinaryOp { left, right, op } => {
                let mut count = 1; // This condition itself
                if matches!(op, sqlparser::ast::BinaryOperator::And | sqlparser::ast::BinaryOperator::Or) {
                    count += self.count_conditions(left);
                    count += self.count_conditions(right);
                }
                count
            }
            _ => 1,
        }
    }

    /// Check if function is an aggregate function
    fn is_aggregate_function(&self, func_name: &str) -> bool {
        matches!(
            func_name,
            "count" | "sum" | "avg" | "min" | "max" | "array_agg" | "string_agg"
                | "bool_and" | "bool_or" | "every" | "stddev" | "variance"
        )
    }

    /// Check if function is a window function
    fn is_window_function(&self, func_name: &str) -> bool {
        matches!(
            func_name,
            "row_number" | "rank" | "dense_rank" | "lag" | "lead" | "first_value" | "last_value"
                | "nth_value" | "percent_rank" | "cume_dist" | "ntile"
        )
    }

    /// Calculate component scores based on breakdown
    fn calculate_component_scores(&self, breakdown: &ComplexityBreakdown) -> ComplexityComponents {
        ComplexityComponents {
            join_complexity: self.calculate_join_score(&breakdown.join_info),
            subquery_complexity: self.calculate_subquery_score(&breakdown.subquery_info),
            function_complexity: self.calculate_function_score(&breakdown.function_info),
            condition_complexity: self.calculate_condition_score(&breakdown.condition_info),
            aggregation_complexity: self.calculate_aggregation_score(&breakdown.aggregation_info),
            window_complexity: self.calculate_window_score(&breakdown.function_info),
            cte_complexity: 0.0, // TODO: Implement CTE analysis
        }
    }

    /// Calculate join complexity score
    fn calculate_join_score(&self, join_info: &JoinInfo) -> f64 {
        let base_score = (join_info.total_joins as f64 * 3.0).min(15.0);
        let outer_join_penalty = join_info.outer_joins as f64 * 2.0;
        let cross_join_penalty = join_info.cross_joins as f64 * 3.0;
        
        (base_score + outer_join_penalty + cross_join_penalty).min(self.join_weight)
    }

    /// Calculate subquery complexity score
    fn calculate_subquery_score(&self, subquery_info: &SubqueryInfo) -> f64 {
        let base_score = (subquery_info.total_subqueries as f64 * 5.0).min(15.0);
        let nesting_penalty = (subquery_info.max_nesting_level as f64 * 2.0).min(5.0);
        
        (base_score + nesting_penalty).min(self.subquery_weight)
    }

    /// Calculate function complexity score
    fn calculate_function_score(&self, function_info: &FunctionInfo) -> f64 {
        let base_score = (function_info.total_functions as f64 * 1.5).min(10.0);
        let unique_penalty = (function_info.unique_functions.len() as f64 * 1.0).min(5.0);
        
        (base_score + unique_penalty).min(self.function_weight)
    }

    /// Calculate condition complexity score
    fn calculate_condition_score(&self, condition_info: &ConditionInfo) -> f64 {
        let total_conditions = condition_info.where_conditions + 
                             condition_info.having_conditions + 
                             condition_info.join_conditions;
        let base_score = (total_conditions as f64 * 1.5).min(10.0);
        let case_penalty = (condition_info.case_statements as f64 * 2.0).min(5.0);
        
        (base_score + case_penalty).min(self.condition_weight)
    }

    /// Calculate aggregation complexity score
    fn calculate_aggregation_score(&self, aggregation_info: &AggregationInfo) -> f64 {
        let base_score = (aggregation_info.group_by_columns as f64 * 2.0).min(6.0);
        let agg_score = (aggregation_info.aggregate_functions as f64 * 1.5).min(4.0);
        
        (base_score + agg_score).min(self.aggregation_weight)
    }

    /// Calculate window function complexity score
    fn calculate_window_score(&self, function_info: &FunctionInfo) -> f64 {
        (function_info.window_functions as f64 * 3.0).min(self.window_weight)
    }

    /// Calculate total complexity score
    fn calculate_total_score(&self, components: &ComplexityComponents) -> f64 {
        components.join_complexity
            + components.subquery_complexity
            + components.function_complexity
            + components.condition_complexity
            + components.aggregation_complexity
            + components.window_complexity
            + components.cte_complexity
    }

    /// Classify complexity based on total score
    fn classify_complexity(&self, score: f64) -> ComplexityClass {
        match score {
            s if s <= 25.0 => ComplexityClass::Simple,
            s if s <= 50.0 => ComplexityClass::Moderate,
            s if s <= 75.0 => ComplexityClass::Complex,
            _ => ComplexityClass::VeryComplex,
        }
    }

    /// Create a simple score for empty or invalid queries
    fn create_simple_score(&self) -> ComplexityScore {
        ComplexityScore {
            total_score: 0.0,
            components: ComplexityComponents {
                join_complexity: 0.0,
                subquery_complexity: 0.0,
                function_complexity: 0.0,
                condition_complexity: 0.0,
                aggregation_complexity: 0.0,
                window_complexity: 0.0,
                cte_complexity: 0.0,
            },
            classification: ComplexityClass::Simple,
            breakdown: self.create_simple_breakdown(),
        }
    }

    /// Create a simple breakdown for empty or invalid queries
    fn create_simple_breakdown(&self) -> ComplexityBreakdown {
        ComplexityBreakdown {
            table_count: 0,
            join_info: JoinInfo {
                total_joins: 0,
                inner_joins: 0,
                outer_joins: 0,
                cross_joins: 0,
                self_joins: 0,
                max_join_depth: 0,
            },
            subquery_info: SubqueryInfo {
                total_subqueries: 0,
                correlated_subqueries: 0,
                max_nesting_level: 0,
                exists_subqueries: 0,
                in_subqueries: 0,
            },
            function_info: FunctionInfo {
                total_functions: 0,
                aggregate_functions: 0,
                window_functions: 0,
                scalar_functions: 0,
                unique_functions: HashSet::new(),
            },
            condition_info: ConditionInfo {
                where_conditions: 0,
                having_conditions: 0,
                join_conditions: 0,
                complex_expressions: 0,
                case_statements: 0,
            },
            aggregation_info: AggregationInfo {
                group_by_columns: 0,
                aggregate_functions: 0,
                having_clause: false,
                distinct_aggregates: 0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_query_complexity() {
        let analyzer = ComplexityAnalyzer::new();
        let result = analyzer.analyze("SELECT id, name FROM users").unwrap();
        
        assert_eq!(result.classification, ComplexityClass::Simple);
        assert!(result.total_score < 10.0);
        assert_eq!(result.breakdown.table_count, 1);
        assert_eq!(result.breakdown.join_info.total_joins, 0);
    }

    #[test]
    fn test_complex_join_query() {
        let analyzer = ComplexityAnalyzer::new();
        let sql = r#"
            SELECT u.name, o.total, p.name as product_name
            FROM users u 
            INNER JOIN orders o ON u.id = o.user_id
            LEFT JOIN order_items oi ON o.id = oi.order_id
            LEFT JOIN products p ON oi.product_id = p.id
            WHERE u.active = true 
              AND o.status = 'completed'
              AND o.created_at > '2024-01-01'
        "#;
        let result = analyzer.analyze(sql).unwrap();
        
        assert!(result.classification != ComplexityClass::Simple);
        assert!(result.components.join_complexity > 5.0);
        assert_eq!(result.breakdown.join_info.total_joins, 3);
        assert_eq!(result.breakdown.join_info.inner_joins, 1);
        assert_eq!(result.breakdown.join_info.outer_joins, 2);
    }

    #[test]
    fn test_subquery_complexity() {
        let analyzer = ComplexityAnalyzer::new();
        let sql = r#"
            SELECT * FROM users u 
            WHERE EXISTS (
                SELECT 1 FROM orders o 
                WHERE o.user_id = u.id 
                  AND o.total > (SELECT AVG(total) FROM orders)
            )
        "#;
        let result = analyzer.analyze(sql).unwrap();
        
        assert!(result.components.subquery_complexity > 5.0);
        assert_eq!(result.breakdown.subquery_info.total_subqueries, 2);
        assert_eq!(result.breakdown.subquery_info.exists_subqueries, 1);
        assert!(result.breakdown.subquery_info.max_nesting_level >= 2);
    }

    #[test]
    fn test_function_complexity() {
        let analyzer = ComplexityAnalyzer::new();
        let sql = r#"
            SELECT 
                COUNT(*) as total_orders,
                AVG(total) as avg_total,
                CASE 
                    WHEN total > 1000 THEN 'high'
                    WHEN total > 100 THEN 'medium'
                    ELSE 'low'
                END as category
            FROM orders 
            GROUP BY user_id
            HAVING COUNT(*) > 5
        "#;
        let result = analyzer.analyze(sql).unwrap();
        
        assert!(result.components.function_complexity > 0.0);
        assert!(result.components.aggregation_complexity > 0.0);
        assert_eq!(result.breakdown.function_info.aggregate_functions, 2);
        assert_eq!(result.breakdown.condition_info.case_statements, 1);
        assert!(result.breakdown.aggregation_info.having_clause);
    }

    #[test]
    fn test_very_complex_query() {
        let analyzer = ComplexityAnalyzer::new();
        let sql = r#"
            WITH monthly_sales AS (
                SELECT 
                    user_id,
                    DATE_TRUNC('month', created_at) as month,
                    SUM(total) as monthly_total,
                    COUNT(*) as order_count,
                    ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY SUM(total) DESC) as rank
                FROM orders o
                WHERE EXISTS (
                    SELECT 1 FROM order_items oi 
                    JOIN products p ON oi.product_id = p.id
                    WHERE oi.order_id = o.id AND p.category = 'electronics'
                )
                GROUP BY user_id, DATE_TRUNC('month', created_at)
            )
            SELECT 
                u.name,
                ms.monthly_total,
                CASE 
                    WHEN ms.rank = 1 THEN 'top_customer'
                    WHEN ms.rank <= 5 THEN 'high_value'
                    ELSE 'regular'
                END as customer_tier,
                LAG(ms.monthly_total) OVER (PARTITION BY ms.user_id ORDER BY ms.month) as prev_month_total
            FROM monthly_sales ms
            JOIN users u ON ms.user_id = u.id
            WHERE ms.rank <= 10
              AND ms.monthly_total > (
                  SELECT AVG(monthly_total) * 1.5 
                  FROM monthly_sales 
                  WHERE month = ms.month
              )
            ORDER BY ms.monthly_total DESC
        "#;
        let result = analyzer.analyze(sql).unwrap();
        
        // This should be classified as complex or very complex
        assert!(matches!(result.classification, ComplexityClass::Complex | ComplexityClass::VeryComplex));
        assert!(result.total_score > 50.0);
        
        // Should have high scores in multiple categories
        assert!(result.components.join_complexity > 0.0);
        assert!(result.components.subquery_complexity > 0.0);
        assert!(result.components.function_complexity > 0.0);
        assert!(result.components.window_complexity > 0.0);
    }
}
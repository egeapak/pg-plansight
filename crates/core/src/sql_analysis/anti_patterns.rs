//! SQL anti-pattern detection on raw query text.
//!
//! Pure, self-contained analyzers that flag query-text smells the plan-only
//! analyzers cannot see, each with a concrete rewrite suggestion. Operates on
//! the SQL string via the `sqlparser` AST; on parse failure it returns no
//! findings rather than erroring.

use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    BinaryOperator, Expr, FunctionArguments, LimitClause, Query, Select, SelectItem, SetExpr,
    SetOperator, SetQuantifier, Statement, TableFactor, Value,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

/// Severity of a detected anti-pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AntiPatternSeverity {
    Low,
    Medium,
    High,
}

/// The kind of anti-pattern detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AntiPatternKind {
    SelectStar,
    LeadingWildcardLike,
    FunctionWrappedPredicate,
    NotIn,
    OffsetWithoutLimit,
    UnionInsteadOfUnionAll,
    CorrelatedSubquery,
}

/// A single detected anti-pattern with an explanation and a rewrite suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AntiPattern {
    pub kind: AntiPatternKind,
    pub severity: AntiPatternSeverity,
    /// What was found and why it is a problem.
    pub message: String,
    /// The concrete rewrite.
    pub suggestion: String,
}

/// Analyzer for SQL text anti-patterns.
pub struct AntiPatternAnalyzer {
    /// OFFSET values at or above this count are flagged as "deep" pagination.
    /// (Any OFFSET without a LIMIT is always flagged regardless of this value.)
    large_offset_threshold: u64,
}

impl Default for AntiPatternAnalyzer {
    fn default() -> Self {
        Self {
            large_offset_threshold: 1000,
        }
    }
}

impl AntiPatternAnalyzer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Analyze a SQL string and return all detected anti-patterns.
    /// Returns an empty `Vec` when the SQL cannot be parsed.
    pub fn analyze(&self, sql: &str) -> Vec<AntiPattern> {
        let dialect = PostgreSqlDialect {};
        let statements = match Parser::parse_sql(&dialect, sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(), // robustness: never propagate parse errors
        };

        let mut ctx = FindingCtx::default();
        for stmt in &statements {
            if let Statement::Query(query) = stmt {
                self.walk_query(query, &mut ctx, &[]);
            }
        }
        ctx.into_findings()
    }

    fn walk_query(&self, query: &Query, ctx: &mut FindingCtx, outer: &[String]) {
        self.check_offset(query, ctx);
        self.walk_set_expr(&query.body, ctx, outer);
    }

    fn walk_set_expr(&self, set_expr: &SetExpr, ctx: &mut FindingCtx, outer: &[String]) {
        match set_expr {
            SetExpr::Select(select) => self.walk_select(select, ctx, outer),
            SetExpr::Query(query) => self.walk_query(query, ctx, outer),
            SetExpr::SetOperation {
                op,
                set_quantifier,
                left,
                right,
            } => {
                if matches!(op, SetOperator::Union)
                    && !matches!(
                        set_quantifier,
                        SetQuantifier::All | SetQuantifier::AllByName
                    )
                {
                    ctx.push(
                        "union",
                        AntiPattern {
                            kind: AntiPatternKind::UnionInsteadOfUnionAll,
                            severity: AntiPatternSeverity::Low,
                            message: "UNION performs a deduplicating sort/hash, which is wasted \
                                      work when the inputs cannot overlap."
                                .to_string(),
                            suggestion: "Use UNION ALL when duplicates are impossible to skip the \
                                         dedup step."
                                .to_string(),
                        },
                    );
                }
                self.walk_set_expr(left, ctx, outer);
                self.walk_set_expr(right, ctx, outer);
            }
            _ => {}
        }
    }

    fn walk_select(&self, select: &Select, ctx: &mut FindingCtx, outer: &[String]) {
        // Detector 1: SELECT *
        if select.projection.iter().any(|item| {
            matches!(
                item,
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _)
            )
        }) {
            ctx.push(
                "projection",
                AntiPattern {
                    kind: AntiPatternKind::SelectStar,
                    severity: AntiPatternSeverity::Low,
                    message: "SELECT * fetches every column.".to_string(),
                    suggestion: "List only the columns you need to cut I/O and enable index-only \
                                 scans."
                        .to_string(),
                },
            );
        }

        // Aliases defined in this SELECT's FROM/JOIN clause.
        let mut scope: Vec<String> = Vec::new();
        for twj in &select.from {
            collect_alias(&twj.relation, &mut scope);
            for join in &twj.joins {
                collect_alias(&join.relation, &mut scope);
            }
        }

        // Inner subqueries see this scope plus any enclosing scopes as "outer".
        let mut outer_for_children: Vec<String> = outer.to_vec();
        outer_for_children.extend(scope.iter().cloned());

        // Walk predicate-bearing expressions.
        if let Some(sel) = &select.selection {
            self.walk_expr(sel, ctx, outer, &scope, &outer_for_children);
        }
        if let Some(having) = &select.having {
            self.walk_expr(having, ctx, outer, &scope, &outer_for_children);
        }
        for item in &select.projection {
            match item {
                SelectItem::UnnamedExpr(e) => {
                    self.walk_expr(e, ctx, outer, &scope, &outer_for_children)
                }
                SelectItem::ExprWithAlias { expr, .. } => {
                    self.walk_expr(expr, ctx, outer, &scope, &outer_for_children)
                }
                _ => {}
            }
        }
        // Derived tables (subqueries in FROM) are their own queries.
        for twj in &select.from {
            self.walk_table_factor(&twj.relation, ctx, &outer_for_children);
            for join in &twj.joins {
                self.walk_table_factor(&join.relation, ctx, &outer_for_children);
            }
        }
    }

    fn walk_table_factor(&self, tf: &TableFactor, ctx: &mut FindingCtx, outer: &[String]) {
        if let TableFactor::Derived { subquery, .. } = tf {
            self.walk_query(subquery, ctx, outer);
        }
    }

    /// Recursively inspect an expression.
    ///
    /// `outer` are aliases from enclosing query scopes (for correlated-subquery
    /// detection); `scope` are aliases of the current query; `outer_for_children`
    /// is `outer ∪ scope`, passed to any nested subqueries.
    fn walk_expr(
        &self,
        expr: &Expr,
        ctx: &mut FindingCtx,
        outer: &[String],
        scope: &[String],
        outer_for_children: &[String],
    ) {
        match expr {
            // Detector 7: correlated reference — a compound identifier whose
            // qualifier belongs to an enclosing scope and not the current one.
            Expr::CompoundIdentifier(parts) => {
                if let Some(first) = parts.first() {
                    let q = first.value.to_lowercase();
                    let in_outer = outer.iter().any(|a| a.to_lowercase() == q);
                    let in_scope = scope.iter().any(|a| a.to_lowercase() == q);
                    if in_outer && !in_scope {
                        ctx.push(
                            "correlated",
                            AntiPattern {
                                kind: AntiPatternKind::CorrelatedSubquery,
                                severity: AntiPatternSeverity::Low,
                                message: "Subquery references an outer table alias (correlated), \
                                          so it may re-evaluate per outer row."
                                    .to_string(),
                                suggestion: "Consider rewriting as a JOIN or a LATERAL subquery."
                                    .to_string(),
                            },
                        );
                    }
                }
            }
            // Detector 2: leading-wildcard LIKE/ILIKE.
            Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } => {
                if let Some(p) = string_literal(pattern)
                    && (p.starts_with('%') || p.starts_with('_'))
                {
                    ctx.push(
                        "like",
                        AntiPattern {
                            kind: AntiPatternKind::LeadingWildcardLike,
                            severity: AntiPatternSeverity::Medium,
                            message: "A LIKE pattern with a leading wildcard cannot use a b-tree \
                                      index."
                                .to_string(),
                            suggestion:
                                "Avoid a leading '%'/'_'; use a pg_trgm (trigram) index or \
                                         restructure the predicate."
                                    .to_string(),
                        },
                    );
                }
                self.walk_expr(expr, ctx, outer, scope, outer_for_children);
            }
            // Detector 4: NOT IN (list or subquery).
            Expr::InList { expr, negated, .. } => {
                if *negated {
                    ctx.push_not_in();
                }
                self.walk_expr(expr, ctx, outer, scope, outer_for_children);
            }
            Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => {
                if *negated {
                    ctx.push_not_in();
                }
                self.walk_expr(expr, ctx, outer, scope, outer_for_children);
                self.walk_set_expr(&subquery.body, ctx, outer_for_children);
            }
            // Detector 3: function/cast-wrapped column in a comparison.
            Expr::BinaryOp { left, op, right } => {
                if is_comparison(op) && (wraps_column(left) || wraps_column(right)) {
                    ctx.push(
                        "func_pred",
                        AntiPattern {
                            kind: AntiPatternKind::FunctionWrappedPredicate,
                            severity: AntiPatternSeverity::Medium,
                            message: "A column is wrapped in a function/cast in a comparison, \
                                      which prevents plain index use."
                                .to_string(),
                            suggestion:
                                "Create an expression index on the wrapped column, or move \
                                         the function to the constant side."
                                    .to_string(),
                        },
                    );
                }
                self.walk_expr(left, ctx, outer, scope, outer_for_children);
                self.walk_expr(right, ctx, outer, scope, outer_for_children);
            }
            Expr::UnaryOp { expr, .. } | Expr::Nested(expr) | Expr::Cast { expr, .. } => {
                self.walk_expr(expr, ctx, outer, scope, outer_for_children);
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                self.walk_expr(expr, ctx, outer, scope, outer_for_children);
                self.walk_expr(low, ctx, outer, scope, outer_for_children);
                self.walk_expr(high, ctx, outer, scope, outer_for_children);
            }
            Expr::Subquery(query) => self.walk_query(query, ctx, outer_for_children),
            Expr::Exists { subquery, .. } => self.walk_query(subquery, ctx, outer_for_children),
            _ => {}
        }
    }

    fn check_offset(&self, query: &Query, ctx: &mut FindingCtx) {
        if let Some(LimitClause::LimitOffset { limit, offset, .. }) = &query.limit_clause
            && let Some(off) = offset
        {
            let no_limit = limit.is_none();
            let deep = offset_value(&off.value)
                .map(|n| n >= self.large_offset_threshold)
                .unwrap_or(false);
            if no_limit || deep {
                ctx.push(
                    "offset",
                    AntiPattern {
                        kind: AntiPatternKind::OffsetWithoutLimit,
                        severity: AntiPatternSeverity::Medium,
                        message: "OFFSET-based pagination scans and discards rows; deep or \
                                  unbounded OFFSET is expensive."
                            .to_string(),
                        suggestion: "Use keyset (\"seek\") pagination: WHERE sort_key > $last \
                                     ORDER BY sort_key LIMIT n."
                            .to_string(),
                    },
                );
            }
        }
    }
}

/// Collects findings while de-duplicating by (kind, location).
#[derive(Default)]
struct FindingCtx {
    seen: std::collections::HashSet<(AntiPatternKind, String)>,
    findings: Vec<AntiPattern>,
}

impl FindingCtx {
    fn push(&mut self, location: &str, pattern: AntiPattern) {
        if self.seen.insert((pattern.kind, location.to_string())) {
            self.findings.push(pattern);
        }
    }

    fn push_not_in(&mut self) {
        self.push(
            "not_in",
            AntiPattern {
                kind: AntiPatternKind::NotIn,
                severity: AntiPatternSeverity::Medium,
                message: "NOT IN is NULL-unsafe and frequently produces poor plans.".to_string(),
                suggestion: "Prefer NOT EXISTS (or a LEFT JOIN ... WHERE key IS NULL) for correct \
                             NULL handling and better plans."
                    .to_string(),
            },
        );
    }

    fn into_findings(self) -> Vec<AntiPattern> {
        self.findings
    }
}

fn string_literal(expr: &Expr) -> Option<&str> {
    if let Expr::Value(vws) = expr {
        match &vws.value {
            Value::SingleQuotedString(s)
            | Value::DoubleQuotedString(s)
            | Value::EscapedStringLiteral(s) => return Some(s.as_str()),
            _ => {}
        }
    }
    None
}

fn offset_value(expr: &Expr) -> Option<u64> {
    if let Expr::Value(vws) = expr
        && let Value::Number(s, _) = &vws.value
    {
        return s.parse::<u64>().ok();
    }
    None
}

fn is_comparison(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
    )
}

/// True when `expr` is a function call or cast that contains a column reference
/// (the canonical "non-sargable predicate" shape, e.g. `lower(col)`, `col::text`).
fn wraps_column(expr: &Expr) -> bool {
    match expr {
        Expr::Cast { expr, .. } => contains_column(expr),
        Expr::Function(f) => match &f.args {
            FunctionArguments::List(list) => list.args.iter().any(|arg| {
                use sqlparser::ast::{FunctionArg, FunctionArgExpr};
                matches!(
                    arg,
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                        | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. }
                        if contains_column(e)
                )
            }),
            _ => false,
        },
        _ => false,
    }
}

fn contains_column(expr: &Expr) -> bool {
    match expr {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        Expr::Cast { expr, .. } | Expr::Nested(expr) | Expr::UnaryOp { expr, .. } => {
            contains_column(expr)
        }
        _ => false,
    }
}

fn collect_alias(tf: &TableFactor, out: &mut Vec<String>) {
    let alias = match tf {
        TableFactor::Table { alias, .. } => alias.as_ref(),
        TableFactor::Derived { alias, .. } => alias.as_ref(),
        _ => None,
    };
    if let Some(a) = alias {
        out.push(a.name.value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str) -> Vec<AntiPatternKind> {
        AntiPatternAnalyzer::new()
            .analyze(sql)
            .into_iter()
            .map(|p| p.kind)
            .collect()
    }

    #[test]
    fn test_select_star_detected() {
        assert!(kinds("SELECT * FROM users").contains(&AntiPatternKind::SelectStar));
    }

    #[test]
    fn test_explicit_columns_no_finding() {
        assert!(!kinds("SELECT id, name FROM users").contains(&AntiPatternKind::SelectStar));
    }

    #[test]
    fn test_leading_wildcard_like_detected() {
        assert!(
            kinds("SELECT id FROM users WHERE name LIKE '%abc'")
                .contains(&AntiPatternKind::LeadingWildcardLike)
        );
    }

    #[test]
    fn test_trailing_wildcard_ok() {
        assert!(
            !kinds("SELECT id FROM users WHERE name LIKE 'abc%'")
                .contains(&AntiPatternKind::LeadingWildcardLike)
        );
    }

    #[test]
    fn test_function_wrapped_predicate_detected() {
        assert!(
            kinds("SELECT id FROM users WHERE lower(email) = 'a@b.com'")
                .contains(&AntiPatternKind::FunctionWrappedPredicate)
        );
    }

    #[test]
    fn test_cast_wrapped_predicate_detected() {
        assert!(
            kinds("SELECT id FROM users WHERE id::text = '1'")
                .contains(&AntiPatternKind::FunctionWrappedPredicate)
        );
    }

    #[test]
    fn test_plain_predicate_ok() {
        assert!(
            !kinds("SELECT id FROM users WHERE email = 'a@b.com'")
                .contains(&AntiPatternKind::FunctionWrappedPredicate)
        );
    }

    #[test]
    fn test_not_in_detected() {
        assert!(
            kinds("SELECT id FROM users WHERE id NOT IN (1, 2, 3)")
                .contains(&AntiPatternKind::NotIn)
        );
    }

    #[test]
    fn test_not_in_subquery_detected() {
        assert!(
            kinds("SELECT id FROM a WHERE id NOT IN (SELECT id FROM b)")
                .contains(&AntiPatternKind::NotIn)
        );
    }

    #[test]
    fn test_in_ok() {
        assert!(
            !kinds("SELECT id FROM users WHERE id IN (1, 2, 3)").contains(&AntiPatternKind::NotIn)
        );
    }

    #[test]
    fn test_offset_without_limit_detected() {
        assert!(
            kinds("SELECT id FROM users ORDER BY id OFFSET 50")
                .contains(&AntiPatternKind::OffsetWithoutLimit)
        );
    }

    #[test]
    fn test_offset_with_limit_ok() {
        assert!(
            !kinds("SELECT id FROM users ORDER BY id LIMIT 10 OFFSET 50")
                .contains(&AntiPatternKind::OffsetWithoutLimit)
        );
    }

    #[test]
    fn test_union_detected() {
        assert!(
            kinds("SELECT id FROM a UNION SELECT id FROM b")
                .contains(&AntiPatternKind::UnionInsteadOfUnionAll)
        );
    }

    #[test]
    fn test_union_all_ok() {
        assert!(
            !kinds("SELECT id FROM a UNION ALL SELECT id FROM b")
                .contains(&AntiPatternKind::UnionInsteadOfUnionAll)
        );
    }

    #[test]
    fn test_correlated_subquery_detected() {
        assert!(
            kinds("SELECT id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id)")
                .contains(&AntiPatternKind::CorrelatedSubquery)
        );
    }

    #[test]
    fn test_non_correlated_subquery_ok() {
        assert!(
            !kinds("SELECT id FROM users WHERE id IN (SELECT user_id FROM orders)")
                .contains(&AntiPatternKind::CorrelatedSubquery)
        );
    }

    #[test]
    fn test_unparseable_sql_returns_empty() {
        assert!(
            AntiPatternAnalyzer::new()
                .analyze("this is not valid sql @@@ ;;;")
                .is_empty()
        );
    }
}

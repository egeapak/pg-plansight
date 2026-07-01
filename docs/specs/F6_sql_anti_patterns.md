# F6 — AntiPatternAnalyzer (SQL text)

## Goal
Detect query-text anti-patterns that the plan-only analyzers can't see directly,
each with a concrete rewrite. Operates on the SQL string + `sqlparser` AST
(`sqlparser` with the `visitor` feature is already a dependency).

## File
`crates/core/src/sql_analysis/anti_patterns.rs`, exported from
`sql_analysis/mod.rs` (`pub mod anti_patterns; pub use ...`).

Public API (self-contained, unit-testable, no plan needed):
```rust
pub struct AntiPattern {
    pub kind: AntiPatternKind,
    pub severity: AntiPatternSeverity, // Low | Medium | High
    pub message: String,               // what & why
    pub suggestion: String,            // the rewrite
}
pub enum AntiPatternKind {
    SelectStar, LeadingWildcardLike, FunctionWrappedPredicate,
    NotIn, OffsetWithoutLimit, UnionInsteadOfUnionAll, CorrelatedSubquery,
}
pub struct AntiPatternAnalyzer { /* config thresholds */ }
impl AntiPatternAnalyzer {
    pub fn new() -> Self;
    pub fn analyze(&self, sql: &str) -> Vec<AntiPattern>;
}
```

## Detection (prefer AST; fall back to tokenized/regex where AST is awkward)
Parse with `sqlparser` using `GenericDialect` (or PostgreSQL dialect). On parse
failure, return `vec![]` (don't error). Detectors:

1. **SelectStar** — any `SelectItem::Wildcard` / `QualifiedWildcard`.
   suggestion: list only needed columns to cut I/O and enable index-only scans.
2. **LeadingWildcardLike** — `Expr::Like` whose pattern string literal starts
   with `%` (or `_`). suggestion: leading wildcard can't use a b-tree index;
   consider trigram (`pg_trgm`) index or restructure.
3. **FunctionWrappedPredicate** — in a `WHERE`/`JOIN ON` comparison, a side is
   `Expr::Function`/`Cast` wrapping a column (`lower(col) = ...`,
   `col::text = ...`). suggestion: this prevents plain index use; use an
   expression index or move the function to the constant side.
4. **NotIn** — `Expr::InList { negated: true, .. }` or `InSubquery` negated.
   suggestion: `NOT IN` is NULL-unsafe and often slow; prefer `NOT EXISTS`.
5. **OffsetWithoutLimit** — query has `OFFSET` but no `LIMIT` (or large OFFSET).
   suggestion: deep offset pagination scans+discards; use keyset pagination.
6. **UnionInsteadOfUnionAll** — `SetOperator::Union` with `quantifier != All`.
   suggestion: if duplicates are impossible, use `UNION ALL` to skip the
   dedup sort.
7. **CorrelatedSubquery** — a subquery in `WHERE`/`SELECT` that references an
   outer table alias. (Heuristic; Low severity.) suggestion: consider a join or
   `LATERAL`.

Keep each detector small and independently testable. Avoid duplicate findings
for the same kind (dedup by kind + location text).

## Optional wiring
Expose via `pg_plansight_core` public API. Surfacing in `ProcessedQuery` / TUI is
out of scope for this feature (kept as a pure, public, tested module so it is
not dead code in the library).

## Tests (positive + negative per detector)
- `test_select_star_detected` / `test_explicit_columns_no_finding`.
- `test_leading_wildcard_like_detected` / `test_trailing_wildcard_ok`
  (`LIKE 'abc%'` → none).
- `test_function_wrapped_predicate_detected` / `test_plain_predicate_ok`.
- `test_not_in_detected` / `test_in_ok`.
- `test_offset_without_limit_detected` / `test_offset_with_limit_ok`.
- `test_union_detected` / `test_union_all_ok`.
- `test_unparseable_sql_returns_empty` (negative/robustness).

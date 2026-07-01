# F6 Implementation Plan — `AntiPatternAnalyzer` (SQL text)

Spec: `docs/specs/F6_sql_anti_patterns.md`
Target file: `crates/core/src/sql_analysis/anti_patterns.rs`

This plan is precise to **sqlparser 0.57.0** (confirmed in `crates/core/Cargo.toml`:
`sqlparser = { workspace = true, features = ["visitor"] }`). All AST enum/struct
shapes below were verified by reading
`~/.cargo/registry/.../sqlparser-0.57.0/src/ast/{mod.rs,query.rs,value.rs}`.

> **Spec correction (important):** The spec mentions `Query.offset` / `Query.limit`.
> In sqlparser **0.57** those fields do **not** exist. `Query` has a single field
> `limit_clause: Option<LimitClause>`, where
> `LimitClause::LimitOffset { limit: Option<Expr>, offset: Option<Offset>, limit_by: Vec<Expr> }`
> and `Offset { value: Expr, rows: OffsetRows }`. The `OffsetWithoutLimit`
> detector is written against `limit_clause` below.

---

## 1. Dialect & robust parsing

Follow the existing pattern in `complexity.rs` / `normalization.rs`:

```rust
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
```

- Parse with `PostgreSqlDialect {}` (consistent with the rest of `sql_analysis`).
- `analyze` must **never error**. On parse failure (or empty statement list) it
  returns `vec![]`:

```rust
pub fn analyze(&self, sql: &str) -> Vec<AntiPattern> {
    let dialect = PostgreSqlDialect {};
    let statements = match Parser::parse_sql(&dialect, sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(), // robustness: never propagate parse errors
    };
    let mut findings: Vec<AntiPattern> = Vec::new();
    for stmt in &statements {
        if let Statement::Query(query) = stmt {
            self.analyze_query(query, &mut findings, /* outer_aliases */ &[]);
        }
    }
    self.dedup(&mut findings); // dedup by (kind + location text)
    findings
}
```

Recursion model (mirror `complexity.rs`): a `analyze_query` → `analyze_set_expr`
→ `analyze_select` → `analyze_expr` walk that threads a `&mut Vec<AntiPattern>`
and a slice of outer table aliases (for the correlated-subquery heuristic).

---

## 2. Full public API (write verbatim)

```rust
//! SQL anti-pattern detection on raw query text.
//!
//! Pure, self-contained analyzers that flag query-text smells the plan-only
//! analyzers cannot see, each with a concrete rewrite suggestion. Operates on
//! the SQL string via the `sqlparser` AST; on parse failure it returns no
//! findings rather than erroring.

use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    Expr, LimitClause, Query, Select, SelectItem, SetExpr, SetOperator, SetQuantifier, Statement,
    TableFactor, Value,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
        // see §1
    }
}
```

Notes:
- Derive `Serialize`/`Deserialize` on all public types (consistent with the rest
  of `sql_analysis`; lets findings be JSON-exported later without churn).
- `AntiPattern` derives `PartialEq, Eq` to make test assertions and dedup easy.
- Keep `large_offset_threshold` as the only config field; `new()` delegates to
  `Default` (matches `ComplexityAnalyzer::new`).

---

## 3. Per-detector AST matching (sqlparser 0.57, verified)

A small helper builds findings and the walk routes each node to the relevant
checks. Detectors below are listed with the **exact** verified AST shapes.

### Verified AST reference (0.57)

```text
SelectItem::Wildcard(WildcardAdditionalOptions)
SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind, WildcardAdditionalOptions)
SelectItem::UnnamedExpr(Expr)
SelectItem::ExprWithAlias { expr: Expr, alias: Ident }

Expr::Like  { negated: bool, any: bool, expr: Box<Expr>, pattern: Box<Expr>, escape_char: Option<String> }
Expr::ILike { negated: bool, any: bool, expr: Box<Expr>, pattern: Box<Expr>, escape_char: Option<String> }
Expr::InList     { expr: Box<Expr>, list: Vec<Expr>, negated: bool }
Expr::InSubquery { expr: Box<Expr>, subquery: Box<SetExpr>, negated: bool }
Expr::Cast { kind: CastKind, expr: Box<Expr>, data_type: DataType, format: Option<CastFormat> }
Expr::Function(Function)
Expr::Identifier(Ident)
Expr::CompoundIdentifier(Vec<Ident>)
Expr::BinaryOp { left: Box<Expr>, op: BinaryOperator, right: Box<Expr> }
Expr::Subquery(Box<Query>)
Expr::Value(ValueWithSpan)   // ValueWithSpan { value: Value, span: Span }
   Value::SingleQuotedString(String) | Value::DoubleQuotedString(String)
        | Value::EscapedStringLiteral(String) | Value::Number(String, bool) ...

Query { body: Box<SetExpr>, limit_clause: Option<LimitClause>, ... }
LimitClause::LimitOffset { limit: Option<Expr>, offset: Option<Offset>, limit_by: Vec<Expr> }
LimitClause::OffsetCommaLimit { offset: Expr, limit: Expr }
Offset { value: Expr, rows: OffsetRows }

SetExpr::Select(Box<Select>)
SetExpr::Query(Box<Query>)
SetExpr::SetOperation { op: SetOperator, set_quantifier: SetQuantifier, left: Box<SetExpr>, right: Box<SetExpr> }
SetOperator::{ Union, Except, Intersect, Minus }
SetQuantifier::{ All, Distinct, ByName, AllByName, DistinctByName, None }

Select { projection: Vec<SelectItem>, from: Vec<TableWithJoins>, selection: Option<Expr>, having: Option<Expr>, group_by: GroupByExpr, ... }
TableWithJoins { relation: TableFactor, joins: Vec<Join> }
TableFactor::Table { name: ObjectName, alias: Option<TableAlias>, ... }   // alias.name: Ident
TableFactor::Derived { subquery: Box<Query>, alias: Option<TableAlias>, ... }
Join { relation: TableFactor, join_operator: JoinOperator, ... }
JoinOperator::Inner(JoinConstraint) | Left(..) | Right(..) | LeftOuter(..) | RightOuter(..) | FullOuter(..) | CrossJoin
JoinConstraint::On(Expr) | Using(...) | Natural | None
```

---

### Detector 1 — `SelectStar`  (severity: Low)

**Match:** in `Select.projection`, any
`SelectItem::Wildcard(_)` or `SelectItem::QualifiedWildcard(_, _)`.

```rust
for item in &select.projection {
    if matches!(item, SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _)) {
        push(SelectStar, Low,
             "SELECT * fetches all columns",
             "List only the columns you need to reduce I/O and enable index-only scans.");
        break; // one finding per SELECT (dedup)
    }
}
```

- Message location key for dedup: `"projection"`.

### Detector 2 — `LeadingWildcardLike`  (severity: Medium)

**Match:** `Expr::Like { pattern, .. }` **or** `Expr::ILike { pattern, .. }`
where `pattern` is a string literal beginning with `%` or `_`.

The pattern is `Box<Expr>`; unwrap to a string literal:

```rust
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
```

```rust
Expr::Like { pattern, .. } | Expr::ILike { pattern, .. } => {
    if let Some(p) = string_literal(pattern) {
        if p.starts_with('%') || p.starts_with('_') {
            push(LeadingWildcardLike, Medium,
                 "LIKE pattern with a leading wildcard cannot use a b-tree index",
                 "Avoid a leading '%'/'_'; use a pg_trgm (trigram) index or restructure the predicate.");
        }
    }
}
```

- Recurse into `expr` (the LHS) as well to catch nested predicates.

### Detector 3 — `FunctionWrappedPredicate`  (severity: Medium)

**Match:** inside a comparison `Expr::BinaryOp { left, op, right }` where `op` is a
comparison operator (`Eq, NotEq, Lt, LtEq, Gt, GtEq`), one side is a
column reference wrapped in a function or a cast, i.e.
`Expr::Function(_)` whose argument contains a column, or
`Expr::Cast { expr, .. }` whose inner `expr` is an
`Expr::Identifier(_)` / `Expr::CompoundIdentifier(_)`.

Heuristic helper:

```rust
fn wraps_column(expr: &Expr) -> bool {
    match expr {
        Expr::Cast { expr, .. } => contains_column(expr),
        Expr::Function(f) => function_args_contain_column(f), // walk FunctionArguments::List
        _ => false,
    }
}
fn contains_column(expr: &Expr) -> bool {
    matches!(expr, Expr::Identifier(_) | Expr::CompoundIdentifier(_))
        // plus shallow recursion into Cast/Function args
}
```

Walk `FunctionArguments::List(args)` → `FunctionArg::Unnamed(FunctionArgExpr::Expr(e))`
(same traversal already used in `complexity.rs::analyze_function`) and check
`contains_column(e)`.

```rust
Expr::BinaryOp { left, op, right } if is_comparison(op) => {
    if wraps_column(left) || wraps_column(right) {
        push(FunctionWrappedPredicate, Medium,
             "A column is wrapped in a function/cast in a comparison, which prevents plain index use",
             "Create an expression index on the wrapped column, or move the function to the constant side.");
    }
    // still recurse into both sides
}
```

`is_comparison` matches `BinaryOperator::{Eq, NotEq, Lt, LtEq, Gt, GtEq}`.

> Fallback note: matching "a column inside a function" is inherently heuristic.
> The AST approach above is concrete and sufficient for the test cases
> (`lower(col) = 'x'`, `col::text = '1'`). No regex fallback needed.

### Detector 4 — `NotIn`  (severity: Medium)

**Match:** `Expr::InList { negated: true, .. }` **or**
`Expr::InSubquery { negated: true, .. }`.

```rust
Expr::InList { negated: true, .. } | Expr::InSubquery { negated: true, .. } => {
    push(NotIn, Medium,
         "NOT IN is NULL-unsafe and frequently produces poor plans",
         "Prefer NOT EXISTS (or a LEFT JOIN ... WHERE key IS NULL) for correct NULL handling and better plans.");
}
```

For `InSubquery` also recurse into `subquery` (it's a `Box<SetExpr>`).

### Detector 5 — `OffsetWithoutLimit`  (severity: Medium)

**Match:** at the `Query` level, inspect `query.limit_clause`:

```rust
fn check_offset(&self, query: &Query, out: &mut Vec<AntiPattern>) {
    if let Some(LimitClause::LimitOffset { limit, offset, .. }) = &query.limit_clause {
        if let Some(off) = offset {
            let no_limit = limit.is_none();
            let deep = offset_value(off) // parse Offset.value if Expr::Value(Number)
                .map(|n| n >= self.large_offset_threshold)
                .unwrap_or(false);
            if no_limit || deep {
                push(OffsetWithoutLimit, Medium,
                     "OFFSET-based pagination scans and discards rows; deep or unbounded OFFSET is expensive",
                     "Use keyset (\"seek\") pagination: WHERE (sort_key) > $last ORDER BY sort_key LIMIT n.");
            }
        }
    }
    // OffsetCommaLimit always has both → not flagged.
}
```

`offset_value` reads `off.value` when it is `Expr::Value(ValueWithSpan { value: Value::Number(s, _), .. })` → `s.parse::<u64>().ok()`.

- The spec test `test_offset_with_limit_ok` uses a small OFFSET **with** a LIMIT;
  with default threshold 1000 and `limit.is_some()`, this yields no finding. Good.

### Detector 6 — `UnionInsteadOfUnionAll`  (severity: Low)

**Match:** `SetExpr::SetOperation { op: SetOperator::Union, set_quantifier, .. }`
where `set_quantifier != SetQuantifier::All`.

```rust
SetExpr::SetOperation { op: SetOperator::Union, set_quantifier, left, right } => {
    if !matches!(set_quantifier, SetQuantifier::All | SetQuantifier::AllByName) {
        push(UnionInsteadOfUnionAll, Low,
             "UNION performs a deduplicating sort/hash; this is wasted work when duplicates are impossible",
             "Use UNION ALL when the inputs cannot overlap to skip the dedup step.");
    }
    self.analyze_set_expr(left, ...);
    self.analyze_set_expr(right, ...);
}
```

- `UNION ALL` parses to `set_quantifier == SetQuantifier::All` → not flagged.
- Plain `UNION` parses to `SetQuantifier::Distinct` (or `None`) → flagged.

### Detector 7 — `CorrelatedSubquery`  (severity: Low, heuristic)

**Match:** a subquery (`Expr::Subquery`, `Expr::InSubquery.subquery`,
`Expr::Exists.subquery`, or a derived table) that references a table alias
defined in an **outer** query.

Approach:
1. When entering a `Select`, collect its table aliases from `select.from`:
   for each `TableWithJoins`, read `relation` and each `join.relation`; for
   `TableFactor::Table { alias: Some(a), .. }` / `TableFactor::Derived { alias, .. }`
   take `a.name.value` (the `Ident`). Push them onto the `outer_aliases` slice
   passed down to nested subqueries.
2. When walking a nested subquery's expressions, if any
   `Expr::CompoundIdentifier(parts)` has its first ident equal (case-insensitive)
   to an **outer** alias (and not one defined in the current/inner scope), flag
   `CorrelatedSubquery`.

```rust
push(CorrelatedSubquery, Low,
     "Subquery references an outer table alias (correlated), so it may re-evaluate per outer row",
     "Consider rewriting as a JOIN or a LATERAL subquery.");
```

> Fallback note: full scope resolution is non-trivial. This heuristic
> (outer-alias prefix referenced inside a nested subquery, where the inner scope
> does not redefine that alias) is intentionally conservative and Low severity,
> per the spec. Document this clearly in a code comment.

---

## 4. Dedup

After collecting, dedup by `(kind, location_text)`. Simplest robust approach:
build a `location` string per finding (e.g. `"projection"`, the offending
expression rendered via `expr.to_string()`, or the `Query` body string) and keep
the first finding per `(kind, location)` pair using a `HashSet<(AntiPatternKind, String)>`.
The public `AntiPattern` does not need to carry the location; dedup can run over
a parallel `Vec<(AntiPattern, String)>` internal buffer, or simply dedup by
`kind` alone for the simple cases the tests cover (one finding per kind).

Recommendation: keep an internal `Vec<(String /*loc*/, AntiPattern)>`, then
`retain` with a seen-set, then map to `Vec<AntiPattern>`.

---

## 5. Module wiring (make it public, not dead code)

### 5a. `crates/core/src/sql_analysis/mod.rs`

Add the module declaration alongside the existing ones and a `pub use` of the
public types (mirror the existing `pub use complexity::{...}` block):

```rust
pub mod anti_patterns;     // add near the other `pub mod` lines (after `pub mod` list)
```

```rust
pub use anti_patterns::{AntiPattern, AntiPatternAnalyzer, AntiPatternKind, AntiPatternSeverity};
```

### 5b. `crates/core/src/lib.rs`

Extend the existing `pub use sql_analysis::{ ... }` re-export block (lines 26–29)
so the types are part of the crate's public API:

```rust
pub use sql_analysis::{
    AntiPattern, AntiPatternAnalyzer, AntiPatternKind, AntiPatternSeverity, LiteralInfo,
    LiteralType, NormalizationResult, QueryNormalizer, calculate_query_fingerprint,
    normalize_query_enhanced,
};
```

This guarantees the module is reachable via `pg_plansight_core::AntiPatternAnalyzer`
and therefore not flagged as dead code.

---

## 6. Tests (in `anti_patterns.rs`, `#[cfg(test)] mod tests`)

Helper used by tests:

```rust
fn kinds(sql: &str) -> Vec<AntiPatternKind> {
    AntiPatternAnalyzer::new().analyze(sql).into_iter().map(|p| p.kind).collect()
}
```

One positive + one negative per detector, plus the unparseable-input test.
Exact SQL strings and expectations:

| Test fn | SQL | Expectation |
|---|---|---|
| `test_select_star_detected` | `SELECT * FROM users` | `kinds` contains `SelectStar` |
| `test_explicit_columns_no_finding` | `SELECT id, name FROM users` | `kinds` does **not** contain `SelectStar` (assert empty) |
| `test_leading_wildcard_like_detected` | `SELECT id FROM users WHERE name LIKE '%abc'` | contains `LeadingWildcardLike` |
| `test_trailing_wildcard_ok` | `SELECT id FROM users WHERE name LIKE 'abc%'` | does **not** contain `LeadingWildcardLike` |
| `test_function_wrapped_predicate_detected` | `SELECT id FROM users WHERE lower(email) = 'a@b.com'` | contains `FunctionWrappedPredicate` |
| `test_plain_predicate_ok` | `SELECT id FROM users WHERE email = 'a@b.com'` | does **not** contain `FunctionWrappedPredicate` |
| `test_not_in_detected` | `SELECT id FROM users WHERE id NOT IN (1, 2, 3)` | contains `NotIn` |
| `test_in_ok` | `SELECT id FROM users WHERE id IN (1, 2, 3)` | does **not** contain `NotIn` |
| `test_offset_without_limit_detected` | `SELECT id FROM users ORDER BY id OFFSET 50` | contains `OffsetWithoutLimit` |
| `test_offset_with_limit_ok` | `SELECT id FROM users ORDER BY id LIMIT 10 OFFSET 50` | does **not** contain `OffsetWithoutLimit` |
| `test_union_detected` | `SELECT id FROM a UNION SELECT id FROM b` | contains `UnionInsteadOfUnionAll` |
| `test_union_all_ok` | `SELECT id FROM a UNION ALL SELECT id FROM b` | does **not** contain `UnionInsteadOfUnionAll` |
| `test_unparseable_sql_returns_empty` | `"this is not valid sql @@@ ;;;"` | `analyze(...)` returns empty `Vec` |

Optional extra coverage (recommended, not required by spec):
- `test_not_in_subquery_detected`: `SELECT id FROM a WHERE id NOT IN (SELECT id FROM b)` → `NotIn`.
- `test_correlated_subquery_detected`: `SELECT id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id)` → `CorrelatedSubquery`.
- `test_non_correlated_subquery_ok`: `SELECT id FROM users WHERE id IN (SELECT user_id FROM orders)` → no `CorrelatedSubquery`.

Assertion style: use `assert!(kinds(sql).contains(&AntiPatternKind::X))` /
`assert!(!kinds(sql).contains(&AntiPatternKind::X))`, and for the unparseable
case `assert!(AntiPatternAnalyzer::new().analyze(sql).is_empty())`.

---

## 7. Implementation checklist / order

1. Create `crates/core/src/sql_analysis/anti_patterns.rs` with the public API (§2).
2. Implement the recursive walk + 7 detectors (§3) and dedup (§4).
3. Wire `mod.rs` (§5a) and `lib.rs` (§5b).
4. Add the test module (§6).
5. Run quality gates (from `CLAUDE.md`):
   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-features --all-targets -- -D warnings
   cargo test -p pg-plansight-core anti_patterns
   ```

## 8. Risk / uncertainty notes

- **`Query.limit_clause` vs spec's `Query.offset/limit`** — confirmed: 0.57 uses
  `limit_clause`. Plan uses the real shape.
- **Plain `UNION` quantifier** — sqlparser sets `SetQuantifier::Distinct` (or
  `None` if neither keyword present). The check `!matches!(.., All | AllByName)`
  covers both, so it is robust either way.
- **`FunctionWrappedPredicate`** is heuristic by nature; the AST checks chosen are
  concrete enough for the test SQL. If a future case needs broader coverage,
  extend `contains_column` recursion rather than adding regex.
- **`CorrelatedSubquery`** is an intentional Low-severity heuristic; document the
  scope-resolution limitation in code.

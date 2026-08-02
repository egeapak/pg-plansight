# Allocation churn in the parse path

The SIMD evaluation (`docs/SIMD_ANALYSIS.md`) found that ~14% of parse
instructions go to malloc/free/memcpy/memset — a cost no amount of
vectorisation addresses. This is that side, measured separately.

Reproduce with:

```bash
cargo run --release -p pg-plansight-core --example alloc_profile -- 2000
# per-site attribution (needs a debuginfo build):
CARGO_PROFILE_RELEASE_DEBUG=1 cargo build --release -p pg-plansight-core --example alloc_profile
valgrind --tool=dhat target/release/examples/alloc_profile 150
```

## 1. Baseline churn

`examples/alloc_profile` wraps the global allocator in a counter. On a
2,000-plan synthetic auto_explain log (3.75 MB), before any changes:

| | Allocations | Bytes | Per plan |
| --- | --- | --- | --- |
| Parse | 458,449 | 59.03 MB | 229.2 allocs |
| Grouping | 9,610 | 2.40 MB | 4.8 allocs |
| **Total** | **468,059** | **61.42 MB** | |
| Peak live | | 26.4 MiB | |

**229 allocations and 29.5 KB of allocator traffic per plan, for plans
averaging ~1.9 KB of input text** — a 15.7x byte amplification. Grouping is
not the problem; the parse path is.

## 2. Attribution

DHAT attributes ~42% of blocks to a single fully-inlined `alloc_impl` frame,
so per-site attribution alone was not conclusive. Reading the node-parsing
call chain found the actual pattern. For **each plan node line**:

1. `extract_node_type_from_line` returned an owned `String` — although every
   branch was a slice of the input.
2. `parse_node_type_from_string` called `.to_lowercase()` on it.
3. The `*Type::analyze` it then dispatched to called `.to_lowercase()` on the
   **same string again**.
4. On a fallthrough (`ScanType` fails, `UtilityType` is tried) that happened
   a third and fourth time.

Separately, `parse_plan_from_lines` cloned `PlanLine::query` into
`InternalPlanLine::content` for **every plan line** — a second copy of text the
caller already had resident.

## 3. What was changed

Three mechanical, behaviour-preserving changes in `plan_parser.rs`:

1. `extract_node_type_from_line` returns `&str` instead of `String`.
2. Each `*Type::analyze` was split into a public `analyze(line)` wrapper and an
   internal `analyze_with_lower(line, line_lower)`. `parse_node_type_from_string`
   now lowercases once and passes the result down. The public API is unchanged.
3. `InternalPlanLine::content` borrows (`&'a str`) instead of owning, so
   `parse_plan_from_lines` stops cloning every line.

All 300 core tests pass unchanged.

## 4. Measured effect

| | Before | After | Delta |
| --- | --- | --- | --- |
| Parse allocations | 458,449 | 390,470 | **−14.8%** |
| Parse bytes | 59.03 MB | 52.78 MB | **−10.6%** |
| Allocations per plan | 229.2 | 195.2 | −34 |
| Peak live | 26.4 MiB | 26.4 MiB | **unchanged** |
| End-to-end parse time | 118.29 ms | 113.24 ms | **−3.3%** |

The time figure is a criterion A/B against a saved baseline, same binary and
corpus, 25 s measurement windows: **−3.34%, 95% CI [−5.75%, −0.69%], p = 0.02**.
Statistically significant, but small.

Two results here are worth stating plainly rather than glossing:

**Peak memory did not move at all.** Every allocation removed was transient —
short-lived scratch `String`s freed within the same node. This work reduces
allocator *traffic*, not footprint. If the goal is peak RSS on large logs, this
is the wrong lever entirely; the streaming `QueryGrouper` is the right one.

**A 14.8% cut in allocations bought only 3.3% of time.** These are small,
short-lived allocations that hit glibc's tcache fast path, which costs tens of
nanoseconds, not hundreds. The 14% of instructions the profile attributes to
malloc/free is a real ceiling, but instruction count over-weights allocator
work relative to wall time because those instructions are cheap and
well-predicted. Removing 15% of allocations removing ~3% of time is consistent
with that ceiling, not a contradiction of it.

## 5. What remains

After the change, per DHAT on a 150-plan run (67,453 blocks total, down from
72,527):

| Blocks | Share | Site | Removable? |
| --- | --- | --- | --- |
| 28,121 | 41.7% | inlined `alloc_impl` (unattributable) | unknown |
| 6,600 | 9.8% | `parse_node_tree` | Partly — property `HashMap<String, String>` per node |
| 5,802 | 8.6% | `parse_node_type_from_string` | Mostly not — one lowercase remains, needed by `contains` |
| 4,379 | 6.5% | `QueryNormalizer::normalize` | No — sqlparser AST, inherent |
| 4,173 | 6.2% | `strip_quotes` | No — results are stored in `TableReference` |
| 3,137 | 4.7% | `TextPlanBuilder::finalize` | Partly |
| 2,145 | 3.2% | `format_sql_query` | Partly |
| 1,502 | 2.2% | `RegexPatterns::new` | No — one-time compile, amortises away |

The single remaining `to_lowercase()` per node line is deliberate. The SIMD
evaluation measured allocation-free case-insensitive alternatives against it
and they were **2.3x slower**: `contains()` on the lowered string is
`memchr::memmem` with a SIMD prefilter that short-circuits on the first needle,
whereas a per-needle case-insensitive search makes eight passes. Removing that
allocation costs more time than it saves. It could be eliminated properly by
threading a reusable scratch buffer through the analyze chain, but that is a
signature change across four public types for a fraction of 3%.

## 6. Recommendation

The three changes here are worth keeping — they are strictly less work, cost
nothing in complexity, and are already committed.

Beyond them, **allocation churn is not the profitable next lever**. The
measured return is ~3% for the easy 15%, and the remaining sources are either
inherent (sqlparser's AST, stored identifier strings) or need invasive
signature changes for sub-1% returns each. The regex work identified in
`docs/SIMD_ANALYSIS.md` — a projected ~21% for three self-contained scanner
swaps — is roughly seven times the return for comparable effort, and the two
are independent, so it can be taken first without redoing any of this.

If allocation is revisited later, the highest-value remaining item is the
per-node property `HashMap<String, String>` behind `parse_node_tree` (9.8% of
blocks): plan property keys come from a small fixed set, so interning them as
`&'static str` or a small enum would remove one allocation per property line
without touching the per-node lowercase question at all.

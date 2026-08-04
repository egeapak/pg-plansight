# SIMD in the log-parsing hot path

Where data-parallel byte scanning speeds up `pg-plansight-core`, which
candidates paid, which did not, and what the integration actually delivered.
Every number is measured.

Reproduce with:

```bash
cargo bench -p pg-plansight-core --bench simd_candidates
```

## 1. Where the time actually goes

A callgrind profile of an end-to-end parse (`cargo run --release -p
pg-plansight-core --example profile_parse 1500`, then
`valgrind --tool=callgrind`) attributes the run as:

| Cost centre | Share of instructions |
| --- | --- |
| `regex_automata` bounded backtracker | 27.8% |
| `regex_automata` lazy DFA (fwd + rev) | 8.0% |
| malloc / free / memcpy / memset | ~14% |
| `str::trim_matches` | 2.8% |
| everything else | remainder |

The backtracker is the engine `regex` falls back to for **capture extraction**
on short haystacks, and the parser calls `captures()` several times per plan
line. Measured per call on a representative node line:

| Pattern | ns/call |
| --- | --- |
| `LOG_LINE_PATTERN` (timestamp split) | 1177 |
| `COST_REGEX` (`(cost=..)` tuple) | 931 |
| `INDEX_REGEX` | 808 |
| `TABLE_REGEX` | 558 |

This matters for framing: **the workload is regex- and allocation-bound, not
byte-scan-bound.** It is not a case of "a scalar loop is scanning bytes one at
a time and SIMD would scan 32". The opportunity is that each of those patterns
describes a *fixed byte shape* — a timestamp, a cost tuple, a whitespace run —
recognisable by vector compares instead of by a regex engine.

## 2. Separating the two effects

Replacing a regex with a byte scanner bundles two distinct wins:

1. **Algorithmic** — not running a regex engine at all.
2. **Vectorisation** — comparing 16 or 32 bytes per instruction.

Only the second is attributable to SIMD, and on data this short it is by far
the smaller. So `crates/core/src/simd_scan.rs` implements **both** a `_scalar`
and a `_simd` version of every scanner, and the benchmark measures them as
separate tiers on identical input (plus, where the two differ, a `shipped` arm
for the integrated library function). Reporting a single "12x SIMD speedup" would be misleading;
the split below is the honest accounting.

The tiers only mean something if they do the *same work*. An earlier revision
of this document quoted a 1.29x vectorisation share for the timestamp split
that was an artefact: the `scalar` arm was missing the leading-byte
fast-reject the other arms had, so it paid a full 19-byte check on the ~90% of
lines that start with a tab. The arms are now byte-identical apart from the
scanner, and the real figure is 1.06x.

## 3. Results

4-vCPU Cascade Lake (AVX2 + AVX-512), 2,000-plan synthetic auto_explain log
(3.75 MB, 40,493 lines, 90.1% continuation lines, 1.88 KB per plan) generated
by the same generator as `benches/log_parsing.rs`.

**On absolute numbers:** this shared VM drifts. The same unchanged binary has
measured ~40% faster in one session than another, far beyond within-session
variance. Every table below is from a **single session** and internally
consistent; every claimed delta is a paired criterion A/B. Do not compare
absolute figures across sections captured at different times, and treat gaps
under ~5% (§3.2) as parity.

### 3.1 Candidates that pay

| Site | Current | Scalar bytes | SIMD | Total | SIMD's own share |
| --- | --- | --- | --- | --- | --- |
| Timestamp split, realistic line mix | 925.0 µs | 84.9 µs | **80.4 µs** | **11.5x** | 1.06x |
| Timestamp split, timestamped lines only | 862.9 µs | 32.1 µs | **27.4 µs** | **31.5x** | 1.17x |
| Cost tuple extraction | 3552.4 µs | 488.7 µs | **399.8 µs** | **8.9x** | 1.22x |
| Indentation counting | 70.6 µs | 50.3 µs | **37.3 µs** | **1.9x** | 1.35x |

Read the last two columns together. **Almost the entire win is dropping the
regex, not vectorising.** The timestamp split gets 11.5x overall and only
1.06x of that is SIMD. Indentation is the exception: it never used a regex (it
decoded UTF-8 `char`s to count leading spaces), so proportionally more of its
smaller gain is vectorisation.

The `SIMD` column is the shipped code path for the timestamp split, cost
extraction and indentation. The bench also carries a separate `shipped` arm
for the first two, which measures the library function across a crate
boundary; it lands within ~1.5% of the bench-local arm, so call overhead is
not distorting the comparison.

### 3.2 Candidates that do not pay

| Site | Current | Best alternative tried | Verdict |
| --- | --- | --- | --- |
| Plan-node detection (`(cost=` present?) | 459.2 µs | 441.9 µs (`memchr::memmem` screen) | **Parity** |
| Node-type classification | **259.1 µs** | 513.7 µs (hand SIMD), 903.5 µs (aho-corasick) | **2.0x slower** |

Both negative results have the same cause, worth stating plainly: **this code
is already SIMD-accelerated, just not by us.**

- Node detection runs `COST_REGEX.is_match`. The `regex` crate already applies
  a SIMD literal prefilter internally, so adding a `(cost=` screen in front of
  it duplicates the scan. Even `memchr::memmem` only reaches parity.
- Classification does `line.to_lowercase()` once then up to eight `contains()`
  calls. `contains()` is `memchr::memmem` with a SIMD rare-byte prefilter and
  short-circuits on the first needle that hits. A per-needle case-insensitive
  search means eight passes instead of one prefiltered pass. Single-pass
  multi-pattern `aho-corasick` also loses, because preserving the existing
  needle-priority semantics requires overlapping iteration, which disables its
  Teddy prefilter.

Neither was changed. The scanners behind them stay in `simd_scan` as the
evidence for this section, and because `parse_cost_tuple` builds on
`find_literal_simd`.

### 3.3 Call frequencies

Per-site numbers do not aggregate without call counts, so the counts were
measured by instrumenting the parser with atomic counters over a 2,000-plan
parse of this exact corpus:

```
lines in corpus                    40,493   (4,000 timestamped, 36,493 continuation)
split_log_line                     40,493   once per line
  ... reaching the regex                0   post-integration; 4,000 before it
get_indent_level                   55,986   1.53x per continuation line
extract_cost                       19,993   once per node line
COST_REGEX::is_match               27,993   in parse_plan_from_lines
COST_REGEX::is_match (parse_lines)      0   that path is not reached
```

An earlier revision of this document reported these against a *different*
generator (`profile_parse` had drifted from the bench's), and instrumented
only one of the two `is_match` call sites — which is why it claimed node
detection "never runs". It runs 27,993 times, more often than `extract_cost`.
That does not change the §3.2 conclusion, which was measured on its own
merits: a screen still does not pay there. But the frequency is real, and
`hot_loop_composite` excludes node detection because changing it loses, not
because it is cold.

## 4. What is in the tree

`crates/core/src/simd_scan.rs` — the scanners. Most carry both a scalar and a
vector implementation; the two composites (`parse_cost_tuple`,
`is_log_line_start`) inherit their back-end from the primitive they build on:

| Scanner | Back-ends | On the hot path? |
| --- | --- | --- |
| `timestamp_core_*` | SSE2 / NEON | yes — shared by the two below |
| `timestamp_prefix_len_*` | via `timestamp_core_*` | yes — `split_log_line` |
| `is_log_line_start` (in `parser_utils`) | via `timestamp_core_*` | yes — exporter, pg extension |
| `parse_cost_tuple` | via `find_literal_*` | yes — `extract_cost` |
| `leading_whitespace_*` | AVX2 / NEON | yes — `get_indent_level` |
| `find_literal_*` | AVX2 / NEON | indirectly, via `parse_cost_tuple` |
| `find_ascii_ci_*` | AVX2 only | no — evidence for §3.2 |

Two vector back-ends, chosen at compile time. On **x86_64**, SSE2 for the
16-byte timestamp core (baseline, no detection) and AVX2 for the
32-byte-at-a-time loops behind a runtime `is_x86_feature_detected!` check. On
**aarch64**, NEON for the three hot-path scanners (`find_ascii_ci_*` stays
scalar there — it is not on the hot path, and §3.2 measured its vectorised
form as a loss). No detection is needed: `target_feature = "neon"` is in the
default cfg set for the AArch64 targets, exactly as SSE2 is for x86_64.
AArch64 has no `movemask`, so where the x86 code
extracts a bitmask and compares it against a constant, the NEON code either
merges lanes with a bitwise select and takes a horizontal minimum, or narrows
the comparison result to one nibble per lane (`vshrn_n_u16` by 4).

32-bit `arm` is deliberately not covered: its NEON intrinsics are still
unstable in `core::arch`, and NEON is optional rather than architectural
there. It takes the scalar path, as does every other architecture.

### Correctness

A defect in the three integrated scanners silently corrupts parsed plans
rather than merely slowing them down, so exactness is the binding constraint.
Each is required to be *exactly* equivalent to what it shadows, including:

- `\d` in the `regex` crate matches Unicode digits, not just `[0-9]`.
- `[A-Z]{2,5}` is greedy but fails the whole optional group below 2.
- `(?::?\d{2})?` backtracks the colon away when two digits do not follow.
- `COST_REGEX`'s greedy `([\d.]+)\.\.` backtracks to the **last** `..` in the
  numeric run, where a forward scan takes the first.
- `char::is_whitespace` is true for non-ASCII code points such as U+00A0.
- Group 2 is `(.*)`, and `.` does not match `\n`.

Rather than reimplement Unicode tables, a scanner reports `Verdict::Unsure`
the moment a non-ASCII byte could change the answer and the caller defers to
the regex. `parse_cost_tuple` goes further and declines (`None`) on anything
that is not *precisely* the `(cost=..)` shape — a false negative costs only
the regex call that would have happened anyway, while a false positive would
silently report a different cost.

That distinction is not theoretical. A review found `parse_cost_tuple`
returning a wrong value, not a fallback, when the numeric run held a dot
cluster of exactly three:

```
"Seq Scan on t  (cost=50...5 rows=7 width=8)"
   COST_REGEX -> min "50." max "5"   -> max_total_cost 5.0
   scanner    -> min "50"  max ".5"  -> max_total_cost 0.5   (wrong)
```

The guard rejected a second `..` inside `max`, but with three dots the third
lands as `max`'s first byte where no `..` window exists. Fixed by requiring
the cluster at the separator to be exactly two dots — sound because the scan
can only stop at the *start* of a cluster. The random-token differential test
could not reach this (the probability of assembling a complete valid tuple
around `...` in one draw is negligible), so it was replaced with an exhaustive
enumeration of every string over `{0, 5, .}` up to length 8 substituted into a
well-formed tuple. That test fails with the fix reverted.

Coverage: differential tests against the oracle regex for every scanner,
including a 20,000-case generated corpus for the timestamp scanner, the
exhaustive enumeration above for the cost parser, and the pre-existing 50,000
-case fuzz for `split_log_line` (whose alphabet already included Arabic-Indic
and fullwidth digits). The benchmark additionally gates every timing group
behind a full-corpus equivalence check, including the shipped
`parse_cost_tuple`.

CI runs the core crate's unit tests (`--lib`, 305 of them, including every
differential test above) on aarch64 under qemu, so the NEON scanners are held
to the same oracles as the x86 ones, plus an i686 check so the
generic scalar arm is compiled on every PR rather than first at release time.

The NEON back-end was additionally reviewed with 40 million differential cases
per architecture (5M each for the timestamp, whitespace, literal-search and
cost scanners, plus exhaustive single- and two-byte mutation sweeps of a
canonical timestamp, and a guard-page harness that traps any read past the end
of a haystack). Every result digest is **bit-identical between x86_64
(SSE2+AVX2) and aarch64 (NEON)**, including a digest of the full parsed plan
tree for a 400-plan log. Injected mutants — a wrong lane in the digit-select
constant, a reversed nibble-to-lane mapping, a load one byte too far — were
all caught, so the sweep is not vacuous.

## 5. Integration result

All three winning candidates are integrated; the two losses were left alone.

| Plan | Site | Status |
| --- | --- | --- |
| A | `parser_utils::split_log_line` | Integrated |
| B | `plan_parser::extract_cost` | Integrated |
| C | `parser_utils::get_indent_level`, `plan_parser::count_indentation` | Integrated |
| — | node detection, node classification | **Not changed** (§3.2) |

Three states, each checked out from its own commit and measured back-to-back
in one session, 20–25 s windows:

| State | Parse time (median) | Allocations | Per plan |
| --- | --- | --- | --- |
| `bb60842` — scanners exist, nothing integrated | 76.80 ms | 458,449 | 229.2 |
| `7523adc` — + allocation fixes | 76.69–77.23 ms | 390,470 | 195.2 |
| `HEAD` — + SIMD scanners | **51.70 ms** | **366,344** | **183.2** |

| Comparison | Delta | 95% CI | p |
| --- | --- | --- | --- |
| Allocation fixes alone | none detected | [−0.87%, +2.06%] | 0.55 |
| SIMD scanners on top | **−31.5%** | [−32.9%, −30.2%] | 0.00 |
| **Cumulative** | **−33.0%** | [−33.8%, −32.2%] | 0.00 |

**Do not compute the deltas from the medians.** Criterion's change estimate is
a bootstrap over the two full sample distributions, not a ratio of reported
medians, and the two differ by up to ~1.5 percentage points here. The
confidence intervals and p-values are the authoritative figures; the medians
are context. The `7523adc` row shows a range because that state was measured
twice — once as the baseline the SIMD comparison ran against (76.69 ms) and
once as the subject of the allocation comparison (77.23 ms). Two runs of the
same binary, ~0.7% apart, which is the within-session noise floor.

**1.49x faster end-to-end, and 20% fewer allocations per plan.**

Two honest notes on that table. First, the allocation fixes' *speed* effect is
below this environment's resolution: one session measured −3.3% (p = 0.02),
another −6.3%, and this one nothing at all (p = 0.55). The allocation *counts*
are exact and reproducible; the time saving is not. See
`docs/ALLOCATION_ANALYSIS.md`. Second, the SIMD result beat the ~21% projected
in an earlier revision, because that projection replayed indentation once per
continuation line rather than the 1.53x the parser really does, and did not
model the `Vec<char>` removal that came with Plan C.

## 6. What is left

Node detection and node-type classification should stay unchanged — §3.2
measured both as losses against code `regex` and `memchr` already vectorise.

The remaining lever is not SIMD. `docs/ALLOCATION_ANALYSIS.md` covers
allocation churn, now 183 per plan; the largest single remaining source is the
per-node property `HashMap<String, String>`.

Worth noting for anyone extending this: **~91% of the timestamp win and ~86%
of the cost win are the regex removal, not vectorisation** (§3.1). If the
`unsafe` in `simd_scan` ever becomes a maintenance concern, dropping to the
scalar implementations would surrender very little of what was gained.

`parser_utils::is_log_line_start` — used by the exporter's checkpoint scan and
the pg extension's ingest offset logic — was a second hand-written copy of the
same 19-byte core. It is now a `bool` view of `simd_scan::timestamp_core_simd`,
so there is one definition of the shape rather than two that could drift, and
that path picks up the vector compare as a side effect. `Unsure` maps to
`false`, which is exactly what the old `is_ascii_digit()` chain answered for a
non-ASCII byte; a regression test pins the folded version against a verbatim
copy of the pre-fold implementation over every single-byte mutation and every
truncation of a valid prefix.

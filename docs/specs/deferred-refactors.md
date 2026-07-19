# Spec: Deferred refactors (review follow-ups A–F)

Status: implemented. These are the architectural/efficiency items surfaced by
the review and deliberately deferred from the bug-fix pass because they are
refactors, not defects. Each preserves behavior (the full test suite is the
acceptance gate) and removes a duplication, sawtooth, or wasted pass.

## A — Unify the two parsing state machines
**Problem.** `log_parser.rs` (streaming file parser) and
`log_entry_parser.rs` (`LogEntryParser`, used by the pg extension) each carried
a full copy of the WaitingForQuery/ParsingQuery/ParsingTextPlan/ParsingJsonPlan
continuation transitions and the JSON-demotion flow. They had already drifted
(inline `starts_with('{')` + `detect_plan_format` vs `looks_like_json_start`).
**Design.** One method on the shared `LogParsingState` enum:
`advance_continuation(line, plan_regex) -> ContinuationOutcome {state, plan,
error}`. A text-plan finalization error is returned in `error` (state → None) so
each caller keeps its policy: the extension propagates it; the streaming parser
warns and continues. Both callers delegate; the duplicate handlers and the
`JsonStep` enum are deleted; both agree on `looks_like_json_start`.
**Verify.** 362 core all-features + 285 no-default tests, the real-log fixture,
and the extension's 34 `pg_test`s (drive `LogEntryParser` on live PG 16) pass.

## B — Single-pass exporter parse
**Problem.** The hold-back path scanned the new byte range for the last
complete-entry boundary (`find_entry_boundary`), then re-read the same range
from disk to parse it — two passes per poll.
**Design.** Read the range into memory once, find the boundary in that buffer
(`last_entry_boundary`), parse the bytes up to it from a `Cursor` via
`parse_with_progress`. The flush and compressed-file paths stay on
`parse_file_range_with_progress` (they decompress / read to EOF); `hold_back` is
only ever set for uncompressed incremental reads. Range size is bounded by
`max_file_size_mb` and, in steady state, one poll interval of new content.
**Verify.** 84 exporter tests incl. mid-write hold-back, quiescent flush,
compressed ingest, and direct boundary/read-range unit tests.

## C — LRU fingerprint cache
**Problem.** The query-hash→fingerprint cache grew unbounded and, at 100k
entries, was cleared wholesale — a CPU sawtooth in the long-lived exporter, and
permanently useless for a working set just over the cap.
**Design.** Inline LRU (`FingerprintCache`): `BTreeMap<tick, hash>` recency +
`HashMap<hash, (fingerprint, tick)>`; `get` refreshes recency, `insert` evicts
the LRU entry when full (`cap == 0` = unbounded).
**Verify.** Unit tests: LRU eviction (touched survives, idle evicted), in-place
refresh without growth, unbounded mode.

## D — JSON parse-once
**Problem.** The streaming `JsonPlanBuilder` parsed each JSON entry ~4×
(`normalized_content` parse+serialize, `JsonPlanParser::parse`'s discarded
parse, `ParsedPlan::from_json_plan`'s parse, and the factory's `from_str`).
**Design.** `ParsedPlan::from_json_plan_struct(&JsonPlan)` and
`PlanFactory::create_json_query_plan_from_struct(...)` build from an
already-deserialized `JsonPlan`; `build_query_plan` does one `from_str` + one
`from_value` + one tree conversion. `raw_json` keeps the normalized
single-element array form, so the stored/exported shape is unchanged.
**Verify.** Object/array/demotion outcomes locked by new tests; 354→362 core
tests and the real-log JSON fixture pass.

## E — Configurable byte caps
**Problem.** `MAX_LINE_BYTES` (64 MiB) / `MAX_ENTRY_BYTES` (256 MiB) were private
constants, but the same parser runs in the exporter and the extension, where a
memory-constrained deployment may want them lower and a legitimate huge IN-list
higher.
**Design.** `PostgreSQLLogParser` gains `max_line_bytes`/`max_entry_bytes`
(default to the consts) and a `with_byte_limits(max_line, max_entry)` builder;
`0` disables a cap (`u64::MAX`), documented as trusted-input-only.
**Verify.** A tiny per-line cap truncates a long line while a following entry
still parses; a tiny per-entry cap discards an oversized entry.

## F — TUI plan-render cache borrow
**Problem.** The plan-render cache avoided re-rendering, but a cache HIT still
deep-cloned the entire cached `Text<'static>` each frame.
**Design.** A module-level `borrow_text()` rebuilds a `Text<'a>` borrowing the
cached spans' string data (`Cow::Borrowed`); the plan `Paragraph` is built after
the `&mut self` highlight step so it holds only a shared borrow of the cache.
The full-`Text` clone is gone from the hit path.
**Verify.** 87 TUI tests pass; output unchanged.

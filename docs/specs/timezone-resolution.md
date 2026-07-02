# Spec: Configurable log-timezone resolution

Status: implemented (`crates/core/src/parser_utils.rs`, `crates/core/src/log_parser.rs`)

## Problem

PostgreSQL's `%m`/`%t` `log_line_prefix` tokens print a timezone as either a
numeric offset (`+02`, `-05:30`) or an **abbreviation** (`UTC`, `PDT`, `CST`,
`IST`). Abbreviations are inherently ambiguous:

- `CST` = US Central (−6) in PostgreSQL's *Default* `timezone_abbreviations`
  file, but China Standard Time (+8) elsewhere.
- `IST` = India (+5:30), Israel, or Ireland.
- `BST` = British Summer (+1) vs Bangladesh (+6).

The parser must convert wall-clock log times to UTC. A single hard-coded table
silently shifts every timestamp by hours on a server whose `log_timezone`
prints a conflicting abbreviation — corrupting `--since`/`--until` filtering,
hourly histograms, first/last-seen gauges, and temporal regression analysis.

## Goals

1. Preserve zero-config behavior: with no configuration, resolution is exactly
   the built-in Default-tznames table (backward compatible).
2. Let a caller override how ambiguous abbreviations are interpreted, per-token
   or as a single fixed offset for the whole server.
3. Numeric offsets in the log are unambiguous and must **always** win over any
   configuration.
4. No new dependencies (no `chrono-tz`); must build under `--no-default-features`
   (the extension-embedded core).

## Design

`TimezoneResolver` (`Debug + Clone + Default`):

- `overrides: HashMap<String, i32>` — per-abbreviation offset (seconds east of
  UTC). Keys upper-cased on insert (PostgreSQL emits upper-case tokens), so the
  parse-time lookup is allocation-free and case-tolerant for callers.
- `fixed_offset_seconds: Option<i32>` — applied to any abbreviation the map does
  not name (a server with one known `log_timezone`).

Builder: `new()`, `with_override(abbrev, secs)`, `with_fixed_offset_seconds(secs)`.

Resolution order for a token (`parse_timestamp_with_tz`):

1. **Numeric** offset (`+HH`, `±HHMM`, `±HH:MM`) → used directly, config ignored.
2. `overrides` map (by upper-cased token).
3. `fixed_offset_seconds`.
4. Built-in Default-tznames table.
5. Unknown → assume UTC, warn **once per distinct token** (never per line).

`parse_timestamp(s)` delegates to `parse_timestamp_with_tz(s, &default())`, so
its signature and behavior are unchanged. `PostgreSQLLogParser::with_timezone_override(resolver)`
threads a resolver into the parser's per-entry hot loop.

## Known limitations (documented, not bugs)

- The built-in table stores offsets in half-hour units, so 45-minute zones
  (Nepal `NPT` +5:45, `ACWST` +8:45) are not built-in. They are still exact via
  a numeric offset in the log, or via `with_override(tok, secs)` (full seconds).
- DST is carried by the **token** itself (`CET` vs `CEST`, `EST` vs `EDT`), as
  PostgreSQL prints the daylight-aware abbreviation; the resolver does not do
  date-based DST math.
- Tokens are matched as PostgreSQL emits them (upper case).

## Acceptance criteria & verification (`parser_utils.rs` tests)

- `test_default_resolver_matches_builtin_table` — zero-config unchanged.
- `test_timezone_override_remaps_abbreviation` / `test_override_key_is_case_insensitive`
  — override remaps `CST`→+8; lower/mixed-case keys still match.
- `test_numeric_offset_wins_over_override` — numeric always wins.
- `test_fixed_offset_applies_to_any_abbreviation` — fixed offset + per-token precedence.
- `test_dst_abbreviations_resolve_distinctly` — CET/CEST, EST/EDT distinct.
- `test_fractional_hour_offsets` — IST/NST half-hour; +05:45 numeric and NPT override.
- `test_unknown_zone_still_assumed_utc_with_override` — unknown → UTC, no failure.
- `test_parser_honors_timezone_override_end_to_end` — behavioral: a `CST` log
  parsed through `PostgreSQLLogParser::with_timezone_override` lands at +8, and
  the default parser lands at −6.

//! SIMD byte scanners for the log-parsing hot path.
//!
//! # Why this module exists
//!
//! A callgrind profile of the end-to-end parse (`examples/profile_parse`, a
//! synthetic auto_explain log) attributes ~28% of all instructions to
//! `regex_automata`'s bounded backtracker and another ~14% to malloc/free. The
//! backtracker is what the `regex` crate falls back to for *capture
//! extraction* on short haystacks, and the parser calls `captures()` several
//! times per plan line. Measured per call on a representative node line:
//!
//! ```text
//! LOG_LINE_REGEX.captures()   1177 ns      COST_REGEX.captures()    931 ns
//! INDEX_REGEX.captures()       808 ns      TABLE_REGEX.captures()   558 ns
//! ```
//!
//! Every one of those patterns describes a *fixed byte shape* — a timestamp,
//! a `(cost=..)` tuple, a leading-whitespace run. Those shapes are exactly
//! what data-parallel byte comparisons are good at, so the regex can be
//! replaced by a handful of vector instructions.
//!
//! # Honest accounting
//!
//! Two distinct effects are bundled together whenever a regex is replaced by a
//! scanner in this module, and they should not be conflated:
//!
//! 1. **Algorithmic**: not running a regex engine at all. This is the large
//!    factor, and a plain scalar byte loop captures most of it.
//! 2. **Vectorisation**: doing the byte comparisons 16 at a time. This is the
//!    smaller factor on data this short.
//!
//! A `_scalar` implementation is therefore kept next to every `_simd` one, and
//! `benches/simd_candidates.rs` measures both tiers (regex / scalar / SIMD) so
//! the split between the two effects is visible rather than assumed.
//!
//! # Correctness
//!
//! `timestamp_core_simd`, `parse_cost_tuple` and `leading_whitespace_simd` are
//! on the parser's hot path (`split_log_line`, `extract_cost`,
//! `get_indent_level`). A defect in them silently corrupts parsed plans rather
//! than merely slowing them down, so exactness is the binding constraint.
//!
//! Each scanner is required to be *exactly* equivalent to the regex or
//! `char`-based implementation it shadows, including the awkward corners
//! (`\d` matching Unicode digits, greedy `[A-Z]{2,5}`, `(?::?\d{2})?`
//! backtracking). The tests at the bottom of this file assert that
//! equivalence differentially, over both hand-written corner cases and
//! exhaustive byte sweeps. Where the fast path cannot be certain — the input is
//! not ASCII where the regex would consult Unicode tables — it reports
//! [`Verdict::Unsure`] and the caller is expected to defer to the regex, which
//! keeps behaviour identical by construction.
//!
//! # Portability
//!
//! The vector kernels are written once against [`fearless_simd`] and contain no
//! `unsafe`. Every kernel works on fixed 16-byte blocks and dispatches on
//! [`Level::baseline`] (SSE2 on x86_64, NEON on AArch64, which resolves at
//! compile time), so there is no feature-detection load or level match per
//! call and no AVX2/AVX-512 even where available. The inputs are a 19-byte
//! timestamp, an indentation run and a plan line, so wider native vectors
//! would mostly push lines onto the scalar tail rather than save iterations.
//! Other targets run the crate's scalar fallback.

use fearless_simd::{Level, Select, Simd, SimdBase, SimdMask, dispatch, i8x16, mask8x16, u8x16};

const BLOCK: usize = 16;

/// Outcome of a fast-path shape check that may not be able to decide alone.
///
/// The patterns these scanners replace use `\d`, which in the `regex` crate's
/// default Unicode mode matches far more than `[0-9]` — Devanagari digits,
/// fullwidth digits, and so on. Rather than reimplement that table, the ASCII
/// fast path reports [`Verdict::Unsure`] the moment a non-ASCII byte could
/// change the answer, and the caller re-runs the regex. Real PostgreSQL
/// timestamps are always ASCII, so this costs nothing in practice while
/// keeping the two implementations observably identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The pattern matched; the payload is the length of capture group 1.
    Match(usize),
    /// The pattern definitely does not match, no regex needed.
    NoMatch,
    /// A non-ASCII byte sits where the regex would consult Unicode tables.
    /// The caller must fall back to the regex to stay exact.
    Unsure,
}

// ---------------------------------------------------------------------------
// 1. Timestamp prefix (`LOG_LINE_PATTERN` group 1)
// ---------------------------------------------------------------------------

/// `YYYY-MM-DD HH:MM:SS`: `d` is an ASCII digit, every other byte a literal.
const TS_SHAPE: &[u8; 19] = b"dddd-dd-dd dd:dd:dd";
const TS_CORE_LEN: usize = TS_SHAPE.len();

/// One 16-byte view of [`TS_SHAPE`]: `-1` in `digit_lanes` where the byte
/// must be an ASCII digit, otherwise the byte must equal `literals`.
struct TsWindow {
    digit_lanes: [i8; BLOCK],
    literals: [u8; BLOCK],
}

const fn ts_window(start: usize) -> TsWindow {
    let mut window = TsWindow {
        digit_lanes: [0; BLOCK],
        literals: [0; BLOCK],
    };
    let mut lane = 0;
    while lane < BLOCK {
        let shape_byte = TS_SHAPE[start + lane];
        if shape_byte == b'd' {
            window.digit_lanes[lane] = -1;
        } else {
            window.literals[lane] = shape_byte;
        }
        lane += 1;
    }
    window
}

/// The core is 19 bytes, so it is covered by two overlapping blocks.
const TS_HEAD: TsWindow = ts_window(0);
const TS_TAIL: TsWindow = ts_window(TS_CORE_LEN - BLOCK);

#[inline(always)]
fn ts_window_matches<S: Simd>(simd: S, block: u8x16<S>, window: &TsWindow) -> mask8x16<S> {
    // Wrapping `b - '0'` is at most 9 exactly for ASCII digits.
    let is_digit = (block - u8x16::splat(simd, b'0')).simd_le(9);
    let is_literal = block.simd_eq(u8x16::from_slice(simd, &window.literals));
    let digit_lanes = mask8x16::from_vector(i8x16::from_slice(simd, &window.digit_lanes));
    digit_lanes.select(is_digit, is_literal)
}

#[inline(always)]
fn ts_core_kernel<S: Simd>(simd: S, core: &[u8; TS_CORE_LEN]) -> Verdict {
    let head = u8x16::from_slice(simd, &core[..BLOCK]);
    let tail = u8x16::from_slice(simd, &core[TS_CORE_LEN - BLOCK..]);

    // Non-ASCII anywhere in the core wins over a shape mismatch, as in the
    // scalar version: the regex's Unicode `\d` might still match there.
    if (head | tail).simd_ge(0x80).any_true() {
        return Verdict::Unsure;
    }
    let shape_ok =
        ts_window_matches(simd, head, &TS_HEAD) & ts_window_matches(simd, tail, &TS_TAIL);
    if shape_ok.all_true() {
        Verdict::Match(TS_CORE_LEN)
    } else {
        Verdict::NoMatch
    }
}

/// Validate the mandatory 19-byte `YYYY-MM-DD HH:MM:SS` core that every
/// `%m`/`%t`-prefixed PostgreSQL log line starts with, returning
/// `Match(19)`, `NoMatch`, or `Unsure`.
///
/// Every consumer of that shape goes through here.
/// [`timestamp_prefix_len_simd`] continues from `Match` into the optional
/// tails to produce a full group-1 length, and
/// [`is_log_line_start`](crate::parser_utils::is_log_line_start) is a `bool`
/// view used by the exporter's checkpoint scan and the extension's ingest
/// offset logic.
///
/// Two overlapping 16-byte blocks cover the core, so there is no scalar tail.
#[inline]
pub fn timestamp_core_simd(line: &[u8]) -> Verdict {
    match line.first_chunk::<TS_CORE_LEN>() {
        Some(core) => dispatch!(Level::baseline(), simd => ts_core_kernel(simd, core)),
        None => Verdict::NoMatch,
    }
}

/// Scalar equivalent of [`timestamp_core_simd`], byte-at-a-time.
#[inline]
pub fn timestamp_core_scalar(line: &[u8]) -> Verdict {
    if line.len() < TS_CORE_LEN {
        return Verdict::NoMatch;
    }
    // A non-ASCII byte anywhere in the core is a position where the regex's
    // Unicode-aware `\d` could still match; defer rather than guess.
    if line[..TS_CORE_LEN].iter().any(|&b| !b.is_ascii()) {
        return Verdict::Unsure;
    }
    const DIGITS: [usize; 14] = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18];
    for i in DIGITS {
        if !line[i].is_ascii_digit() {
            return Verdict::NoMatch;
        }
    }
    if line[4] != b'-' || line[7] != b'-' || line[10] != b' ' {
        return Verdict::NoMatch;
    }
    if line[13] != b':' || line[16] != b':' {
        return Verdict::NoMatch;
    }
    Verdict::Match(TS_CORE_LEN)
}

/// Match the timestamp that a `%m`/`%t` `log_line_prefix` puts at the start of
/// a line, returning the length of what
/// `LOG_LINE_PATTERN` (see `parser_utils`) captures as
/// group 1 — i.e. the split point between the timestamp and the message.
///
/// The pattern being replicated is
///
/// ```text
/// ^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(?:\.\d{1,6})?(?: (?:[A-Z]{2,5}|[+-]\d{2}(?::?\d{2})?))?)
/// ```
///
/// The fixed core is [`timestamp_core_simd`]; the two optional tails
/// (fractional seconds, timezone token) are short and variable, so they stay
/// scalar.
#[inline]
pub fn timestamp_prefix_len_simd(line: &[u8]) -> Verdict {
    match timestamp_core_simd(line) {
        Verdict::Match(_) => timestamp_tail(line),
        other => other,
    }
}

/// Scalar equivalent of [`timestamp_prefix_len_simd`], byte-at-a-time.
///
/// Kept as a first-class implementation rather than a fallback: benchmarking
/// it against the SIMD version is what separates "we stopped running a regex"
/// from "we vectorised the comparison".
#[inline]
pub fn timestamp_prefix_len_scalar(line: &[u8]) -> Verdict {
    match timestamp_core_scalar(line) {
        Verdict::Match(_) => timestamp_tail(line),
        other => other,
    }
}

/// Consume the two optional tails after the 19-byte core:
/// `(?:\.\d{1,6})?` then `(?: (?:[A-Z]{2,5}|[+-]\d{2}(?::?\d{2})?))?`.
///
/// Both are short and irregular, so this stays scalar — vectorising a 1-to-6
/// byte run would cost more in setup than it saves. The fiddly part is
/// reproducing the regex's leftmost-greedy semantics exactly; see the inline
/// notes at each decision point.
#[inline]
fn timestamp_tail(line: &[u8]) -> Verdict {
    let mut pos = TS_CORE_LEN;

    // `(?:\.\d{1,6})?` — greedy, so it takes as many digits as it can up to 6.
    // There is no backtracking pressure to take fewer: the rest of the pattern
    // is `(.*)`, which always succeeds.
    if line.get(pos) == Some(&b'.') {
        let mut n = 0;
        while n < 6 {
            match line.get(pos + 1 + n) {
                Some(b) if b.is_ascii_digit() => n += 1,
                // A non-ASCII byte here is a position where Unicode `\d` could
                // still match, so the ASCII path cannot decide.
                Some(b) if !b.is_ascii() => return Verdict::Unsure,
                _ => break,
            }
        }
        // Fewer than one digit means the optional group fails outright and the
        // '.' stays in the message.
        if n >= 1 {
            pos += 1 + n;
        }
    }

    // `(?: (?:[A-Z]{2,5}|[+-]\d{2}(?::?\d{2})?))?` — a space followed by
    // either an uppercase abbreviation or a numeric offset. The two branches
    // start with disjoint characters, so alternation order does not matter.
    if line.get(pos) == Some(&b' ') {
        match line.get(pos + 1) {
            Some(&c) if c.is_ascii_uppercase() => {
                // `[A-Z]{2,5}` is greedy: take up to 5, but fail the whole
                // optional group if fewer than 2 are available.
                let mut n = 0;
                while n < 5 {
                    match line.get(pos + 1 + n) {
                        Some(b) if b.is_ascii_uppercase() => n += 1,
                        _ => break,
                    }
                }
                if n >= 2 {
                    pos += 1 + n;
                }
            }
            Some(&c) if c == b'+' || c == b'-' => {
                // `[+-]\d{2}` is mandatory for this branch.
                let d1 = line.get(pos + 2).copied();
                let d2 = line.get(pos + 3).copied();
                if let (Some(a), Some(b)) = (d1, d2) {
                    if !a.is_ascii() || !b.is_ascii() {
                        return Verdict::Unsure;
                    }
                    if a.is_ascii_digit() && b.is_ascii_digit() {
                        let mut end = pos + 4;
                        // `(?::?\d{2})?` — the colon is optional *inside* the
                        // optional group, so a colon not followed by two
                        // digits makes the whole group fail and the colon
                        // stays in the message.
                        let mut p = end;
                        if line.get(p) == Some(&b':') {
                            p += 1;
                        }
                        match (line.get(p).copied(), line.get(p + 1).copied()) {
                            (Some(x), Some(y)) if !x.is_ascii() || !y.is_ascii() => {
                                return Verdict::Unsure;
                            }
                            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                                end = p + 2;
                            }
                            _ => {}
                        }
                        pos = end;
                    }
                }
            }
            _ => {}
        }
    }

    Verdict::Match(pos)
}

// ---------------------------------------------------------------------------
// 2. Leading whitespace run (`get_indent_level`)
// ---------------------------------------------------------------------------

/// True for the six ASCII bytes that `char::is_whitespace` accepts.
#[inline]
const fn is_ascii_ws(b: u8) -> bool {
    b == b' ' || b.wrapping_sub(0x09) <= 0x0D - 0x09
}

const LANE_INDEX: [u8; BLOCK] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Index of the first lane where `mask` is false, if any.
///
/// x86 turns `to_bitmask` into one `pmovmskb`. AArch64 has no `movemask`, and
/// emulating it costs four vector instructions and a horizontal add, so there
/// a horizontal minimum over the lane indices is cheaper (measured ~7% on the
/// indentation bench).
#[inline(always)]
fn first_false_lane<S: Simd>(simd: S, mask: mask8x16<S>) -> Option<usize> {
    if cfg!(target_arch = "aarch64") {
        let lane = mask
            .select(
                u8x16::splat(simd, BLOCK as u8),
                u8x16::from_slice(simd, &LANE_INDEX),
            )
            .reduce_min() as usize;
        (lane < BLOCK).then_some(lane)
    } else {
        let false_lanes = !mask.to_bitmask() & ((1 << BLOCK) - 1);
        (false_lanes != 0).then(|| false_lanes.trailing_zeros() as usize)
    }
}

#[inline(always)]
fn leading_ws_kernel<S: Simd>(simd: S, line: &[u8]) -> Verdict {
    let mut offset = 0;
    while let Some(chunk) = line.get(offset..offset + BLOCK) {
        let block = u8x16::from_slice(simd, chunk);
        // `char::is_whitespace` in ASCII: ' ' or the range 0x09..=0x0D.
        let is_ws = block.simd_eq(b' ') | (block - u8x16::splat(simd, 0x09)).simd_le(0x0D - 0x09);
        if let Some(lane) = first_false_lane(simd, is_ws) {
            let end = offset + lane;
            return if line[end].is_ascii() {
                Verdict::Match(end)
            } else {
                Verdict::Unsure
            };
        }
        offset += BLOCK;
        // Pull a partial final block back to overlap the previous one rather
        // than finishing in scalar; its re-examined lanes are known whitespace.
        if offset < line.len() && offset + BLOCK > line.len() {
            offset = line.len() - BLOCK;
        }
    }
    match leading_whitespace_scalar(&line[offset..]) {
        Verdict::Match(n) => Verdict::Match(offset + n),
        other => other,
    }
}

/// Count leading whitespace, equivalently to
/// `line.chars().take_while(|c| c.is_whitespace()).count()`.
///
/// The previous implementation decoded UTF-8 one `char` at a time to count
/// what is, on every real plan line, a run of ASCII spaces. This compares 16
/// bytes per iteration instead.
///
/// Returns [`Verdict::Unsure`] when the run is stopped by a non-ASCII byte,
/// since `char::is_whitespace` is true for several non-ASCII code points
/// (U+00A0, U+2028, ...) and deciding would mean decoding after all.
#[inline]
pub fn leading_whitespace_simd(line: &[u8]) -> Verdict {
    dispatch!(Level::baseline(), simd => leading_ws_kernel(simd, line))
}

/// Scalar equivalent of [`leading_whitespace_simd`].
#[inline]
pub fn leading_whitespace_scalar(line: &[u8]) -> Verdict {
    let mut i = 0;
    while i < line.len() {
        let b = line[i];
        if is_ascii_ws(b) {
            i += 1;
        } else if b.is_ascii() {
            return Verdict::Match(i);
        } else {
            return Verdict::Unsure;
        }
    }
    Verdict::Match(i)
}

// ---------------------------------------------------------------------------
// 3. Literal search (`(cost=` plan-node detection)
// ---------------------------------------------------------------------------

#[inline(always)]
fn find_literal_kernel<S: Simd>(simd: S, haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let needle_len = needle.len();
    let first = u8x16::splat(simd, needle[0]);
    let last = u8x16::splat(simd, needle[needle_len - 1]);
    let candidate_starts = haystack.len() - needle_len + 1;
    let mut offset = 0;
    while offset + BLOCK <= candidate_starts {
        let last_offset = offset + needle_len - 1;
        let block_first = u8x16::from_slice(simd, &haystack[offset..offset + BLOCK]);
        let block_last = u8x16::from_slice(simd, &haystack[last_offset..last_offset + BLOCK]);
        // Requiring the first and last needle byte to line up makes false
        // candidates rare, so the bitmask and `memcmp` stay off the hot path.
        let candidates = block_first.simd_eq(first) & block_last.simd_eq(last);
        if candidates.any_true() {
            let mut lanes = candidates.to_bitmask();
            while lanes != 0 {
                let start = offset + lanes.trailing_zeros() as usize;
                if &haystack[start..start + needle_len] == needle {
                    return Some(start);
                }
                lanes &= lanes - 1;
            }
        }
        offset += BLOCK;
    }
    find_literal_scalar(&haystack[offset..], needle).map(|found| offset + found)
}

/// Find the first occurrence of `needle` in `haystack`.
///
/// This is the primitive behind `parse_cost_tuple`'s `(cost=` lookup. The
/// classic "generic SIMD" filter is used: broadcast the first and last needle
/// bytes, compare both against 16-byte windows, and only run a full `memcmp`
/// where both agree — which for a 6-byte needle over log text is almost never.
#[inline]
pub fn find_literal_simd(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    dispatch!(Level::baseline(), simd => find_literal_kernel(simd, haystack, needle))
}

/// Scalar equivalent of [`find_literal_simd`].
#[inline]
pub fn find_literal_scalar(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let last = needle[needle.len() - 1];
    let end = haystack.len() - needle.len();
    let mut i = 0;
    while i <= end {
        if haystack[i] == first
            && haystack[i + needle.len() - 1] == last
            && &haystack[i..i + needle.len()] == needle
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// 4. Cost tuple (`COST_REGEX`)
// ---------------------------------------------------------------------------

/// A parsed `(cost=..)` tuple: `(startup, total, rows, width)`.
pub type CostTuple = (f64, f64, u64, u32);

/// Parse `(cost=1.23..4.56 rows=7 width=8)`, equivalently to
/// `COST_REGEX.captures()` followed by parsing the four groups.
///
/// The `(cost=` marker is located with the vectorised literal search; the
/// four fields are then parsed byte-wise. Most of the win here is skipping the
/// regex engine rather than the vector compare — see `docs/SIMD_ANALYSIS.md`.
///
/// # Exact-or-decline
///
/// This returns `None` for anything that is not *precisely* the shape above,
/// and the caller is expected to fall back to the regex. That is deliberate:
/// a false negative only costs the regex call that would have happened
/// anyway, whereas a false positive would silently produce a different cost
/// than the parser reports today. Inputs that decline rather than guess:
///
/// * a numeric run containing a second `..`, or a dot cluster longer than two,
///   where the regex's greedy `([\d.]+)\.\.` would backtrack to the *last*
///   separator while this scan takes the first;
/// * non-ASCII digits, which the regex's Unicode-mode `\d` accepts;
/// * a separator that is Unicode whitespace, or the vertical tab that `\s`
///   accepts but `u8::is_ascii_whitespace` does not;
/// * a fractional `rows=`, which PostgreSQL 18 prints for `loops > 1` and
///   which `COST_REGEX` (`rows=(\d+)`) also refuses.
pub fn parse_cost_tuple(line: &[u8]) -> Option<CostTuple> {
    let start = find_literal_simd(line, b"(cost=")? + b"(cost=".len();

    // `min`: a run of digits and dots, terminated by the `..` separator.
    let mut i = start;
    while i < line.len()
        && (line[i].is_ascii_digit() || (line[i] == b'.' && line.get(i + 1) != Some(&b'.')))
    {
        i += 1;
    }
    if line.get(i) != Some(&b'.') || line.get(i + 1) != Some(&b'.') || i == start {
        return None;
    }
    // A dot cluster of three or more is the one case where taking the *first*
    // `..` is wrong: the regex's greedy `([\d.]+)\.\.` backtracks to the
    // *last* `..`, so `(cost=50...5 ..)` splits as `50.` / `5` for the regex
    // but `50` / `.5` here — a max cost off by a factor of ten, silently.
    // The scan loop above can only stop at the *start* of a cluster (it steps
    // over a '.' only when the next byte is not one), so the leading dot count
    // at `i` characterises the whole cluster and this one test settles it.
    if line.get(i + 2) == Some(&b'.') {
        return None;
    }
    let min: f64 = std::str::from_utf8(&line[start..i]).ok()?.parse().ok()?;

    // `max`: a run of digits and dots, with no second `..` inside it.
    let max_start = i + 2;
    let mut j = max_start;
    while j < line.len() && (line[j].is_ascii_digit() || line[j] == b'.') {
        j += 1;
    }
    let max_bytes = &line[max_start..j];
    if max_bytes.is_empty() || max_bytes.windows(2).any(|w| w == b"..") {
        return None;
    }
    let max: f64 = std::str::from_utf8(max_bytes).ok()?.parse().ok()?;

    let mut k = j;
    let rows = scan_labelled_int(line, &mut k, b"rows=")?;
    let width = scan_labelled_int(line, &mut k, b"width=")?;
    if line.get(k) != Some(&b')') {
        return None;
    }
    Some((min, max, rows, width.try_into().ok()?))
}

/// Consume `\s+`, then `label`, then `\d+`, advancing `pos` past the digits.
fn scan_labelled_int(line: &[u8], pos: &mut usize, label: &[u8]) -> Option<u64> {
    let mut k = *pos;
    let ws_start = k;
    while k < line.len() && line[k].is_ascii_whitespace() {
        k += 1;
    }
    if k == ws_start || !line.get(k..)?.starts_with(label) {
        return None;
    }
    k += label.len();
    let digits = k;
    while k < line.len() && line[k].is_ascii_digit() {
        k += 1;
    }
    if k == digits {
        return None;
    }
    let value = std::str::from_utf8(&line[digits..k]).ok()?.parse().ok()?;
    *pos = k;
    Some(value)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser_utils::LOG_LINE_PATTERN;
    use regex::Regex;

    /// The oracle: the regex the timestamp scanner replaces.
    fn oracle(re: &Regex, line: &str) -> Option<usize> {
        re.captures(line).map(|c| c.get(1).unwrap().as_str().len())
    }

    fn check_ts(re: &Regex, line: &str) {
        let want = oracle(re, line);
        for (name, got) in [
            ("simd", timestamp_prefix_len_simd(line.as_bytes())),
            ("scalar", timestamp_prefix_len_scalar(line.as_bytes())),
        ] {
            match got {
                // Deferring to the regex is always allowed; it is how the
                // Unicode corners stay exact.
                Verdict::Unsure => {}
                Verdict::Match(n) => assert_eq!(
                    Some(n),
                    want,
                    "{name}: group-1 length disagrees with the regex for {line:?}"
                ),
                Verdict::NoMatch => assert_eq!(
                    None, want,
                    "{name}: reported no match but the regex matched {line:?}"
                ),
            }
        }
    }

    #[test]
    fn timestamp_matches_the_regex_on_real_shapes() {
        let re = Regex::new(LOG_LINE_PATTERN).unwrap();
        for line in [
            // The shapes PostgreSQL actually emits.
            "2025-06-12 00:00:00.047 UTC [3416001] LOG:  duration: 11.370 ms  plan:",
            "2025-06-12 00:00:00 UTC [3416001] LOG:  checkpoint complete",
            "2025-06-12 00:00:00.047 +02 [1] LOG:  x",
            "2025-06-12 00:00:00.047 -05:30 [1] LOG:  x",
            "2025-06-12 00:00:00.047 -0530 [1] LOG:  x",
            "2025-06-12 00:00:00.123456 PDT [1] LOG:  x",
            "2025-06-12 00:00:00.047 [1] LOG:  no timezone token",
            // Exactly the boundaries of each quantifier.
            "2025-06-12 00:00:00.1 UTC x",
            "2025-06-12 00:00:00.1234567 UTC x",
            "2025-06-12 00:00:00. UTC x",
            "2025-06-12 00:00:00 A x",
            "2025-06-12 00:00:00 AB x",
            "2025-06-12 00:00:00 ABCDE x",
            "2025-06-12 00:00:00 ABCDEFG x",
            "2025-06-12 00:00:00 -05:3 x",
            "2025-06-12 00:00:00 -0 x",
            "2025-06-12 00:00:00 +0200x",
            "2025-06-12 00:00:00",
            // Non-matches, including the continuation lines that dominate.
            "\tQuery Text: SELECT 1",
            "\t  ->  Index Scan using \"ix\" on \"t\"  (cost=0.4..9.1 rows=1 width=8)",
            "",
            "2025-06-12",
            "2025-06-12 00:00:0",
            "2025-06-1200:00:00 UTC x",
            "20a5-06-12 00:00:00 UTC x",
            "2025-06-12T00:00:00 UTC x",
        ] {
            check_ts(&re, line);
        }
    }

    /// Non-ASCII digits are exactly the case the fast path must not decide on
    /// its own: the regex's `\d` matches them, a byte compare does not.
    /// The core check is the shared primitive behind both
    /// `timestamp_prefix_len_*` and `parser_utils::is_log_line_start`, so pin
    /// its two implementations to each other and to the regex directly rather
    /// than only through their callers.
    #[test]
    fn timestamp_core_implementations_agree() {
        let re = Regex::new(LOG_LINE_PATTERN).unwrap();
        let base = b"2025-06-12 00:00:00.047 UTC msg";
        let mut checked = 0usize;
        // Every truncation, then every single-byte value at every core offset.
        for len in 0..=base.len() {
            let slice = &base[..len];
            assert_eq!(
                timestamp_core_simd(slice),
                timestamp_core_scalar(slice),
                "core implementations disagree at length {len}"
            );
            checked += 1;
        }
        for pos in 0..TS_CORE_LEN {
            for byte in 0u16..=255 {
                let mut m = base.to_vec();
                m[pos] = byte as u8;
                let simd = timestamp_core_simd(&m);
                assert_eq!(
                    simd,
                    timestamp_core_scalar(&m),
                    "core implementations disagree at {pos} with {byte:#04x}"
                );
                // A definite core verdict must agree with the regex, which can
                // only match when the core does.
                if let Ok(text) = std::str::from_utf8(&m) {
                    match simd {
                        Verdict::Match(n) => {
                            assert_eq!(n, TS_CORE_LEN);
                            assert!(
                                re.is_match(text),
                                "core matched but the regex did not: {text:?}"
                            );
                        }
                        Verdict::NoMatch => assert!(
                            !re.is_match(text),
                            "core rejected but the regex matched: {text:?}"
                        ),
                        Verdict::Unsure => {}
                    }
                }
                checked += 1;
            }
        }
        assert!(checked > 4_800, "sweep did not run: {checked}");
    }

    #[test]
    fn timestamp_defers_on_non_ascii_digits() {
        let re = Regex::new(LOG_LINE_PATTERN).unwrap();
        for line in [
            "٢٠٢٥-٠٦-١٢ ٠٠:٠٠:٠٠ UTC x",
            "2025-06-12 00:00:00.٥ UTC x",
            "2025-06-12 00:00:00 +٠٥ x",
            "2025-06-12 00:00:00 +05:٣٠ x",
        ] {
            check_ts(&re, line);
        }
    }

    /// A generated corpus over the alphabet that actually appears around
    /// timestamps, to catch quantifier corners the hand-written cases miss.
    #[test]
    fn timestamp_differential_over_generated_corpus() {
        let re = Regex::new(LOG_LINE_PATTERN).unwrap();
        let alphabet = [
            "0", "9", "-", ":", " ", ".", "+", "A", "Z", "T", "[", "\t", "١",
        ];
        let base = "2025-06-12 00:00:00";
        // Every 3-token suffix appended to a valid core, plus the same tokens
        // spliced into the core itself.
        let mut state = 0x1234_5678u32;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as usize
        };
        for _ in 0..20_000 {
            let mut s = String::new();
            if rand() % 4 != 0 {
                s.push_str(base);
            }
            for _ in 0..(rand() % 8) {
                s.push_str(alphabet[rand() % alphabet.len()]);
            }
            check_ts(&re, &s);
        }
    }

    #[test]
    fn leading_whitespace_matches_char_iteration() {
        let cases: Vec<String> = [
            "".to_string(),
            "no indent".to_string(),
            " one".to_string(),
            "        eight".to_string(),
            "\t\ttabs".to_string(),
            " \t mixed".to_string(),
            " ".repeat(31) + "x",
            " ".repeat(32) + "x",
            " ".repeat(33) + "x",
            " ".repeat(64),
            "\u{a0}nbsp".to_string(),
            "   \u{a0}nbsp after spaces".to_string(),
            "   é".to_string(),
        ]
        .to_vec();
        for line in cases {
            let want = line.chars().take_while(|c| c.is_whitespace()).count();
            for (name, got) in [
                ("simd", leading_whitespace_simd(line.as_bytes())),
                ("scalar", leading_whitespace_scalar(line.as_bytes())),
            ] {
                if let Verdict::Match(n) = got {
                    // Equal only because the run is all ASCII, where byte
                    // count and char count coincide.
                    assert_eq!(n, want, "{name}: indent disagrees for {line:?}");
                }
            }
        }
    }

    #[test]
    fn literal_search_matches_naive() {
        let hay = concat!(
            "        ->  Bitmap Heap Scan on \"public\".\"tbl_3_c4\" c4  ",
            "(cost=12.15..870.04 rows=128 width=24)"
        );
        for needle in [
            &b"(cost="[..],
            b"rows=",
            b"Bitmap",
            b"width=24)",
            b"nowhere",
            b"c",
        ] {
            let want = hay
                .as_bytes()
                .windows(needle.len())
                .position(|w| w == needle);
            assert_eq!(find_literal_simd(hay.as_bytes(), needle), want);
            assert_eq!(find_literal_scalar(hay.as_bytes(), needle), want);
        }
    }

    #[test]
    fn literal_search_handles_short_and_empty_inputs() {
        assert_eq!(find_literal_simd(b"", b"x"), None);
        assert_eq!(find_literal_simd(b"abc", b""), None);
        assert_eq!(find_literal_simd(b"abc", b"abcd"), None);
        assert_eq!(find_literal_simd(b"abc", b"abc"), Some(0));
        // Straddles the 32-byte block boundary the AVX2 loop steps by.
        let hay = format!("{}(cost=", "x".repeat(30));
        assert_eq!(find_literal_simd(hay.as_bytes(), b"(cost="), Some(30));
    }

    /// The oracle for the cost parser: `COST_REGEX`, reproduced here so the
    /// test is self-contained.
    fn cost_oracle(re: &Regex, line: &str) -> Option<CostTuple> {
        let c = re.captures(line)?;
        Some((
            c["min"].parse().ok()?,
            c["max"].parse().ok()?,
            c["rows"].parse().ok()?,
            c["width"].parse().ok()?,
        ))
    }

    #[test]
    fn cost_tuple_matches_the_regex() {
        let re = Regex::new(
            r"\(cost=(?<min>[\d.]+)\.\.(?<max>[\d.]+)\s+rows=(?<rows>\d+)\s+width=(?<width>\d+)\)",
        )
        .unwrap();
        for line in [
            // The shapes auto_explain emits.
            r#"Index Scan Backward using "IX_a" on "public"."t" t  (cost=0.43..95610.13 rows=159718 width=56)"#,
            "Limit  (cost=0.43..599.04 rows=1000 width=56)",
            "Seq Scan on users  (cost=0.00..1.00 rows=1 width=4)",
            "  ->  Bitmap Heap Scan on c4  (cost=12.15..870.04 rows=128 width=24)",
            // Integer costs, and multi-space separators.
            "Result  (cost=0..1 rows=1 width=0)",
            "Result  (cost=0.00..1.00  rows=1  width=0)",
            // With ANALYZE actuals appended after the cost tuple.
            "Limit  (cost=0.43..599.04 rows=1000 width=56) (actual time=0.012..0.034 rows=10 loops=1)",
            // Non-matches.
            "Output: a, b, c",
            "Filter: (x = 1)",
            "(cost=0.43 rows=1 width=8)",
            "(cost=0.43..1.00 rows= width=8)",
            "(cost=0.43..1.00 rows=1 width=8",
            "(cost=..1.00 rows=1 width=8)",
            "",
        ] {
            assert_eq!(
                parse_cost_tuple(line.as_bytes()),
                cost_oracle(&re, line),
                "cost tuple disagrees with the regex on {line:?}"
            );
        }
    }

    /// Inputs where declining is the correct answer: the scan must return
    /// `None` so the caller falls back, and must never report a tuple the
    /// regex would not have produced.
    #[test]
    fn cost_tuple_declines_rather_than_guessing() {
        let re = Regex::new(
            r"\(cost=(?<min>[\d.]+)\.\.(?<max>[\d.]+)\s+rows=(?<rows>\d+)\s+width=(?<width>\d+)\)",
        )
        .unwrap();
        for line in [
            // PostgreSQL 18 prints a fractional per-loop `rows`; COST_REGEX
            // refuses it, so this must not silently truncate to an integer.
            "Limit  (cost=0.43..599.04 rows=1000.50 width=56)",
            // A second `..` makes the regex's greedy `[\d.]+` backtrack to the
            // last separator; this scan takes the first, so it must decline.
            "Limit  (cost=0.43..599..04 rows=1000 width=56)",
            // Regression: a cluster of exactly three dots. The scan used to
            // take dots 1-2 as the separator and leave dot 3 as the first byte
            // of `max`, where the "no second `..`" guard could not see it —
            // reporting max=0.5 where the regex says 5.0, and turning an
            // `InvalidCostFormat` error into an invented value.
            "Seq Scan on t  (cost=50...5 rows=7 width=8)",
            "Seq Scan on t  (cost=.0...5 rows=7 width=8)",
            "Limit  (cost=1...2 rows=1 width=8)",
            "Limit  (cost=0.43...599 rows=1000 width=56)",
            "Limit  (cost=1....2 rows=1 width=8)",
            // Unicode digits, which the regex's `\d` accepts.
            "Limit  (cost=٠.٤٣..٥٩٩.٠٤ rows=١٠٠٠ width=٥٦)",
            // Vertical tab is `\s` to the regex but not to
            // `u8::is_ascii_whitespace`.
            "Limit  (cost=0.43..599.04\u{0b}rows=1000 width=56)",
        ] {
            let got = parse_cost_tuple(line.as_bytes());
            assert!(
                got.is_none() || got == cost_oracle(&re, line),
                "cost tuple guessed {got:?} on an input it should decline: {line:?}"
            );
        }
    }

    /// Exhaustive differential coverage of the numeric run itself.
    ///
    /// Every string over `{'0', '5', '.'}` up to length 8 is substituted into a
    /// well-formed cost tuple and compared against the regex. This is the test
    /// that would have caught the three-dot separator bug: a random token walk
    /// almost never assembles `(cost=` `d` `...` `d` ` rows=` ... in one draw,
    /// so the hazard needs enumeration rather than sampling.
    #[test]
    fn cost_tuple_differential_over_exhaustive_numeric_runs() {
        let re = Regex::new(
            r"\(cost=(?<min>[\d.]+)\.\.(?<max>[\d.]+)\s+rows=(?<rows>\d+)\s+width=(?<width>\d+)\)",
        )
        .unwrap();
        let alphabet = [b'0', b'5', b'.'];
        let mut run = Vec::new();
        let mut checked = 0usize;
        for len in 0..=8 {
            // Enumerate every string of this length by counting in base 3.
            let total = 3usize.pow(len as u32);
            for n in 0..total {
                run.clear();
                let mut m = n;
                for _ in 0..len {
                    run.push(alphabet[m % 3]);
                    m /= 3;
                }
                let body = std::str::from_utf8(&run).unwrap();
                let line = format!("Seq Scan on t  (cost={body} rows=7 width=8)");
                if let Some(tuple) = parse_cost_tuple(line.as_bytes()) {
                    assert_eq!(
                        Some(tuple),
                        cost_oracle(&re, &line),
                        "cost tuple reported a value the regex does not: {line:?}"
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 9_000, "enumeration did not run: {checked}");
    }

    /// Generated differential coverage over the alphabet that appears inside a
    /// cost tuple, to catch structural corners the hand-written cases miss.
    #[test]
    fn cost_tuple_differential_over_generated_corpus() {
        let re = Regex::new(
            r"\(cost=(?<min>[\d.]+)\.\.(?<max>[\d.]+)\s+rows=(?<rows>\d+)\s+width=(?<width>\d+)\)",
        )
        .unwrap();
        let toks = [
            "(cost=", "0", "7", ".", "..", " ", "  ", "rows=", "width=", ")", "\t", "x", "\u{0b}",
        ];
        let mut state = 0x9E37_79B9u32;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as usize
        };
        for _ in 0..30_000 {
            let mut s = String::from("Seq Scan on t  ");
            for _ in 0..(rand() % 12) {
                s.push_str(toks[rand() % toks.len()]);
            }
            let got = parse_cost_tuple(s.as_bytes());
            if let Some(tuple) = got {
                assert_eq!(
                    Some(tuple),
                    cost_oracle(&re, &s),
                    "cost tuple reported a value the regex does not: {s:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod sweep_tests {
    use super::*;

    const MUTANTS: [u8; 12] = [
        0, b'0', b'9', b'/', b':', b'-', b' ', b'a', 0x7F, 0x80, 0xA0, 0xFF,
    ];

    fn assert_timestamp_agrees(line: &[u8]) {
        let want = timestamp_core_scalar(line);
        assert_eq!(timestamp_core_simd(line), want, "core: {line:?}");
        assert_eq!(
            timestamp_prefix_len_simd(line),
            timestamp_prefix_len_scalar(line),
            "prefix: {line:?}"
        );
    }

    #[test]
    fn timestamp_core_matches_scalar_for_every_byte_at_every_offset() {
        let base = b"2025-06-12 00:00:00.047 UTC [1] LOG:  x";
        assert_eq!(timestamp_core_simd(base), Verdict::Match(TS_CORE_LEN));
        for len in 0..=base.len() {
            assert_timestamp_agrees(&base[..len]);
        }
        for position in 0..base.len() {
            for byte in 0..=u8::MAX {
                let mut line = base.to_vec();
                line[position] = byte;
                assert_timestamp_agrees(&line);
            }
        }
    }

    /// Two simultaneous mutations, so a non-ASCII byte in one window and a
    /// shape error in the other must still report `Unsure` like the scalar.
    #[test]
    fn timestamp_core_matches_scalar_under_paired_mutation() {
        let base = b"2025-06-12 00:00:00 UTC";
        for first in 0..TS_CORE_LEN {
            for second in first + 1..TS_CORE_LEN {
                for first_byte in MUTANTS {
                    for second_byte in MUTANTS {
                        let mut line = base.to_vec();
                        line[first] = first_byte;
                        line[second] = second_byte;
                        assert_timestamp_agrees(&line);
                    }
                }
            }
        }
    }

    #[test]
    fn leading_whitespace_matches_scalar_for_every_run_length_and_terminator() {
        let terminators: [&[u8]; 6] = [
            b"x",
            b"\xC2\xA0",
            b"->",
            b"\xE2\x80\xA8",
            b"",
            b"\t\x0B\x0C\r",
        ];
        // What follows the terminator must not matter, including when the
        // overlapping final block re-reads it.
        let suffixes: [&[u8]; 3] = [b"", b"  (cost=1..2 rows=1 width=4)", b"\xFF \t"];
        for run in 0..140 {
            for terminator in terminators {
                for suffix in suffixes {
                    for filler in [b' ', b'\t', b'\r'] {
                        let mut line = vec![filler; run];
                        line.extend_from_slice(terminator);
                        line.extend_from_slice(suffix);
                        let want = leading_whitespace_scalar(&line);
                        assert_eq!(
                            leading_whitespace_simd(&line),
                            want,
                            "run {run}, terminator {terminator:?}, suffix {suffix:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn leading_whitespace_classifies_every_byte_like_scalar() {
        for position in 0..40 {
            for byte in 0..=u8::MAX {
                let mut line = vec![b' '; 40];
                line[position] = byte;
                assert_eq!(
                    leading_whitespace_simd(&line),
                    leading_whitespace_scalar(&line),
                    "byte {byte:#04x} at {position}"
                );
            }
        }
    }

    #[test]
    fn find_literal_matches_scalar_at_every_position_and_length() {
        let needles: [&[u8]; 5] = [b"(cost=", b"c", b"ab", b"aXa", b"(cost=0.00..1.00 rows"];
        for needle in needles {
            for len in 0..72 {
                for position in 0..=len {
                    // Decoys share the needle's first and last byte, so they
                    // pass the vector filter and must be rejected by the
                    // full comparison.
                    let mut haystack = vec![b'.'; len];
                    for decoy in (0..len).step_by(5) {
                        haystack[decoy] = needle[0];
                        if decoy + needle.len() <= len {
                            haystack[decoy + needle.len() - 1] = needle[needle.len() - 1];
                        }
                    }
                    if position + needle.len() <= len {
                        haystack[position..position + needle.len()].copy_from_slice(needle);
                    }
                    let want = find_literal_scalar(&haystack, needle);
                    assert_eq!(
                        find_literal_simd(&haystack, needle),
                        want,
                        "needle {needle:?} in {haystack:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn find_literal_handles_short_and_empty_inputs() {
        assert_eq!(find_literal_simd(b"", b"x"), None);
        assert_eq!(find_literal_simd(b"abc", b""), None);
        assert_eq!(find_literal_simd(b"abc", b"abcd"), None);
        assert_eq!(find_literal_simd(b"abc", b"abc"), Some(0));
        let hay = format!("{}(cost=", "x".repeat(30));
        assert_eq!(find_literal_simd(hay.as_bytes(), b"(cost="), Some(30));
    }
}

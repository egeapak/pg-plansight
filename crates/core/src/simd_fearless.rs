//! `fearless_simd` ports of the hot-path scanners in [`crate::simd_scan`].
//!
//! One generic kernel per scanner replaces the per-architecture intrinsics
//! bodies, with no `unsafe`. Behaviour must stay identical to the hand-written
//! versions, including [`Verdict::Unsure`] on non-ASCII bytes.
//!
//! Every kernel works on fixed 16-byte blocks and dispatches on
//! [`Level::baseline`] (SSE2 on x86_64, NEON on AArch64), which resolves at
//! compile time: no feature-detection load or level match per call, and no
//! AVX2/AVX-512 even where available. The inputs are a 19-byte timestamp, an
//! indentation run and a plan line, so wider native vectors (32 or 64 lanes)
//! would mostly push lines onto the scalar tail rather than save iterations.

use crate::simd_scan::Verdict;
use fearless_simd::{Level, Select, Simd, SimdBase, SimdMask, dispatch, i8x16, mask8x16, u8x16};

const BLOCK: usize = 16;

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

/// `fearless_simd` equivalent of [`crate::simd_scan::timestamp_core_simd`].
#[inline]
pub fn timestamp_core_fearless(line: &[u8]) -> Verdict {
    match line.first_chunk::<TS_CORE_LEN>() {
        Some(core) => dispatch!(Level::baseline(), simd => ts_core_kernel(simd, core)),
        None => Verdict::NoMatch,
    }
}

/// `fearless_simd` equivalent of [`crate::simd_scan::timestamp_prefix_len_simd`].
#[inline]
pub fn timestamp_prefix_len_fearless(line: &[u8]) -> Verdict {
    match timestamp_core_fearless(line) {
        Verdict::Match(_) => crate::simd_scan::timestamp_tail(line),
        other => other,
    }
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
    match crate::simd_scan::leading_whitespace_scalar(&line[offset..]) {
        Verdict::Match(n) => Verdict::Match(offset + n),
        other => other,
    }
}

/// `fearless_simd` equivalent of [`crate::simd_scan::leading_whitespace_simd`].
#[inline]
pub fn leading_whitespace_fearless(line: &[u8]) -> Verdict {
    dispatch!(Level::baseline(), simd => leading_ws_kernel(simd, line))
}

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
    crate::simd_scan::find_literal_scalar(&haystack[offset..], needle).map(|found| offset + found)
}

/// `fearless_simd` equivalent of [`crate::simd_scan::find_literal_simd`].
#[inline]
pub fn find_literal_fearless(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    dispatch!(Level::baseline(), simd => find_literal_kernel(simd, haystack, needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simd_scan::{
        find_literal_scalar, find_literal_simd, leading_whitespace_scalar, leading_whitespace_simd,
        timestamp_core_scalar, timestamp_core_simd, timestamp_prefix_len_simd,
    };

    const MUTANTS: [u8; 12] = [
        0, b'0', b'9', b'/', b':', b'-', b' ', b'a', 0x7F, 0x80, 0xA0, 0xFF,
    ];

    fn assert_timestamp_agrees(line: &[u8]) {
        let want = timestamp_core_scalar(line);
        assert_eq!(timestamp_core_fearless(line), want, "core: {line:?}");
        assert_eq!(timestamp_core_simd(line), want, "hand core: {line:?}");
        assert_eq!(
            timestamp_prefix_len_fearless(line),
            timestamp_prefix_len_simd(line),
            "prefix: {line:?}"
        );
    }

    #[test]
    fn timestamp_core_matches_scalar_for_every_byte_at_every_offset() {
        let base = b"2025-06-12 00:00:00.047 UTC [1] LOG:  x";
        assert_eq!(timestamp_core_fearless(base), Verdict::Match(TS_CORE_LEN));
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
                            leading_whitespace_fearless(&line),
                            want,
                            "run {run}, terminator {terminator:?}, suffix {suffix:?}"
                        );
                        assert_eq!(leading_whitespace_simd(&line), want, "hand: {line:?}");
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
                    leading_whitespace_fearless(&line),
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
                        find_literal_fearless(&haystack, needle),
                        want,
                        "needle {needle:?} in {haystack:?}"
                    );
                    assert_eq!(find_literal_simd(&haystack, needle), want, "hand");
                }
            }
        }
    }

    #[test]
    fn find_literal_handles_short_and_empty_inputs() {
        assert_eq!(find_literal_fearless(b"", b"x"), None);
        assert_eq!(find_literal_fearless(b"abc", b""), None);
        assert_eq!(find_literal_fearless(b"abc", b"abcd"), None);
        assert_eq!(find_literal_fearless(b"abc", b"abc"), Some(0));
        let hay = format!("{}(cost=", "x".repeat(30));
        assert_eq!(find_literal_fearless(hay.as_bytes(), b"(cost="), Some(30));
    }
}

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
//! 2. **Vectorisation**: doing the byte comparisons 16 or 32 at a time. This is
//!    the smaller factor on data this short.
//!
//! Both a `_scalar` and a `_simd` implementation are therefore provided for
//! every scanner, and `benches/simd_candidates.rs` measures both tiers
//! (regex / scalar / SIMD) so the split between the two effects is visible
//! rather than assumed.
//!
//! # Correctness
//!
//! `timestamp_prefix_len_simd`, `parse_cost_tuple` and `leading_whitespace_simd`
//! are on the parser's hot path (`split_log_line`, `extract_cost`,
//! `get_indent_level`); `find_ascii_ci_*` is not, and exists as the evidence
//! behind a measured negative result. A defect in the first three silently
//! corrupts parsed plans rather than merely slowing them down, so exactness is
//! the binding constraint, not a nicety.
//!
//! Each scanner is required to be *exactly* equivalent to the regex or
//! `char`-based implementation it shadows, including the awkward corners
//! (`\d` matching Unicode digits, greedy `[A-Z]{2,5}`, `(?::?\d{2})?`
//! backtracking). The tests at the bottom of this file assert that
//! equivalence differentially, over both hand-written corner cases and a
//! generated corpus. Where the fast path cannot be certain — the input is not
//! ASCII where the regex would consult Unicode tables — it reports
//! [`Verdict::Unsure`] and the caller is expected to defer to the regex, which
//! keeps behaviour identical by construction.
//!
//! # Portability
//!
//! Two vector back-ends, chosen at compile time by `target_arch`:
//!
//! * **x86_64** — SSE2 for the 16-byte timestamp core (baseline on the target,
//!   so no detection), AVX2 for the 32-byte-at-a-time scanners behind a
//!   runtime `is_x86_feature_detected!` check.
//! * **aarch64** — NEON for the three scanners on the hot path (timestamp
//!   core, leading whitespace, literal search). `find_ascii_ci_*` has no NEON
//!   path and uses the scalar one: it is not on the hot path, and §3.2 of
//!   `docs/SIMD_ANALYSIS.md` measured its vectorised form as a loss anyway.
//!   No runtime detection is needed — `target_feature = "neon"` is in the
//!   default cfg set for `aarch64-unknown-linux-gnu` (and the other AArch64
//!   ABI targets), exactly as SSE2 is for x86_64. AArch64 has no `movemask`,
//!   so where the x86 code extracts a bitmask and compares it against a
//!   constant, the NEON code either merges lanes with a bitwise select and
//!   takes a horizontal minimum, or narrows the comparison result to one
//!   nibble per lane (`vshrn_n_u16` by 4) to get an equivalent scannable
//!   mask.
//!
//! 32-bit `arm` is deliberately *not* covered: its NEON intrinsics are still
//! unstable in `core::arch`, and NEON is optional rather than architectural
//! there. It takes the scalar path, as does every other architecture, so
//! behaviour is identical everywhere and only throughput differs.

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

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

/// Byte offsets 0..16 of `YYYY-MM-DD HH:MM` that must hold an ASCII digit.
/// Bits {0,1,2,3,5,6,8,9,11,12,14,15}.
#[cfg(target_arch = "x86_64")]
const TS_DIGIT_MASK: u16 = 0xDB6F;
/// Byte offsets 0..16 that must hold a literal separator: 4 and 7 are `-`,
/// 10 is a space, 13 is `:`. Bits {4,7,10,13}.
#[cfg(target_arch = "x86_64")]
const TS_SEP_MASK: u16 = 0x2490;

/// Lane selector for the same 16-byte window on AArch64: `0xFF` where the byte
/// must be an ASCII digit, `0x00` where it must be a literal separator. Every
/// one of the 16 lanes is one or the other, so a single bitwise select merges
/// the two comparison results.
#[cfg(target_arch = "aarch64")]
const TS_DIGIT_SELECT: [u8; 16] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0xFF,
];

/// The literal separators at offsets 4, 7, 10 and 13. Lanes the selector above
/// marks as digit positions are ignored, so their template value is arbitrary.
#[cfg(target_arch = "aarch64")]
const TS_SEP_TEMPLATE: [u8; 16] = [0, 0, 0, 0, b'-', 0, 0, b'-', 0, 0, b' ', 0, 0, b':', 0, 0];

/// Length of the mandatory `YYYY-MM-DD HH:MM:SS` core.
const TS_CORE_LEN: usize = 19;

/// Match the timestamp that a `%m`/`%t` `log_line_prefix` puts at the start of
/// a line, returning the length of what
/// [`LOG_LINE_PATTERN`](crate::parser_utils::LOG_LINE_PATTERN) captures as
/// group 1 — i.e. the split point between the timestamp and the message.
///
/// The pattern being replicated is
///
/// ```text
/// ^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(?:\.\d{1,6})?(?: (?:[A-Z]{2,5}|[+-]\d{2}(?::?\d{2})?))?)
/// ```
///
/// The fixed 19-byte core is checked with one 16-byte vector compare plus
/// three scalar bytes; the two optional tails (fractional seconds, timezone
/// token) are short and variable, so they stay scalar.
#[inline]
pub fn timestamp_prefix_len_simd(line: &[u8]) -> Verdict {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is part of the x86_64 baseline, so the intrinsics used
        // by `ts_core_sse2` are always available on this target. It reads
        // exactly 16 bytes, which the length check below guarantees exist.
        if line.len() < TS_CORE_LEN {
            Verdict::NoMatch
        } else {
            match unsafe { ts_core_sse2(line) } {
                Verdict::Match(_) => timestamp_tail(line),
                other => other,
            }
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: `target_feature = "neon"` is in the default cfg set for the
        // AArch64 targets, so `ts_core_neon`'s intrinsics are always available
        // here. It reads exactly 16 bytes, which the length check guarantees.
        if line.len() < TS_CORE_LEN {
            Verdict::NoMatch
        } else {
            match unsafe { ts_core_neon(line) } {
                Verdict::Match(_) => timestamp_tail(line),
                other => other,
            }
        }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        timestamp_prefix_len_scalar(line)
    }
}

/// Scalar equivalent of [`timestamp_prefix_len_simd`], byte-at-a-time.
///
/// Kept as a first-class implementation rather than a fallback: benchmarking
/// it against the SIMD version is what separates "we stopped running a regex"
/// from "we vectorised the comparison".
#[inline]
pub fn timestamp_prefix_len_scalar(line: &[u8]) -> Verdict {
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
    timestamp_tail(line)
}

/// Validate the fixed 19-byte `YYYY-MM-DD HH:MM:SS` core.
///
/// One unaligned 16-byte load covers `YYYY-MM-DD HH:MM`; the digit test and
/// the separator test each collapse to a single `movemask` compared against a
/// constant, so the whole check is a handful of instructions instead of 19
/// dependent scalar compares. The trailing `:SS` is done scalar.
///
/// Returns `Match(TS_CORE_LEN)` — the tail is the caller's job.
///
/// # Safety
///
/// Requires `line.len() >= 19`, since it performs a 16-byte unaligned load
/// from the start of `line` and then indexes bytes 16..19 directly. SSE2 is
/// baseline on `x86_64`, so no feature detection is needed.
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn ts_core_sse2(line: &[u8]) -> Verdict {
    debug_assert!(line.len() >= TS_CORE_LEN);
    // SAFETY: caller guarantees at least 19 readable bytes, so a 16-byte
    // unaligned load starting at offset 0 is in bounds.
    let x = unsafe { _mm_loadu_si128(line.as_ptr() as *const __m128i) };

    // Any byte with its high bit set is non-ASCII. `movemask` gathers exactly
    // those sign bits, so one compare against zero screens the whole window.
    if unsafe { _mm_movemask_epi8(x) } != 0 {
        return Verdict::Unsure;
    }
    if !line[16..TS_CORE_LEN].iter().all(|b| b.is_ascii()) {
        return Verdict::Unsure;
    }

    // Digit test, unsigned: `b - b'0'` lands in 0..=9 exactly for ASCII
    // digits, and wraps to a large value for anything below '0', so
    // `min(sub, 9) == sub` is true only for digits.
    let sub = unsafe { _mm_sub_epi8(x, _mm_set1_epi8(0x30)) };
    let is_digit = unsafe { _mm_cmpeq_epi8(_mm_min_epu8(sub, _mm_set1_epi8(9)), sub) };
    let digits = unsafe { _mm_movemask_epi8(is_digit) } as u16;
    if digits & TS_DIGIT_MASK != TS_DIGIT_MASK {
        return Verdict::NoMatch;
    }

    // Separator test: compare against a template holding the expected literal
    // at each separator offset. Non-separator lanes are ignored by the mask.
    let template = unsafe {
        _mm_setr_epi8(
            0, 0, 0, 0, b'-' as i8, 0, 0, b'-' as i8, 0, 0, b' ' as i8, 0, 0, b':' as i8, 0, 0,
        )
    };
    let seps = unsafe { _mm_movemask_epi8(_mm_cmpeq_epi8(x, template)) } as u16;
    if seps & TS_SEP_MASK != TS_SEP_MASK {
        return Verdict::NoMatch;
    }

    // Bytes 16..19 are `:SS`, outside the 16-byte window.
    if line[16] != b':' || !line[17].is_ascii_digit() || !line[18].is_ascii_digit() {
        return Verdict::NoMatch;
    }
    Verdict::Match(TS_CORE_LEN)
}

/// NEON counterpart of [`ts_core_sse2`].
///
/// AArch64 has no `movemask`, so rather than extract a bitmask and compare it
/// against a constant, the digit and separator results are merged with a
/// bitwise select — every one of the 16 lanes is constrained by exactly one of
/// the two tests — and the merged vector is required to be all-ones via a
/// single horizontal minimum.
///
/// Returns `Match(TS_CORE_LEN)`; the tail is the caller's job.
///
/// # Safety
///
/// Requires `line.len() >= 19`, since it performs a 16-byte unaligned load
/// from the start of `line` and then indexes bytes 16..19 directly. NEON is
/// part of the default cfg set for the AArch64 targets, so like SSE2 on
/// x86_64 it needs no feature detection.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn ts_core_neon(line: &[u8]) -> Verdict {
    debug_assert!(line.len() >= TS_CORE_LEN);
    // SAFETY: caller guarantees at least 19 readable bytes, so a 16-byte
    // unaligned load starting at offset 0 is in bounds.
    let x = unsafe { vld1q_u8(line.as_ptr()) };

    // Any byte with its high bit set is non-ASCII; one horizontal maximum
    // screens the whole window.
    if vmaxvq_u8(x) >= 0x80 {
        return Verdict::Unsure;
    }
    if !line[16..TS_CORE_LEN].iter().all(|b| b.is_ascii()) {
        return Verdict::Unsure;
    }

    // Digit test, unsigned: `b - b'0'` lands in 0..=9 exactly for ASCII digits
    // and wraps to a large value for anything below '0'.
    let sub = vsubq_u8(x, vdupq_n_u8(b'0'));
    let is_digit = vcleq_u8(sub, vdupq_n_u8(9));
    // SAFETY: both constants are exactly 16 bytes.
    let is_sep = vceqq_u8(x, unsafe { vld1q_u8(TS_SEP_TEMPLATE.as_ptr()) });
    let select = unsafe { vld1q_u8(TS_DIGIT_SELECT.as_ptr()) };

    // Take the digit result where a digit is required and the separator result
    // where a separator is; every lane must then be all-ones.
    if vminvq_u8(vbslq_u8(select, is_digit, is_sep)) != 0xFF {
        return Verdict::NoMatch;
    }

    // Bytes 16..19 are `:SS`, outside the 16-byte window.
    if line[16] != b':' || !line[17].is_ascii_digit() || !line[18].is_ascii_digit() {
        return Verdict::NoMatch;
    }
    Verdict::Match(TS_CORE_LEN)
}

/// Collapse a 16-lane all-ones/all-zeroes comparison result to one nibble per
/// lane, packed into a `u64` — AArch64's stand-in for x86's `movemask`.
///
/// `vshrn_n_u16(v, 4)` narrows the eight `u16` lanes to `u8`, taking bits
/// 11..4 of each. For a comparison result that is 0xFF or 0x00 per byte, that
/// leaves the low nibble carrying the even byte and the high nibble the odd
/// one, so nibble *i* of the `u64` corresponds to input byte *i* and
/// `trailing_zeros() / 4` locates a lane.
///
/// # Safety
///
/// Carries `#[target_feature(enable = "neon")]` so its callers (which have the
/// same attribute) can invoke it; it dereferences nothing itself.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon_mask_nibbles(v: uint8x16_t) -> u64 {
    let narrowed = vshrn_n_u16::<4>(vreinterpretq_u16_u8(v));
    vget_lane_u64::<0>(vreinterpret_u64_u8(narrowed))
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

/// Count leading whitespace, equivalently to
/// `line.chars().take_while(|c| c.is_whitespace()).count()`.
///
/// The current implementation decodes UTF-8 one `char` at a time to count what
/// is, on every real plan line, a run of ASCII spaces. This compares 32 bytes
/// per iteration instead.
///
/// Returns [`Verdict::Unsure`] when the run is stopped by a non-ASCII byte,
/// since `char::is_whitespace` is true for several non-ASCII code points
/// (U+00A0, U+2028, ...) and deciding would mean decoding after all.
#[inline]
pub fn leading_whitespace_simd(line: &[u8]) -> Verdict {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: guarded by the runtime AVX2 check immediately above.
            return unsafe { leading_ws_avx2(line) };
        }
        leading_whitespace_scalar(line)
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is in the default cfg set for the AArch64 targets.
        unsafe { leading_ws_neon(line) }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        leading_whitespace_scalar(line)
    }
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

/// # Safety
///
/// The caller must have verified AVX2 support at runtime.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn leading_ws_avx2(line: &[u8]) -> Verdict {
    let mut i = 0;
    // Full 32-byte blocks. Whitespace is `' '` or the contiguous range
    // 0x09..=0x0D, so two compares and an OR classify all 32 lanes at once.
    while i + 32 <= line.len() {
        // SAFETY: the loop condition guarantees 32 readable bytes at `i`.
        let x = unsafe { _mm256_loadu_si256(line.as_ptr().add(i) as *const __m256i) };
        let is_space = _mm256_cmpeq_epi8(x, _mm256_set1_epi8(b' ' as i8));
        // Unsigned range test for 0x09..=0x0D: `b - 9` is in 0..=4 exactly for
        // that range, and wraps large below it.
        let sub = _mm256_sub_epi8(x, _mm256_set1_epi8(9));
        let in_ctl = _mm256_cmpeq_epi8(_mm256_min_epu8(sub, _mm256_set1_epi8(4)), sub);
        let ws = _mm256_or_si256(is_space, in_ctl);
        // Lanes that are *not* whitespace; the first such lane ends the run.
        let mask = !(_mm256_movemask_epi8(ws) as u32);
        if mask != 0 {
            let off = i + mask.trailing_zeros() as usize;
            return if line[off].is_ascii() {
                Verdict::Match(off)
            } else {
                Verdict::Unsure
            };
        }
        i += 32;
    }
    // Tail shorter than a vector.
    match leading_whitespace_scalar(&line[i..]) {
        Verdict::Match(n) => Verdict::Match(i + n),
        other => other,
    }
}

/// NEON counterpart of [`leading_ws_avx2`], 16 bytes at a time.
///
/// # Safety
///
/// Only the unaligned loads are unsafe, and each is bounded by the loop
/// condition. NEON needs no feature detection on AArch64.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn leading_ws_neon(line: &[u8]) -> Verdict {
    let mut i = 0;
    while i + 16 <= line.len() {
        // SAFETY: the loop condition guarantees 16 readable bytes at `i`.
        let x = unsafe { vld1q_u8(line.as_ptr().add(i)) };
        let is_space = vceqq_u8(x, vdupq_n_u8(b' '));
        // Unsigned range test for 0x09..=0x0D, as in the AVX2 version.
        let sub = vsubq_u8(x, vdupq_n_u8(9));
        let in_ctl = vcleq_u8(sub, vdupq_n_u8(4));
        let ws = vorrq_u8(is_space, in_ctl);
        // The nibble mask is all-ones exactly when every lane is whitespace,
        // so it answers the guard too — no separate horizontal reduction.
        // SAFETY: this function carries the `neon` target feature.
        let mask = unsafe { neon_mask_nibbles(ws) };
        if mask != u64::MAX {
            // `mask != u64::MAX` guarantees `!mask != 0`, so `trailing_zeros`
            // is below 64 and the lane index below 16.
            let off = i + (!mask).trailing_zeros() as usize / 4;
            return if line[off].is_ascii() {
                Verdict::Match(off)
            } else {
                Verdict::Unsure
            };
        }
        i += 16;
    }
    match leading_whitespace_scalar(&line[i..]) {
        Verdict::Match(n) => Verdict::Match(i + n),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// 3. Literal search (`(cost=` plan-node detection)
// ---------------------------------------------------------------------------

/// Find the first occurrence of `needle` in `haystack`.
///
/// This is the primitive behind detecting whether a plan line is a node line
/// (`plan_regex.is_match`, looking for the `(cost=` shape). The classic
/// "generic SIMD" filter is used: broadcast the first and last needle bytes,
/// compare both against 32-byte windows, and only run a full `memcmp` where
/// both agree — which for a 6-byte needle over log text is almost never.
#[inline]
pub fn find_literal_simd(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if needle.len() >= 2
            && haystack.len() >= needle.len() + 32
            && std::is_x86_feature_detected!("avx2")
        {
            // SAFETY: guarded by the runtime AVX2 check and the length checks
            // required by `find_literal_avx2`.
            return unsafe { find_literal_avx2(haystack, needle) };
        }
        find_literal_scalar(haystack, needle)
    }
    #[cfg(target_arch = "aarch64")]
    {
        if needle.len() >= 2 && haystack.len() >= needle.len() + 16 {
            // SAFETY: NEON is in the default cfg set for the AArch64 targets,
            // and this length check is what `find_literal_neon` requires.
            return unsafe { find_literal_neon(haystack, needle) };
        }
        find_literal_scalar(haystack, needle)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        find_literal_scalar(haystack, needle)
    }
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

/// # Safety
///
/// The caller must have verified AVX2 support at runtime, and must ensure
/// `needle.len() >= 2` and `haystack.len() >= needle.len() + 32`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn find_literal_avx2(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let n = needle.len();
    let first = _mm256_set1_epi8(needle[0] as i8);
    let last = _mm256_set1_epi8(needle[n - 1] as i8);
    let end = haystack.len() - n;

    let mut i = 0;
    while i + 32 <= end + 1 {
        // SAFETY: `i + 32 <= end + 1` and `end = len - n`, so both loads —
        // the second offset by `n - 1` — stay within `haystack`.
        let block_first = unsafe { _mm256_loadu_si256(haystack.as_ptr().add(i) as *const __m256i) };
        let block_last =
            unsafe { _mm256_loadu_si256(haystack.as_ptr().add(i + n - 1) as *const __m256i) };

        // A candidate needs its first *and* last byte to line up. Requiring
        // both makes false positives rare enough that the memcmp below is off
        // the hot path.
        let eq_first = _mm256_cmpeq_epi8(first, block_first);
        let eq_last = _mm256_cmpeq_epi8(last, block_last);
        let mut mask = _mm256_movemask_epi8(_mm256_and_si256(eq_first, eq_last)) as u32;

        while mask != 0 {
            let off = i + mask.trailing_zeros() as usize;
            if &haystack[off..off + n] == needle {
                return Some(off);
            }
            mask &= mask - 1;
        }
        i += 32;
    }
    // Remainder.
    find_literal_scalar(&haystack[i..], needle).map(|off| i + off)
}

/// NEON counterpart of [`find_literal_avx2`], 16 bytes at a time.
///
/// # Safety
///
/// The caller must ensure `needle.len() >= 2` and
/// `haystack.len() >= needle.len() + 16`.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn find_literal_neon(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let n = needle.len();
    let first = vdupq_n_u8(needle[0]);
    let last = vdupq_n_u8(needle[n - 1]);
    let end = haystack.len() - n;

    let mut i = 0;
    while i + 16 <= end + 1 {
        // SAFETY: `i + 16 <= end + 1` with `end = len - n`, so both loads —
        // the second offset by `n - 1` — stay within `haystack`.
        let block_first = unsafe { vld1q_u8(haystack.as_ptr().add(i)) };
        let block_last = unsafe { vld1q_u8(haystack.as_ptr().add(i + n - 1)) };
        let candidates = vandq_u8(vceqq_u8(block_first, first), vceqq_u8(block_last, last));
        let mut mask = unsafe { neon_mask_nibbles(candidates) };
        while mask != 0 {
            let lane = mask.trailing_zeros() as usize / 4;
            let off = i + lane;
            if &haystack[off..off + n] == needle {
                return Some(off);
            }
            // Clear the whole nibble: `mask &= mask - 1` would clear one bit
            // of it and re-report the same lane.
            mask &= !(0xF_u64 << (lane * 4));
        }
        i += 16;
    }
    find_literal_scalar(&haystack[i..], needle).map(|off| i + off)
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
// 5. ASCII case-insensitive search (node-type classification)
// ---------------------------------------------------------------------------

/// Case-insensitively find `needle` (which must already be ASCII lowercase) in
/// `haystack`, without allocating.
///
/// This is the primitive behind the node-type classification chain in
/// `plan_parser`, which currently does `line.to_lowercase()` — a Unicode-aware
/// transform that allocates a fresh `String` — and then runs up to eight
/// `contains()` calls against it. Both the allocation and the Unicode tables
/// are avoidable when the haystack is ASCII, which plan node lines are unless
/// the schema uses non-ASCII identifiers.
///
/// Returns [`Verdict::Unsure`] if `haystack` contains a non-ASCII byte, since
/// full Unicode lowercasing is not a byte-wise operation there (`'İ'`
/// lowercases to two code points) and the results could legitimately differ.
#[inline]
pub fn find_ascii_ci_simd(haystack: &[u8], needle_lower: &[u8]) -> Verdict {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: guarded by the runtime AVX2 check immediately above.
            return unsafe { find_ascii_ci_avx2(haystack, needle_lower) };
        }
    }
    find_ascii_ci_scalar(haystack, needle_lower)
}

/// Scalar equivalent of [`find_ascii_ci_simd`].
#[inline]
pub fn find_ascii_ci_scalar(haystack: &[u8], needle_lower: &[u8]) -> Verdict {
    if !haystack.is_ascii() {
        return Verdict::Unsure;
    }
    if needle_lower.is_empty() {
        return Verdict::Match(0);
    }
    if haystack.len() < needle_lower.len() {
        return Verdict::NoMatch;
    }
    let n = needle_lower.len();
    for i in 0..=haystack.len() - n {
        if haystack[i..i + n]
            .iter()
            .zip(needle_lower)
            .all(|(h, x)| h.to_ascii_lowercase() == *x)
        {
            return Verdict::Match(i);
        }
    }
    Verdict::NoMatch
}

/// # Safety
///
/// The caller must have verified AVX2 support at runtime.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn find_ascii_ci_avx2(haystack: &[u8], needle_lower: &[u8]) -> Verdict {
    // The ASCII screen has to happen anyway to stay exact, and it vectorises
    // to the same `movemask` trick used for the timestamp core.
    let mut i = 0;
    while i + 32 <= haystack.len() {
        // SAFETY: the loop condition guarantees 32 readable bytes at `i`.
        let x = unsafe { _mm256_loadu_si256(haystack.as_ptr().add(i) as *const __m256i) };
        if _mm256_movemask_epi8(x) != 0 {
            return Verdict::Unsure;
        }
        i += 32;
    }
    if !haystack[i..].is_ascii() {
        return Verdict::Unsure;
    }

    let n = needle_lower.len();
    if n == 0 {
        return Verdict::Match(0);
    }
    if haystack.len() < n {
        return Verdict::NoMatch;
    }

    // Same first/last-byte candidate filter as `find_literal_avx2`, but each
    // comparison is done against both cases of the needle byte. `| 0x20` would
    // be cheaper, yet it also folds bytes like '@'/'`' together, which would
    // report matches the `to_lowercase()` version never would.
    let (f_lo, l_lo) = (needle_lower[0], needle_lower[n - 1]);
    let (f_up, l_up) = (f_lo.to_ascii_uppercase(), l_lo.to_ascii_uppercase());
    let end = haystack.len() - n;

    let mut i = 0;
    while i + 32 <= end + 1 {
        // SAFETY: `i + 32 <= end + 1` with `end = len - n`, so both loads stay
        // in bounds.
        let bf = unsafe { _mm256_loadu_si256(haystack.as_ptr().add(i) as *const __m256i) };
        let bl = unsafe { _mm256_loadu_si256(haystack.as_ptr().add(i + n - 1) as *const __m256i) };
        let eq_f = _mm256_or_si256(
            _mm256_cmpeq_epi8(bf, _mm256_set1_epi8(f_lo as i8)),
            _mm256_cmpeq_epi8(bf, _mm256_set1_epi8(f_up as i8)),
        );
        let eq_l = _mm256_or_si256(
            _mm256_cmpeq_epi8(bl, _mm256_set1_epi8(l_lo as i8)),
            _mm256_cmpeq_epi8(bl, _mm256_set1_epi8(l_up as i8)),
        );
        let mut mask = _mm256_movemask_epi8(_mm256_and_si256(eq_f, eq_l)) as u32;
        while mask != 0 {
            let off = i + mask.trailing_zeros() as usize;
            if off <= end
                && haystack[off..off + n]
                    .iter()
                    .zip(needle_lower)
                    .all(|(h, x)| h.to_ascii_lowercase() == *x)
            {
                return Verdict::Match(off);
            }
            mask &= mask - 1;
        }
        i += 32;
    }
    match find_ascii_ci_scalar(&haystack[i..], needle_lower) {
        Verdict::Match(off) => Verdict::Match(i + off),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Tests: differential against the implementations these shadow.
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

    #[test]
    fn ascii_ci_search_matches_to_lowercase_contains() {
        let lines = [
            "Index Scan Backward using \"IX_a\" on \"public\".\"tbl\" t  (cost=0.4..9.1 rows=1 width=8)",
            "  ->  Bitmap Heap Scan on \"public\".\"t_c4\" c4  (cost=12.15..870.04 rows=128 width=24)",
            "Nested Loop Left Join  (cost=1.15..279.82 rows=7 width=110)",
            "HashAggregate  (cost=1.00..2.00 rows=1 width=8)",
            "Seq Scan on users  (cost=0.00..1.00 rows=1 width=4)",
            "Gather Merge  (cost=0.00..1.00 rows=1 width=4)",
            "",
            "short",
        ];
        let needles = [
            "bitmap index scan",
            "seq scan",
            "nested loop",
            "hash join",
            "aggregate",
            "gather merge",
            "index",
            "scan",
        ];
        for line in lines {
            let lowered = line.to_lowercase();
            for needle in needles {
                let want = lowered.find(needle);
                for (name, got) in [
                    (
                        "simd",
                        find_ascii_ci_simd(line.as_bytes(), needle.as_bytes()),
                    ),
                    (
                        "scalar",
                        find_ascii_ci_scalar(line.as_bytes(), needle.as_bytes()),
                    ),
                ] {
                    match got {
                        Verdict::Unsure => {}
                        Verdict::Match(i) => {
                            assert_eq!(Some(i), want, "{name}: {needle:?} in {line:?}")
                        }
                        Verdict::NoMatch => {
                            assert_eq!(None, want, "{name}: {needle:?} in {line:?}")
                        }
                    }
                }
            }
        }
    }

    /// Non-ASCII identifiers are legal in PostgreSQL, and full Unicode
    /// lowercasing is not byte-wise, so the scanner must decline rather than
    /// risk a different answer.
    #[test]
    fn ascii_ci_search_defers_on_non_ascii() {
        let line = "Seq Scan on \"öffentlich\".\"tabelle\"  (cost=0.00..1.00 rows=1 width=4)";
        assert_eq!(
            find_ascii_ci_simd(line.as_bytes(), b"seq scan"),
            Verdict::Unsure
        );
        assert_eq!(
            find_ascii_ci_scalar(line.as_bytes(), b"seq scan"),
            Verdict::Unsure
        );
        // Long enough to exercise the vectorised ASCII screen, not just the
        // scalar tail.
        let long = format!("{}İ{}", "Seq Scan on t ".repeat(4), "x".repeat(64));
        assert_eq!(
            find_ascii_ci_simd(long.as_bytes(), b"seq scan"),
            Verdict::Unsure
        );
    }
}

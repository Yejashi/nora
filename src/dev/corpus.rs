//! Deterministic terminal-output corpus generator (N26a).
//!
//! Every generator is a pure function of the seed and the requested
//! length `n`: the same seed and length always produce the same bytes
//! (`unique_glyphs` is a pure function of `n` alone), so benchmark
//! trials, regression tests and cross-machine comparisons can verify
//! exact bytes and final-state checksums. No external RNG dependency:
//! the generators drive a small inline SplitMix64.

/// SplitMix64: a small, dependency-free deterministic PRNG.
#[derive(Clone, Copy)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next pseudo-random 64-bit value.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Columns per generated text line (the documented nominal width).
pub const LINE_LEN: usize = 80;

/// Fixed Unicode pool for [`Corpus::unicode_lines`], in stable order:
/// Latin-1, Greek, Cyrillic, Hiragana, Hangul, CJK, Devanagari (with
/// combining marks), Ethiopic and symbols.
const UNICODE_POOL: &[char] = &[
    'é', 'ñ', 'ß', 'ö', 'ü', 'α', 'β', 'π', 'Ω', 'а', 'б', 'д', 'ж', 'あ', 'い', 'う', 'え', '한',
    '글', '世', '界', 'स', 'ं', 'क', '्', 'ሀ', 'ሁ', 'ሂ', '✓', '→', '∞', '≈',
];

/// Fixed control-sequence pool for [`Corpus::control_sequence_flood`],
/// in stable order: cursor positioning/movement, screen and line
/// clears, SGR styles, cursor visibility and alternate-screen
/// switches.
const CONTROL_SEQUENCES: &[&[u8]] = &[
    b"\x1b[1;1H",
    b"\x1b[24;80H",
    b"\x1b[A",
    b"\x1b[B",
    b"\x1b[C",
    b"\x1b[D",
    b"\x1b[2J",
    b"\x1b[K",
    b"\x1b[1m",
    b"\x1b[22m",
    b"\x1b[4m",
    b"\x1b[24m",
    b"\x1b[31m",
    b"\x1b[42m",
    b"\x1b[38;2;1;2;3m",
    b"\x1b[48;2;4;5;6m",
    b"\x1b[0m",
    b"\x1b[?25h",
    b"\x1b[?25l",
    b"\x1b[?1049h",
    b"\x1b[?1049l",
];

/// Deterministic corpus generator (N26a).
///
/// A `Corpus` carries only a seed; every method is a pure function of
/// the seed and the requested length (`unique_glyphs` ignores the
/// seed and depends only on the length).
#[derive(Clone, Copy, Debug)]
pub struct Corpus {
    seed: u64,
}

impl Corpus {
    /// Fixed default seed for reproducible corpora.
    pub const DEFAULT_SEED: u64 = 0x5EED_2026_1014;

    /// Create a corpus generator with `seed`.
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// The seed this generator was created with.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// `n` lines of printable ASCII: each line is exactly [`LINE_LEN`]
    /// columns followed by `\r\n`.
    pub fn ascii_lines(&self, n: usize) -> Vec<u8> {
        let mut rng = Rng::new(self.seed);
        let mut out = Vec::with_capacity(n * (LINE_LEN + 2));
        for _ in 0..n {
            for _ in 0..LINE_LEN {
                // 0x20..=0x7E: space through tilde (printable ASCII).
                out.push(0x20 + (rng.next() % 95) as u8);
            }
            out.push(b'\r');
            out.push(b'\n');
        }
        out
    }

    /// `n` lines of Unicode: each line is exactly [`LINE_LEN`]
    /// characters (drawn from [`UNICODE_POOL`]) followed by `\r\n`.
    pub fn unicode_lines(&self, n: usize) -> Vec<u8> {
        let mut rng = Rng::new(self.seed);
        let mut out = Vec::with_capacity(n * (LINE_LEN * 3 + 2));
        for _ in 0..n {
            for _ in 0..LINE_LEN {
                let ch = UNICODE_POOL[(rng.next() % UNICODE_POOL.len() as u64) as usize];
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
            out.push(b'\r');
            out.push(b'\n');
        }
        out
    }

    /// `n` control sequences, each drawn from [`CONTROL_SEQUENCES`],
    /// concatenated in draw order (no printable bytes).
    pub fn control_sequence_flood(&self, n: usize) -> Vec<u8> {
        let mut rng = Rng::new(self.seed);
        let mut out = Vec::new();
        for _ in 0..n {
            let seq = CONTROL_SEQUENCES[(rng.next() % CONTROL_SEQUENCES.len() as u64) as usize];
            out.extend_from_slice(seq);
        }
        out
    }

    /// `n` unique glyphs, one per line (UTF-8 followed by `\n`).
    ///
    /// Code points are assigned in ascending order from U+0100, skipping
    /// surrogates and non-characters, so every glyph is unique and
    /// valid. If `n` exceeds the available code points the corpus is
    /// truncated at U+10FFFF.
    pub fn unique_glyphs(&self, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cp = 0x100u32;
        let mut count = 0usize;
        while count < n && cp <= 0x10FFFF {
            if !(0xD800..=0xDFFF).contains(&cp) && !is_non_character(cp) {
                let ch = char::from_u32(cp).expect("checked valid code point");
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                out.push(b'\n');
                count += 1;
            }
            cp += 1;
        }
        out
    }
}

impl Default for Corpus {
    fn default() -> Self {
        Self::new(Self::DEFAULT_SEED)
    }
}

/// Unicode non-character (a code point that must not appear in data).
fn is_non_character(cp: u32) -> bool {
    (0xFDD0..=0xFDEF).contains(&cp) || (cp & 0xFFFF) == 0xFFFE || (cp & 0xFFFF) == 0xFFFF
}

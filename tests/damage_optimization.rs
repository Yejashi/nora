//! N27b: retained glyph cache.
//!
//! The live render pass re-shapes every cell on every frame. A cell's glyph
//! geometry is a pure function of its text, bold/italic/underline/strike
//! traits and the (zoom-scaled) font size, so an unchanged cell shapes to an
//! identical buffer. [`nora::render::glyph::GlyphCache`] retains the shaped
//! buffer and reuses it on a hit, skipping the per-cell cosmic-text shaping.
//!
//! These tests are headless (no GPU, no display): they build a hermetic
//! [`FontSystem`] from a pinned monospace face (the N02 pattern) and verify,
//! against the uncached [`shape_cell`] path, that the cache is a pure
//! presentation concern that never changes which glyphs are drawn.

use std::time::Instant;

use cosmic_text::UnderlineStyle;
use glyphon::{Buffer, FontSystem, Metrics, fontdb};
use nora::render::glyph::{GlyphCache, shape_cell};

/// Candidate monospace faces, mirroring `tests/text_render.rs`.
const MONO_CANDIDATES: [&str; 4] = [
    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSansMono-Regular.ttf",
    "/nix/store/8m9r3c9q6lx9rj9vffs5d53h0w0cs4a9-noto-fonts-2026.05.01/share/fonts/noto/NotoSansMono.ttf",
];

/// Resolve one monospace face, honoring `NORA_TEST_FONT_MONO` and falling back
/// to the pinned candidates.
fn resolve_mono_font() -> String {
    let env_font = std::env::var("NORA_TEST_FONT_MONO").unwrap_or_default();
    if !env_font.is_empty() {
        return env_font;
    }
    for c in MONO_CANDIDATES {
        if std::path::Path::new(c).is_file() {
            return c.to_string();
        }
    }
    panic!("no candidate mono font available");
}

/// A hermetic `FontSystem` holding only the resolved monospace face (N02).
fn font_system() -> FontSystem {
    FontSystem::new_with_fonts([fontdb::Source::File(resolve_mono_font().into())])
}

/// A comparable signature of a shaped buffer: the glyph id and (x, y, width)
/// of every laid-out glyph, in run order. Two buffers with the same signature
/// render identically.
fn layout_signature(buffer: &Buffer) -> Vec<(u16, f32, f32, f32)> {
    let mut sig = Vec::new();
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            sig.push((glyph.glyph_id, glyph.x, glyph.y, glyph.w));
        }
    }
    sig
}

/// The cache must reproduce the uncached path exactly: a miss shapes the same
/// buffer `shape_cell` would, and a hit reuses it byte-for-byte (N27b).
#[test]
fn cache_hit_matches_uncached_shaping() {
    let mut fs = font_system();
    let metrics = Metrics::new(20.0, 24.0);
    let size = metrics.font_size;
    let mut cache = GlyphCache::new();

    // A spread of cell shapes: plain, bold, italic, underlined, struck, a
    // combining cluster, a wide char and the empty cell.
    let cells: &[(&str, bool, bool, UnderlineStyle, bool)] = &[
        ("A", false, false, UnderlineStyle::None, false),
        ("A", true, false, UnderlineStyle::None, false),
        ("A", false, true, UnderlineStyle::None, false),
        ("A", false, false, UnderlineStyle::Double, false),
        ("A", false, false, UnderlineStyle::None, true),
        ("e\u{301}", false, false, UnderlineStyle::None, false),
        ("\u{4F60}", false, false, UnderlineStyle::None, false),
        ("", false, false, UnderlineStyle::None, false),
    ];

    for (text, bold, italic, underline, strike) in cells {
        // Uncached: shape fresh.
        let fresh = shape_cell(&mut fs, text, metrics, *bold, *italic, *underline, *strike);
        // Cached: the first call is a miss (shapes), the second a hit (reuses).
        let first = cache.shape(
            &mut fs, metrics, text, *bold, *italic, *underline, *strike, size,
        );
        let second = cache.shape(
            &mut fs, metrics, text, *bold, *italic, *underline, *strike, size,
        );
        // The miss and the hit must both match the uncached buffer.
        let fresh_sig = layout_signature(&fresh);
        let first_sig = layout_signature(&first);
        let second_sig = layout_signature(&second);
        assert_eq!(
            fresh_sig, first_sig,
            "cache miss for {text:?} must match the uncached shaping"
        );
        assert_eq!(
            first_sig, second_sig,
            "cache hit for {text:?} must reuse the retained buffer"
        );
    }
}

/// Damage coherence: the cache distinguishes cells by their full styling
/// identity, so an unchanged cell reuses one entry while a changed cell gets
/// a new one (N27b).
#[test]
fn cache_distinguishes_styled_cells() {
    let mut fs = font_system();
    let metrics = Metrics::new(20.0, 24.0);
    let size = metrics.font_size;
    let mut cache = GlyphCache::new();

    // Same text, same style: one entry (the repeat is a hit).
    cache.shape(
        &mut fs,
        metrics,
        "A",
        false,
        false,
        UnderlineStyle::None,
        false,
        size,
    );
    cache.shape(
        &mut fs,
        metrics,
        "A",
        false,
        false,
        UnderlineStyle::None,
        false,
        size,
    );
    assert_eq!(cache.len(), 1, "an unchanged cell reuses one entry");

    // Same text, different bold / underline / size: a new entry each.
    cache.shape(
        &mut fs,
        metrics,
        "A",
        true,
        false,
        UnderlineStyle::None,
        false,
        size,
    );
    assert_eq!(cache.len(), 2, "a bold cell is a distinct entry");
    cache.shape(
        &mut fs,
        metrics,
        "A",
        false,
        false,
        UnderlineStyle::Double,
        false,
        size,
    );
    assert_eq!(cache.len(), 3, "an underlined cell is a distinct entry");
    cache.shape(
        &mut fs,
        metrics,
        "A",
        false,
        false,
        UnderlineStyle::None,
        false,
        size * 2.0,
    );
    assert_eq!(cache.len(), 4, "a zoomed cell is a distinct entry");

    // Different text, same style: a new entry.
    cache.shape(
        &mut fs,
        metrics,
        "B",
        false,
        false,
        UnderlineStyle::None,
        false,
        size,
    );
    assert_eq!(cache.len(), 5, "a different cell is a distinct entry");
}

/// The warm cache must be cheaper than re-shaping every cell fresh (N27b):
/// a hit is a lookup plus a small buffer clone, while a miss re-runs
/// cosmic-text's Unicode segmentation, font lookup and layout.
#[test]
fn cache_hits_are_cheaper_than_fresh_shaping() {
    let mut fs = font_system();
    let metrics = Metrics::new(20.0, 24.0);
    let size = metrics.font_size;

    // 2000 distinct combining-mark clusters: expensive to shape, distinct
    // enough to exercise the cache.
    let cells: Vec<String> = (0..2000).map(|i| format!("c{i}\u{301}")).collect();

    // Warm the cache: shape every cell once (all misses).
    let mut cache = GlyphCache::new();
    for text in &cells {
        cache.shape(
            &mut fs,
            metrics,
            text,
            false,
            false,
            UnderlineStyle::None,
            false,
            size,
        );
    }

    // After: shape the same cells again (all hits).
    let t_cached_start = Instant::now();
    for text in &cells {
        cache.shape(
            &mut fs,
            metrics,
            text,
            false,
            false,
            UnderlineStyle::None,
            false,
            size,
        );
    }
    let t_cached = t_cached_start.elapsed();

    // Before: shape the same cells fresh (no cache).
    let t_fresh_start = Instant::now();
    for text in &cells {
        shape_cell(
            &mut fs,
            text,
            metrics,
            false,
            false,
            UnderlineStyle::None,
            false,
        );
    }
    let t_fresh = t_fresh_start.elapsed();

    eprintln!("N27b glyph cache: fresh={t_fresh:?}, cached={t_cached:?}");
    assert!(
        t_cached < t_fresh,
        "warm cache ({t_cached:?}) should beat fresh shaping ({t_fresh:?})"
    );
}

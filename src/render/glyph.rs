//! Per-cell glyph shaping and a retained glyph cache (N27b).
//!
//! [`shape_cell`] shapes one terminal cell exactly the way the live render
//! pass did before N27b: one cosmic-text [`Buffer`], one layout run, the
//! generic monospace family, with the cell's bold/italic/underline/strike
//! traits set on the shaping attributes.
//!
//! [`GlyphCache`] retains already-shaped buffers keyed by the cell's full
//! shaping identity (text, bold, italic, underline, strikethrough, and the
//! zoom-scaled font size). A live frame re-shapes every cell on every frame,
//! but an unchanged cell shapes to an identical buffer, so the cache reuses
//! the retained [`Buffer`] on a hit and skips the per-cell shaping. This is a
//! pure presentation concern: it holds owned glyph state only, no engine or
//! GPU lifetimes, and it never changes which glyphs are drawn — a cache hit
//! reproduces byte-for-byte the buffer the uncached path would shape.
//!
//! The cache is bounded: when the number of distinct styled cells exceeds
//! [`MAX_GLYPH_CACHE_ENTRIES`], the cache clears (a one-time re-shape), keeping
//! memory bounded by the cap.

use std::collections::HashMap;

use cosmic_text::UnderlineStyle;
use glyphon::{Attrs, Buffer, Cursor, Family, FontSystem, Metrics, Shaping, Style, Weight};

/// The cap on distinct retained styled cells (N27b). Well above any realistic
/// grid (200×50 = 10 000); a busy terminal that churns more distinct styled
/// cells than this clears and re-shapes once, which is still far cheaper than
/// re-shaping every cell every frame.
pub const MAX_GLYPH_CACHE_ENTRIES: usize = 16384;

/// Shape one cell with an explicit weight, style and decorations (N13a,
/// N13c), mirroring the live render pass. The family is the generic monospace
/// family so cosmic-text resolves the same loaded monospace face the rest of
/// the app uses.
///
/// The underline and strikethrough decorations are set on the shaping
/// attributes so cosmic-text emits them in the laid-out line's `decorations`
/// spans, which the render pass reads back and draws (N13c).
pub fn shape_cell(
    fs: &mut FontSystem,
    text: &str,
    metrics: Metrics,
    bold: bool,
    italic: bool,
    underline: UnderlineStyle,
    strikethrough: bool,
) -> Buffer {
    let weight = if bold { Weight::BOLD } else { Weight::NORMAL };
    let style = if italic { Style::Italic } else { Style::Normal };
    let mut attrs = Attrs::new()
        .family(Family::Monospace)
        .weight(weight)
        .style(style);
    if underline != UnderlineStyle::None {
        attrs.text_decoration.underline = underline;
    }
    if strikethrough {
        attrs.text_decoration.strikethrough = true;
    }
    let mut buffer = Buffer::new(fs, metrics);
    buffer.set_text(text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_cursor(fs, Cursor::default(), false);
    buffer
}

/// One retained, already-shaped glyph buffer (N27b).
struct ShapedCell {
    /// The shaped buffer, ready to hand to glyphon's [`glyphon::TextArea`].
    buffer: Buffer,
}

/// The full shaping identity of a cell, the key of a retained buffer (N27b).
///
/// A retained buffer is reused only when every field is identical to the cell
/// being drawn, so a cache hit reproduces exactly the glyphs the uncached
/// path would shape.
#[derive(Clone, Eq, Hash, PartialEq)]
struct GlyphKey {
    /// The full cluster: the cell character plus its combining marks.
    text: String,
    /// Bold weight (shape with a bold face).
    bold: bool,
    /// Italic style (shape with an italic face).
    italic: bool,
    /// Underline style (N13c).
    underline: UnderlineStyle,
    /// Strikethrough (N13c).
    strikethrough: bool,
    /// Font size in logical pixels (zoom-scaled), as bits so it is `Hash`/`Eq`.
    size: u32,
}

/// A retained glyph cache (N27b): reuses an already-shaped [`Buffer`] when a
/// cell's full shaping identity matches one already shaped.
///
/// The live render pass re-shapes every cell on every frame. A cell's glyph
/// geometry is a pure function of its text, bold/italic/underline/strike
/// traits and the (zoom-scaled) font size, so an unchanged cell shapes to an
/// identical buffer. This cache retains the shaped [`Buffer`] and reuses it on
/// a hit, skipping the per-cell cosmic-text shaping (Unicode segmentation,
/// font lookup and layout). It is bounded: when the number of distinct styled
/// cells exceeds [`MAX_GLYPH_CACHE_ENTRIES`], the cache clears (a one-time
/// re-shape), keeping memory bounded by the cap.
pub struct GlyphCache {
    entries: HashMap<GlyphKey, ShapedCell>,
}

impl GlyphCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Shape `text` with the given style, reusing a retained buffer when the
    /// cell's full shaping identity was already shaped (N27b).
    ///
    /// `size` is the zoom-scaled font size in logical pixels; it is folded
    /// into the key so a zoom change re-shapes (the metrics differ) rather
    /// than reusing a buffer shaped at the old size.
    #[allow(clippy::too_many_arguments)] // fixed render wiring: fs + metrics + text + style + size
    pub fn shape(
        &mut self,
        fs: &mut FontSystem,
        metrics: Metrics,
        text: &str,
        bold: bool,
        italic: bool,
        underline: UnderlineStyle,
        strikethrough: bool,
        size: f32,
    ) -> Buffer {
        let key = GlyphKey {
            text: text.to_owned(),
            bold,
            italic,
            underline,
            strikethrough,
            size: size.to_bits(),
        };
        if let Some(entry) = self.entries.get(&key) {
            return entry.buffer.clone();
        }
        let buffer = shape_cell(fs, text, metrics, bold, italic, underline, strikethrough);
        if self.entries.len() >= MAX_GLYPH_CACHE_ENTRIES {
            self.entries.clear();
        }
        self.entries.insert(
            key,
            ShapedCell {
                buffer: buffer.clone(),
            },
        );
        buffer
    }

    /// The number of distinct styled cells currently retained (N27b).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if no styled cells are retained yet (N27b).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for GlyphCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_reuses_identical_cells() {
        let mut fs = FontSystem::new();
        let metrics = Metrics::new(20.0, 24.0);
        let mut cache = GlyphCache::new();
        let size = metrics.font_size;

        // A fresh cache is empty.
        assert!(cache.is_empty());
        // First shaping of a cell is a miss (1 entry).
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
        assert_eq!(cache.len(), 1);
        // Second shaping of the same cell is a hit (still 1 entry).
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
        assert_eq!(cache.len(), 1);
        // A differently styled cell is a new entry.
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
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn cache_is_bounded() {
        let mut fs = FontSystem::new();
        let metrics = Metrics::new(20.0, 24.0);
        let mut cache = GlyphCache::new();
        let size = metrics.font_size;

        // Fill past the cap: the cache clears and re-shapes once, so `len`
        // never exceeds the cap.
        for i in 0..(MAX_GLYPH_CACHE_ENTRIES + 8) {
            let text = format!("cell {i}");
            cache.shape(
                &mut fs,
                metrics,
                &text,
                false,
                false,
                UnderlineStyle::None,
                false,
                size,
            );
        }
        assert!(cache.len() <= MAX_GLYPH_CACHE_ENTRIES);
        assert!(!cache.is_empty());
    }
}

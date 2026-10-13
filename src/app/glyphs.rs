//! Font loading and per-cell glyph shaping for windowed nora (N08b2b).
//!
//! Ports the N02 pattern from `tests/text_render.rs` identically: pinned
//! candidate lists (env-overridable for CI), a `FontSystem` built only from the
//! resolved files (no system font scan), per-cell cosmic `Buffer` shaping with
//! cell-allocated `bounds`, and mono cell metrics derived the same way the test
//! derives them. ASCII is the v0.1 scope (N08 card).

use std::path::PathBuf;

use glyphon::{Attrs, Buffer, Cursor, Family, FontSystem, Metrics, Shaping, fontdb};

/// N02/N08 base font size, in logical pixels (matches `tests/text_render.rs`).
pub const FONT_SIZE: f32 = 20.0;

/// Fixed cell height, in logical pixels (matches the N02 test).
pub const CELL_H: f32 = 24.0;

/// Hardcoded mono family name, exactly as the N02 test hardcodes it.
const MONO_FAMILY: &str = "Noto Sans Mono";

/// Pinned mono candidate paths, in resolution order (N02 test list, verbatim).
const MONO_CANDIDATES: [&str; 2] = [
    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    "/nix/store/8m9r3c9q6lx9rj9vffs5d53h0w0cs4a9-noto-fonts-2026.05.01/share/fonts/noto/NotoSansMono.ttf",
];

/// Pinned CJK candidate paths, in resolution order (N02 test list, verbatim).
const CJK_CANDIDATES: [&str; 2] = [
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/nix/store/i0nxswc8ljllp6lnha7vpmr3w2l33khj-noto-fonts-cjk-sans-2.004/share/fonts/opentype/noto-cjk/NotoSansCJK-VF.otf.ttc",
];

/// Pinned Ethiopic candidate paths, in resolution order (N02 test list, verbatim).
const ETHIOPIC_CANDIDATES: [&str; 2] = [
    "/usr/share/fonts/noto/NotoSerifEthiopic-Regular.ttf",
    "/nix/store/8m9r3c9q6lx9rj9vffs5d53h0w0cs4a9-noto-fonts-2026.05.01/share/fonts/noto/NotoSerifEthiopic.ttf",
];

/// Resolve a pinned font at runtime: env override first (returned as-is when
/// non-empty, mirroring the test), then the first existing candidate. Panic
/// if nothing resolves (N02 test rule).
fn resolve_font(env_var: &str, candidates: &[&str]) -> PathBuf {
    if let Ok(p) = std::env::var(env_var)
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    for c in candidates {
        if std::path::Path::new(c).is_file() {
            return PathBuf::from(c);
        }
    }
    panic!("no candidate font available for {env_var}: {candidates:?}");
}

/// The one-cluster advance, in logical pixels (N02 test rule).
fn cluster_advance(buffer: &Buffer) -> f32 {
    buffer
        .layout_runs()
        .next()
        .expect("shaped buffer must have one line")
        .line_w
}

/// Shape one cell exactly as N02 does: one buffer, one layout run.
fn shape_cell(fs: &mut FontSystem, text: &str, family: Family, metrics: Metrics) -> Buffer {
    let mut buffer = Buffer::new(fs, metrics);
    buffer.set_text(text, &Attrs::new().family(family), Shaping::Advanced, None);
    buffer.shape_until_cursor(fs, Cursor::default(), false);
    buffer
}

/// Loaded fonts and the mono cell metrics they imply.
pub struct Fonts {
    pub fs: FontSystem,
    metrics: Metrics,
    /// Mono advance (cell width), in logical pixels; a whole pixel by
    /// construction (derived from the mono face, as in the N02 test).
    pub cell_w: u32,
    /// The resolved mono font file (env override or candidate-list hit).
    pub mono_path: PathBuf,
}

impl Fonts {
    /// Load the pinned fonts and derive the mono cell metrics (N02 pattern).
    pub fn load() -> Self {
        let mono_path = resolve_font("NORA_TEST_FONT_MONO", &MONO_CANDIDATES);
        let cjk_path = resolve_font("NORA_TEST_FONT_CJK", &CJK_CANDIDATES);
        let ethiopic_path = resolve_font("NORA_TEST_FONT_ETHIOPIC", &ETHIOPIC_CANDIDATES);

        let mut fs = FontSystem::new_with_fonts([
            fontdb::Source::File(mono_path.clone()),
            fontdb::Source::File(cjk_path),
            fontdb::Source::File(ethiopic_path),
        ]);

        let mono = Family::Name(MONO_FAMILY);
        let metrics = Metrics::new(FONT_SIZE, CELL_H);
        let advance = cluster_advance(&shape_cell(&mut fs, "M", mono, metrics));
        assert!(
            (advance - advance.round()).abs() <= 0.01,
            "mono advance {advance} px must be a whole pixel"
        );
        let cell_w = advance.round() as u32;
        assert!(
            (8..24).contains(&cell_w),
            "cell width {cell_w} px is implausible"
        );

        Self {
            fs,
            metrics,
            cell_w,
            mono_path,
        }
    }

    /// Cell height in logical pixels (fixed by the N02 metrics).
    pub const fn cell_h(&self) -> u32 {
        CELL_H as u32
    }

    /// Shape one visible cell into a fresh cosmic `Buffer` under the mono
    /// family (ASCII v0.1 scope). The returned buffer is the per-cell content
    /// that `Graphics::draw` places with cell-allocated `bounds`.
    pub fn shape_cell(&mut self, text: &str) -> Buffer {
        let mono = Family::Name(MONO_FAMILY);
        shape_cell(&mut self.fs, text, mono, self.metrics)
    }
}

/// The whole cell grid that fits in a physical window size, from the mono cell
/// metrics. Pure: no GPU, no display.
// The N08b2b-2 window loop drives it; b-1 draws an empty grid, so it is
// unused here.
#[allow(dead_code)]
pub fn grid_size(width: u32, height: u32, cell_w: u32, cell_h: u32) -> (u16, u16) {
    let cols = width.checked_div(cell_w).unwrap_or(0);
    let rows = height.checked_div(cell_h).unwrap_or(0);
    (
        cols.try_into().unwrap_or(u16::MAX),
        rows.try_into().unwrap_or(u16::MAX),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_size_fills_the_window() {
        assert_eq!(grid_size(960, 600, 12, 24), (80, 25));
    }

    #[test]
    fn grid_size_drops_partial_cells() {
        assert_eq!(grid_size(100, 100, 12, 24), (8, 4));
    }

    #[test]
    fn grid_size_zero_is_zero() {
        assert_eq!(grid_size(0, 0, 12, 24), (0, 0));
        assert_eq!(grid_size(11, 23, 12, 24), (0, 0));
    }

    #[test]
    fn grid_size_zero_cell_metrics_never_divide_by_zero() {
        assert_eq!(grid_size(960, 600, 0, 0), (0, 0));
    }

    #[test]
    fn font_constants_match_the_n02_test() {
        assert_eq!(FONT_SIZE, 20.0);
        assert_eq!(CELL_H, 24.0);
    }
}

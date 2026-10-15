//! Font loading and per-cell glyph shaping for windowed nora (N08b2b).
//!
//! Ports the N02 pattern from `tests/text_render.rs` identically: pinned
//! candidate lists (env-overridable for CI), a `FontSystem` built only from the
//! resolved files (no system font scan), per-cell cosmic `Buffer` shaping with
//! cell-allocated `bounds`, and mono cell metrics derived the same way the test
//! derives them. ASCII is the v0.1 scope (N08 card).

use std::path::{Path, PathBuf};

use glyphon::{Attrs, Buffer, Cursor, Family, FontSystem, Metrics, Shaping, fontdb};

/// N02/N08 base font size, in logical pixels (matches `tests/text_render.rs`).
pub const FONT_SIZE: f32 = 20.0;

/// Fixed cell height, in logical pixels (matches the N02 test).
pub const CELL_H: f32 = 24.0;

/// Preferred mono family (the N02 test font); the system fallback may load another.
const MONO_FAMILY: &str = "Noto Sans Mono";

/// Pinned mono candidate paths, in resolution order (N02 test list, verbatim).
const MONO_CANDIDATES: [&str; 4] = [
    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSansMono-Regular.ttf",
    "/nix/store/8m9r3c9q6lx9rj9vffs5d53h0w0cs4a9-noto-fonts-2026.05.01/share/fonts/noto/NotoSansMono.ttf",
];

/// Pinned CJK candidate paths, in resolution order (N02 test list, verbatim).
const CJK_CANDIDATES: [&str; 4] = [
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/nix/store/i0nxswc8ljllp6lnha7vpmr3w2l33khj-noto-fonts-cjk-sans-2.004/share/fonts/opentype/noto-cjk/NotoSansCJK-VF.otf.ttc",
];

/// Pinned Ethiopic candidate paths, in resolution order (N02 test list, verbatim).
const ETHIOPIC_CANDIDATES: [&str; 4] = [
    "/usr/share/fonts/noto/NotoSerifEthiopic-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSerifEthiopic-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSerifEthiopic-Regular.ttf",
    "/nix/store/8m9r3c9q6lx9rj9vffs5d53h0w0cs4a9-noto-fonts-2026.05.01/share/fonts/noto/NotoSerifEthiopic.ttf",
];

/// Resolve a font at runtime: env override first (returned as-is when
/// non-empty, mirroring the tests), then the first existing pinned candidate,
/// then the system font database (fontconfig on Linux) queried for `families`
/// in order. `None` when nothing resolves.
fn resolve_font(env_var: &str, candidates: &[&str], families: &[&str]) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env_var)
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    for c in candidates {
        if Path::new(c).is_file() {
            return Some(PathBuf::from(c));
        }
    }
    system_font(families)
}

/// Query the system font database for the first of `families` that resolves
/// to a font file. `"monospace"` is the fontconfig generic alias; any other
/// entry is an exact family name.
fn system_font(families: &[&str]) -> Option<PathBuf> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    for family in families {
        let family = match *family {
            "monospace" => fontdb::Family::Monospace,
            name => fontdb::Family::Name(name),
        };
        let query = fontdb::Query {
            families: &[family],
            ..fontdb::Query::default()
        };
        if let Some(id) = db.query(&query)
            && let Some((fontdb::Source::File(path), _)) = db.face_source(id)
        {
            return Some(path);
        }
    }
    None
}

/// The primary family name of the face loaded from `path`, or `fallback`.
fn family_of(fs: &FontSystem, path: &Path, fallback: &str) -> String {
    fs.db()
        .faces()
        .find(|face| matches!(&face.source, fontdb::Source::File(p) if p == path))
        .and_then(|face| face.families.first().map(|(name, _)| name.clone()))
        .unwrap_or_else(|| fallback.to_string())
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
    /// Mono advance (cell width), in logical pixels; a whole pixel by
    /// construction (derived from the mono face, as in the N02 test).
    pub cell_w: u32,
    /// The resolved mono font file (env override or candidate-list hit).
    pub mono_path: PathBuf,
}

impl Fonts {
    /// Load the pinned fonts and derive the mono cell metrics (N02 pattern).
    pub fn load() -> Self {
        let mono_path = resolve_font(
            "NORA_TEST_FONT_MONO",
            &MONO_CANDIDATES,
            &[MONO_FAMILY, "DejaVu Sans Mono", "monospace"],
        )
        .unwrap_or_else(|| {
            panic!(
                "no monospace font found: install Noto Sans Mono (or any fontconfig \
                 monospace font) or set NORA_TEST_FONT_MONO to a font file"
            )
        });
        // Script fallbacks are optional: without them wide/Ethiopic text
        // renders as missing glyphs instead of aborting startup.
        let cjk_path = resolve_font(
            "NORA_TEST_FONT_CJK",
            &CJK_CANDIDATES,
            &["Noto Sans CJK SC", "Noto Sans CJK JP", "Noto Sans CJK"],
        );
        let ethiopic_path = resolve_font(
            "NORA_TEST_FONT_ETHIOPIC",
            &ETHIOPIC_CANDIDATES,
            &["Noto Serif Ethiopic", "Noto Sans Ethiopic"],
        );

        let mut fs = FontSystem::new_with_fonts(
            [Some(mono_path.clone()), cjk_path, ethiopic_path]
                .into_iter()
                .flatten()
                .map(fontdb::Source::File),
        );

        // Shape against the family that actually loaded, and make it the
        // `Family::Monospace` target for the cell shaper.
        let mono_family = family_of(&fs, &mono_path, MONO_FAMILY);
        fs.db_mut().set_monospace_family(mono_family.clone());
        let mono = Family::Name(&mono_family);
        let metrics = Metrics::new(FONT_SIZE, CELL_H);
        let advance = cluster_advance(&shape_cell(&mut fs, "M", mono, metrics));
        // Noto Sans Mono yields a whole-pixel advance at this size; other
        // monospace faces may not, so the cell snaps to the nearest pixel.
        let cell_w = advance.round() as u32;
        assert!(
            (8..24).contains(&cell_w),
            "cell width {cell_w} px is implausible"
        );

        Self {
            fs,
            cell_w,
            mono_path,
        }
    }

    /// Cell height in logical pixels (fixed by the N02 metrics).
    pub const fn cell_h(&self) -> u32 {
        CELL_H as u32
    }
}

/// The shaping font size at a zoom level (N19b): the base [`FONT_SIZE`]
/// scaled by the zoom factor.
pub fn zoomed_font_size(zoom: f32) -> f32 {
    FONT_SIZE * zoom
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

    #[test]
    fn zoomed_font_size_scales_the_base_size() {
        assert_eq!(zoomed_font_size(1.0), FONT_SIZE);
        assert_eq!(zoomed_font_size(2.0), FONT_SIZE * 2.0);
        assert_eq!(zoomed_font_size(0.5), FONT_SIZE * 0.5);
    }
}

#[cfg(test)]
mod font_resolution_tests {
    use super::*;

    #[test]
    fn unknown_family_does_not_resolve() {
        assert_eq!(system_font(&["nora-no-such-family-7f3a"]), None);
    }

    /// The generic alias resolves to an existing file wherever fontconfig
    /// knows a monospace font; a font-less host yields `None`, not a panic.
    #[test]
    fn monospace_alias_resolves_to_a_file_when_available() {
        if let Some(path) = system_font(&["monospace"]) {
            eprintln!("system monospace: {}", path.display());
            assert!(path.is_file(), "{} is not a file", path.display());
        }
    }
}

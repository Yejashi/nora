//! Font loading and per-cell glyph shaping for windowed nora (N08b2b).
//!
//! Ports the N02 pattern from `tests/text_render.rs` identically: pinned
//! candidate lists (env-overridable for CI), a `FontSystem` built only from the
//! resolved files (no system font scan), per-cell cosmic `Buffer` shaping with
//! cell-allocated `bounds`, and mono cell metrics derived the same way the test
//! derives them. ASCII is the v0.1 scope (N08 card).

use std::path::{Path, PathBuf};

use glyphon::{Attrs, Buffer, Cursor, Family, FontSystem, Metrics, Shaping, fontdb};

/// Pixels per point at the 96 DPI reference (CSS and most toolkits): a
/// point is 1/72 inch.
const PX_PER_PT: f32 = 96.0 / 72.0;

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

/// The system font database (fontconfig directories), scanned once per
/// process on first use.
fn system_db() -> &'static fontdb::Database {
    static DB: std::sync::OnceLock<fontdb::Database> = std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db
    })
}

/// Query the system font database for the first of `families` that resolves
/// to a font file. `"monospace"` is the fontconfig generic alias; any other
/// entry is an exact family name.
fn system_font(families: &[&str]) -> Option<PathBuf> {
    let db = system_db();
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

/// Cell geometry in physical pixels for one font size, scale factor and
/// zoom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    /// Shaping font size (physical px).
    pub font_px: f32,
    /// Cell width: the mono advance, rounded to a whole pixel.
    pub cell_w: u32,
    /// Cell height: the face's ascent + descent + line gap, rounded up.
    pub cell_h: u32,
}

/// The configured font size in points converted to physical pixels at a
/// display scale factor and zoom level.
pub fn font_px(size_pt: f32, scale: f64, zoom: f32) -> f32 {
    size_pt * PX_PER_PT * scale as f32 * zoom
}

/// Loaded fonts and the current cell metrics.
pub struct Fonts {
    pub fs: FontSystem,
    /// The mono family the cells are shaped with.
    pub family: String,
    /// The resolved mono font file (env override, config family, pinned
    /// candidate or fontconfig match).
    pub mono_path: PathBuf,
    /// Configured size in points.
    size_pt: f32,
    /// Current metrics (physical pixels); see [`Fonts::measure`].
    pub metrics: CellMetrics,
    /// Cell width in physical pixels (mirrors `metrics.cell_w`).
    pub cell_w: u32,
}

impl Fonts {
    /// Load the fonts for `font` (family preference and size in points)
    /// and measure the cells at scale 1 and zoom 1. The configured family
    /// is tried first, unless it is the generic `monospace`; then Noto Sans
    /// Mono, DejaVu Sans Mono and fontconfig's monospace alias. Bold faces
    /// of the mono family are loaded when installed (no synthetic bold).
    pub fn load(font: &nora::config::FontConfig) -> Self {
        let mut families: Vec<&str> = Vec::new();
        let configured = font.family.trim();
        if !configured.is_empty() && configured != "monospace" {
            families.push(configured);
        }
        families.extend([MONO_FAMILY, "DejaVu Sans Mono", "monospace"]);
        // An explicitly configured family wins over the pinned candidates.
        let configured_path = if families.len() > 3 {
            system_font(&families[..1])
        } else {
            None
        };
        let mono_path = configured_path
            .or_else(|| resolve_font("NORA_TEST_FONT_MONO", &MONO_CANDIDATES, &families))
            .unwrap_or_else(|| {
                panic!(
                    "no monospace font found: install Noto Sans Mono (or any fontconfig \
                     monospace font), set font.family, or set NORA_TEST_FONT_MONO"
                )
            });
        if families.len() > 3 && system_font(&families[..1]).is_none() {
            eprintln!("nora: font.family '{configured}' not found; using a fallback");
        }
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
        let family = family_of(&fs, &mono_path, MONO_FAMILY);
        for path in styled_faces(&family) {
            if path != mono_path {
                fs.db_mut().load_font_source(fontdb::Source::File(path));
            }
        }
        fs.db_mut().set_monospace_family(family.clone());
        let mut fonts = Self {
            fs,
            family,
            mono_path,
            size_pt: font.size,
            metrics: CellMetrics {
                font_px: 0.0,
                cell_w: 1,
                cell_h: 1,
            },
            cell_w: 1,
        };
        fonts.measure(1.0, 1.0);
        fonts
    }

    /// Recompute the cell metrics for a display scale factor and zoom;
    /// returns them. Pure font work: no GPU, no display.
    pub fn measure(&mut self, scale: f64, zoom: f32) -> CellMetrics {
        let px = font_px(self.size_pt, scale, zoom).max(1.0);
        let family = self.family.clone();
        let advance = cluster_advance(&shape_cell(
            &mut self.fs,
            "M",
            Family::Name(&family),
            Metrics::new(px, px),
        ));
        let line = self.line_height(px).unwrap_or(px * 1.2);
        let metrics = CellMetrics {
            font_px: px,
            cell_w: (advance.round() as u32).max(1),
            cell_h: (line.ceil() as u32).max(1),
        };
        self.metrics = metrics;
        self.cell_w = metrics.cell_w;
        metrics
    }

    /// The mono face's line height at `px`: (ascent + descent + leading)
    /// scaled from font units.
    fn line_height(&mut self, px: f32) -> Option<f32> {
        let id = self.fs.db().faces().find_map(|face| {
            matches!(&face.source, fontdb::Source::File(p) if *p == self.mono_path)
                .then_some(face.id)
        })?;
        let font = self.fs.get_font(id, fontdb::Weight::NORMAL)?;
        let m = font.as_swash().metrics(&[]);
        if m.units_per_em == 0 {
            return None;
        }
        Some((m.ascent + m.descent + m.leading) * px / f32::from(m.units_per_em))
    }

    /// Cell height in physical pixels.
    pub fn cell_h(&self) -> u32 {
        self.metrics.cell_h
    }
}

/// Font files for the bold, italic and bold-italic faces of `family`, as
/// fontconfig knows them (regular excluded).
fn styled_faces(family: &str) -> Vec<PathBuf> {
    let db = system_db();
    let mut out = Vec::new();
    for (weight, style) in [
        (fontdb::Weight::BOLD, fontdb::Style::Normal),
        (fontdb::Weight::NORMAL, fontdb::Style::Italic),
        (fontdb::Weight::BOLD, fontdb::Style::Italic),
    ] {
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight,
            style,
            ..fontdb::Query::default()
        };
        if let Some(id) = db.query(&query)
            && let Some(face) = db.face(id)
            && face.weight == weight
            && face.style == style
            && let fontdb::Source::File(path) = &face.source
        {
            out.push(path.clone());
        }
    }
    out
}

/// The whole cell grid that fits in a physical window size, from the mono cell
/// metrics. Pure: no GPU, no display.
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
    fn font_px_converts_points_at_96_dpi() {
        assert_eq!(font_px(15.0, 1.0, 1.0), 20.0);
        assert_eq!(font_px(12.0, 2.0, 1.0), 32.0);
        assert_eq!(font_px(15.0, 1.0, 2.0), 40.0);
    }
}

#[cfg(test)]
mod font_resolution_tests {
    use super::*;

    /// Cell metrics are physical pixels: they scale with the display scale
    /// factor and zoom alike, the line box covers the em, and the derived
    /// grid always fits the surface (audit R3/R4/R5).
    #[test]
    fn metrics_scale_with_display_and_zoom() {
        let mut fonts = Fonts::load(&nora::config::FontConfig::default());
        let m1 = fonts.measure(1.0, 1.0);
        assert_eq!(m1.font_px, 20.0, "15 pt default = 20 px at 96 DPI");
        assert!(m1.cell_h as f32 >= m1.font_px, "line box covers the em");
        let m2 = fonts.measure(2.0, 1.0);
        assert!((m2.font_px - 2.0 * m1.font_px).abs() < 0.01);
        assert!((i64::from(m2.cell_w) - 2 * i64::from(m1.cell_w)).abs() <= 1);
        assert!((i64::from(m2.cell_h) - 2 * i64::from(m1.cell_h)).abs() <= 1);
        assert_eq!(fonts.measure(1.0, 2.0), m2, "zoom 2 at scale 1 == scale 2");
        let m125 = fonts.measure(1.25, 1.0);
        for m in [m1, m125, m2] {
            let (cols, rows) = grid_size(1000, 700, m.cell_w, m.cell_h);
            assert!(u32::from(cols) * m.cell_w <= 1000);
            assert!(u32::from(rows) * m.cell_h <= 700);
            assert!(cols > 0 && rows > 0);
        }
    }

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

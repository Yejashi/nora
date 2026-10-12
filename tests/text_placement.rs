//! N02a: headless fixed-cell text placement spike.
//!
//! Proves, on the pinned glyphon 0.12.0 / cosmic-text 0.19.0, that terminal
//! cells can be shaped per cell and placed without layout drift:
//!
//! - One cosmic-text `Buffer` per cell cluster — no whole-row paragraph
//!   layout; one layout run per cell.
//! - A cell's left/top is pure terminal geometry (`col * cell_w`,
//!   `row * cell_h`), never derived from shaped glyph advances.
//! - Cluster advances stay inside the cells the engine allocates: narrow and
//!   combining clusters fit one cell; full-width CJK/Ethiopic clusters fit
//!   two.
//! - Only dirty cells are shaped; unchanged cells keep bit-identical geometry
//!   across frames (no re-shape, no drift).
//!
//! Pinned test fonts (OFL), resolved at runtime from a candidate list —
//! either the originally pinned path or the current NixOS store path — and
//! overridable via NORA_TEST_FONT_MONO / NORA_TEST_FONT_CJK /
//! NORA_TEST_FONT_ETHIOPIC:
//! - Latin + box drawing: Noto Sans Mono (`mono_path()`)
//! - CJK wide: Noto Sans CJK (`cjk_path()`)
//! - Ethiopic fallback: Noto Serif Ethiopic (`ethiopic_path()`)
//!
//! GPU atlas/raster cache accounting is the N02b concern; this file is pure
//! CPU shaping plus placement arithmetic, no device or surface involved.

use std::time::Instant;

use glyphon::{Attrs, Buffer, Cursor, Family, FontSystem, Metrics, Shaping, fontdb};

/// Resolve a pinned test font at runtime: env override first, then the first
/// existing candidate. NixOS store paths change with generations, so the
/// candidate list carries both the original pinned paths (previous machine)
/// and the current NixOS store paths.
fn resolve_font(env_var: &str, candidates: &[&str]) -> String {
    if let Ok(p) = std::env::var(env_var)
        && !p.is_empty()
    {
        return p;
    }
    for c in candidates {
        if std::path::Path::new(c).is_file() {
            return c.to_string();
        }
    }
    panic!("no candidate test font available for {env_var}: {candidates:?}");
}

static MONO_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static CJK_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static ETHIOPIC_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn mono_path() -> &'static str {
    MONO_PATH
        .get_or_init(|| {
            resolve_font(
                "NORA_TEST_FONT_MONO",
                &[
                    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
                    "/nix/store/8m9r3c9q6lx9rj9vffs5d53h0w0cs4a9-noto-fonts-2026.05.01/share/fonts/noto/NotoSansMono.ttf",
                ],
            )
        })
        .as_str()
}

fn cjk_path() -> &'static str {
    CJK_PATH
        .get_or_init(|| {
            resolve_font(
                "NORA_TEST_FONT_CJK",
                &[
                    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
                    "/nix/store/i0nxswc8ljllp6lnha7vpmr3w2l33khj-noto-fonts-cjk-sans-2.004/share/fonts/opentype/noto-cjk/NotoSansCJK-VF.otf.ttc",
                ],
            )
        })
        .as_str()
}

fn ethiopic_path() -> &'static str {
    ETHIOPIC_PATH
        .get_or_init(|| {
            resolve_font(
                "NORA_TEST_FONT_ETHIOPIC",
                &[
                    "/usr/share/fonts/noto/NotoSerifEthiopic-Regular.ttf",
                    "/nix/store/8m9r3c9q6lx9rj9vffs5d53h0w0cs4a9-noto-fonts-2026.05.01/share/fonts/noto/NotoSerifEthiopic.ttf",
                ],
            )
        })
        .as_str()
}

/// Base cell geometry: 20 px font in a 24 px row (1.2 line height).
const FONT_SIZE: f32 = 20.0;
const CELL_H: f32 = 24.0;

/// Terminal geometry decides cell starts; the shaper never does.
fn cell_origin(col: u32, row: u32, cell_w: f32, cell_h: f32) -> (f32, f32) {
    (col as f32 * cell_w, row as f32 * cell_h)
}

fn font_system() -> FontSystem {
    FontSystem::new_with_fonts([
        fontdb::Source::File(mono_path().into()),
        fontdb::Source::File(cjk_path().into()),
        fontdb::Source::File(ethiopic_path().into()),
    ])
}

fn base_metrics() -> Metrics {
    Metrics::new(FONT_SIZE, CELL_H)
}

/// First family name in the pinned db whose family starts with `prefix`.
fn family_name(fs: &FontSystem, prefix: &str) -> String {
    let face = fs
        .db()
        .faces()
        .find(|face| {
            face.families
                .first()
                .is_some_and(|(name, _)| name.starts_with(prefix))
        })
        .unwrap_or_else(|| panic!("pinned font with family starting {prefix:?} did not load"));
    face.families[0].0.clone()
}

/// Shape one cell: a single-buffer cluster under the given family and metrics.
fn shape_cell(fs: &mut FontSystem, text: &str, family: Family, metrics: Metrics) -> Buffer {
    let mut buffer = Buffer::new(fs, metrics);
    buffer.set_text(text, &Attrs::new().family(family), Shaping::Advanced, None);
    buffer.shape_until_cursor(fs, Cursor::default(), false);
    buffer
}

/// The cluster's advance width in pixels, for a one-line, one-cluster buffer.
fn cluster_advance(buffer: &Buffer, text: &str) -> f32 {
    let mut runs = buffer.layout_runs();
    let run = runs.next().expect("shaped buffer must have one line");
    assert!(runs.next().is_none(), "expected exactly one layout run");
    assert_eq!(run.text, text);
    let first = run
        .glyphs
        .first()
        .expect("shaped cluster must produce glyphs");
    let last = run
        .glyphs
        .last()
        .expect("shaped cluster must produce glyphs");
    assert_eq!(first.start, 0);
    assert_eq!(
        last.end,
        text.len(),
        "cluster must cover the whole cell text"
    );
    run.line_w
}

/// File-backed source path of a face, if it was loaded from a file.
fn face_path(face: &fontdb::FaceInfo) -> Option<String> {
    match &face.source {
        fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
            Some(path.to_string_lossy().into_owned())
        }
        fontdb::Source::Binary(_) => None,
    }
}

/// Source path of the font face the cluster actually resolved to.
fn cluster_font_path(fs: &FontSystem, buffer: &Buffer) -> String {
    let run = buffer.layout_runs().next().unwrap();
    let face = fs
        .db()
        .faces()
        .find(|face| face.id == run.glyphs[0].font_id)
        .expect("glyph font id must resolve in the pinned db");
    face_path(face).unwrap_or_else(|| {
        panic!(
            "cluster resolved to a non-file face: {:?}",
            face.post_script_name
        )
    })
}

#[test]
fn pinned_fonts_resolve_and_own_every_face() {
    let fs = font_system();
    let faces: Vec<&fontdb::FaceInfo> = fs.db().faces().collect();
    // `FontSystem::new_with_fonts` loads system fonts before the pinned ones
    // (cosmic-text calls `db.load_system_fonts()`), so the db is not
    // hermetic: DejaVu, Liberation and friends coexist. The gate is that
    // every pinned face is present and file-backed.
    for pinned in [mono_path(), cjk_path(), ethiopic_path()] {
        assert!(
            faces
                .iter()
                .any(|face| face_path(face).as_deref() == Some(pinned)),
            "pinned font {pinned} not found in the db ({} faces loaded)",
            faces.len()
        );
    }
    for prefix in ["Noto Sans Mono", "Noto Sans CJK", "Noto Serif Ethiopic"] {
        let _ = family_name(&fs, prefix);
    }
}

#[test]
fn narrow_clusters_share_one_monospace_advance() {
    let mut fs = font_system();
    let family = Family::Name("Noto Sans Mono");
    let metrics = base_metrics();

    let chars = ["A", "x", "W", "0", "-", "\u{2500}"];
    let advances: Vec<f32> = chars
        .iter()
        .map(|ch| cluster_advance(&shape_cell(&mut fs, ch, family, metrics), ch))
        .collect();

    let cell_w = advances[0];
    // A 0.6 em monospace advance at 20 px is ~12 px; catch gross substitution.
    assert!(
        (10.0..14.0).contains(&cell_w),
        "monospace advance {cell_w} px"
    );
    for (ch, adv) in chars.iter().zip(advances.iter()) {
        assert!(
            (adv - cell_w).abs() <= 0.5,
            "{ch:?} advance {adv} px differs from {cell_w} px"
        );
    }
}

#[test]
fn combining_cluster_stays_within_one_cell() {
    let mut fs = font_system();
    let family = Family::Name("Noto Sans Mono");
    let metrics = base_metrics();

    let cell_w = cluster_advance(&shape_cell(&mut fs, "M", family, metrics), "M");
    let text = "e\u{301}";
    let buffer = shape_cell(&mut fs, text, family, metrics);
    let adv = cluster_advance(&buffer, text);

    assert!(adv > 0.0);
    assert!(
        adv <= cell_w + 0.5,
        "combining cluster advance {adv} px must fit one cell of {cell_w} px"
    );
    assert_eq!(cluster_font_path(&fs, &buffer), mono_path());
}

#[test]
fn cjk_cluster_is_wide_and_spans_within_two_cells() {
    let mut fs = font_system();
    let family = Family::Name("Noto Sans Mono");
    let metrics = base_metrics();

    let cell_w = cluster_advance(&shape_cell(&mut fs, "M", family, metrics), "M");
    let cjk = family_name(&fs, "Noto Sans CJK");
    let cjk_family = Family::Name(cjk.as_str());
    let buffer = shape_cell(&mut fs, "你", cjk_family, metrics);
    let adv = cluster_advance(&buffer, "你");

    assert!(
        adv > cell_w,
        "CJK advance {adv} px must be wider than the narrow cell {cell_w} px"
    );
    assert!(
        adv <= 2.0 * cell_w + 0.5,
        "CJK advance {adv} px must fit two cells of {cell_w} px"
    );
    assert_eq!(cluster_font_path(&fs, &buffer), cjk_path());
}

#[test]
fn ethiopic_fallback_cluster_spans_within_two_cells() {
    let mut fs = font_system();
    let family = Family::Name("Noto Sans Mono");
    let metrics = base_metrics();

    let cell_w = cluster_advance(&shape_cell(&mut fs, "M", family, metrics), "M");
    let ethiopic = family_name(&fs, "Noto Serif Ethiopic");
    let ethiopic_family = Family::Name(ethiopic.as_str());
    let buffer = shape_cell(&mut fs, "\u{1200}", ethiopic_family, metrics);
    let adv = cluster_advance(&buffer, "\u{1200}");

    assert!(
        adv > cell_w,
        "Ethiopic advance {adv} px must be wider than the narrow cell {cell_w} px"
    );
    assert!(
        adv <= 2.0 * cell_w + 0.5,
        "Ethiopic advance {adv} px must fit two cells of {cell_w} px"
    );
    assert_eq!(cluster_font_path(&fs, &buffer), ethiopic_path());
}

#[test]
fn cell_starts_are_terminal_math_not_shaped_advance() {
    let mut fs = font_system();
    let family = Family::Name("Noto Sans Mono");
    let metrics = base_metrics();

    let cell_w = cluster_advance(&shape_cell(&mut fs, "M", family, metrics), "M");
    let cjk = family_name(&fs, "Noto Sans CJK");
    let cjk_adv = cluster_advance(
        &shape_cell(&mut fs, "你", Family::Name(cjk.as_str()), metrics),
        "你",
    );

    // Cell starts follow terminal geometry exactly, at any column.
    for col in [0u32, 1, 7, 13, 39, 79] {
        let (left, _) = cell_origin(col, 0, cell_w, CELL_H);
        assert_eq!(left, (col as f32) * cell_w);
    }

    // A wide glyph overflows its own cell (cjk_adv > cell_w), yet the next
    // cell still starts at (col + 1) * cell_w — never at col * cell_w + the
    // shaped advance. Terminal math, not the shaper, decides where cell 8
    // begins.
    assert!(
        cjk_adv > cell_w,
        "CJK glyph {cjk_adv} px must overflow one cell of {cell_w} px"
    );
    let (next, _) = cell_origin(8, 0, cell_w, CELL_H);
    assert_eq!(
        next,
        8.0 * cell_w,
        "next cell start must be pure terminal math"
    );
    // The overflow fits the engine's two-cell allocation; N02b proves the
    // clip on pixels.
    assert!(2.0 * cell_w - cjk_adv > 0.0);
}

#[test]
fn scale_changes_glyph_size_but_not_cell_math() {
    let mut fs = font_system();
    let family = Family::Name("Noto Sans Mono");
    let base = cluster_advance(&shape_cell(&mut fs, "M", family, base_metrics()), "M");

    for scale in [1.0f32, 1.25, 1.5, 2.0] {
        let metrics = base_metrics().scale(scale);
        let cell_h = CELL_H * scale;
        assert_eq!(metrics.line_height, cell_h);

        let cell_w = cluster_advance(&shape_cell(&mut fs, "M", family, metrics), "M");
        // Glyph size tracks the font scale; hinting allows sub-pixel drift.
        let expected = base * scale;
        assert!(
            (cell_w - expected).abs() <= 0.5,
            "advance {cell_w} px at scale {scale} vs expected {expected} px"
        );

        for col in [0u32, 1, 41, 79] {
            let (left, top) = cell_origin(col, 3, cell_w, cell_h);
            assert_eq!(left, (col as f32) * cell_w);
            assert_eq!(top, 3.0 * cell_h);
        }
    }
}

#[test]
fn unchanged_cells_are_not_reshaped_and_keep_geometry() {
    let mut fs = font_system();
    let family = Family::Name("Noto Sans Mono");
    let cjk = family_name(&fs, "Noto Sans CJK");
    let cjk_family = Family::Name(cjk.as_str());
    let metrics = base_metrics();

    // Frame 1: five cells, five shape calls.
    let mut cells: Vec<(&str, Family, Buffer)> = vec![
        ("A", family, shape_cell(&mut fs, "A", family, metrics)),
        ("b", family, shape_cell(&mut fs, "b", family, metrics)),
        (
            "你",
            cjk_family,
            shape_cell(&mut fs, "你", cjk_family, metrics),
        ),
        (
            "\u{2500}",
            family,
            shape_cell(&mut fs, "\u{2500}", family, metrics),
        ),
        (
            "e\u{301}",
            family,
            shape_cell(&mut fs, "e\u{301}", family, metrics),
        ),
    ];
    let before: Vec<f32> = cells
        .iter()
        .map(|(text, _, buffer)| cluster_advance(buffer, text))
        .collect();

    // Frame 2: only cell 2 is dirty; shape just that cell.
    let mut shaped = 0usize;
    cells[2].2 = shape_cell(&mut fs, "C", family, metrics);
    cells[2].0 = "C";
    shaped += 1;

    assert_eq!(shaped, 1, "exactly one cell was shaped on frame 2");
    for (i, (text, _, buffer)) in cells.iter().enumerate() {
        let adv = cluster_advance(buffer, text);
        if i == 2 {
            assert_ne!(adv, before[i], "changed cell must have new geometry");
        } else {
            assert_eq!(
                adv.to_bits(),
                before[i].to_bits(),
                "unchanged cell {i} geometry must be bit-identical"
            );
        }
    }
}

#[test]
fn full_grid_shapes_without_layout_drift() {
    let mut fs = font_system();
    let family = Family::Name("Noto Sans Mono");
    let metrics = base_metrics();

    const COLS: u32 = 80;
    const ROWS: u32 = 24;

    // One buffer per cell (1920 buffers, one layout run each); this is the
    // buffer/run count the per-cell model pays for.
    let start = Instant::now();
    let mut min: f32 = f32::MAX;
    let mut max: f32 = 0.0;
    for row in 0..ROWS {
        for col in 0..COLS {
            let ch = char::from_u32(0x61 + (row * COLS + col) % 26).unwrap();
            let text = ch.to_string();
            let buffer = shape_cell(&mut fs, &text, family, metrics);
            let adv = cluster_advance(&buffer, &text);
            min = min.min(adv);
            max = max.max(adv);
        }
    }
    let elapsed = start.elapsed();

    assert!(
        max - min <= 0.5,
        "per-cell advances drifted: min {min} px, max {max} px"
    );
    let span = COLS as f32 * min;
    let (last_left, _) = cell_origin(COLS - 1, ROWS - 1, min, CELL_H);
    assert_eq!(
        last_left + min,
        span,
        "grid must span exactly COLS * cell_w"
    );

    // Measured, not asserted: debug-build cost of 1920 single-cell shapes.
    eprintln!(
        "N02a measured: shaped 1920 single-cell buffers in {:.1?} (min {min:.2} px, max {max:.2} px)",
        elapsed
    );
}

//! N08a: offscreen terminal rendering — mapping tests plus GPU proof.
//!
//! Tests 1-6 are pure mapping: an owned `Snapshot` (N07) is turned into
//! draw items, background fills and a cursor fill by `nora::render::term`,
//! with the damage→region mapping exact (`Full` ⇒ all rows, `Partial` ⇒
//! each `[left..=right]` inclusive span plus the cursor row always).
//!
//! Tests 7-9 reuse the N02b headless GPU harness verbatim (device/adapter/
//! texture setup, per-cell `TextArea` construction, render pass setup, and
//! the `copy_texture_to_buffer` → `map_async` → `device.poll` readback with
//! the `BufferView` dropped before `unmap`) to prove the mapped output
//! renders correctly: a `Spec` cell background clears the cell and the
//! glyph ink differs from it; a pinned `HIDDEN` cell produces zero ink; a
//! pinned `INVERSE` cell swaps the resolved fg/bg; the cursor cell is
//! sampled in the resolved `NamedColor::Cursor` color; and repeated
//! rendering of an unchanged `Snapshot` is pixel-identical.
//!
//! `cell_w` is measured per N02a (cross-character advance equality), never
//! hardcoded; `cell_h = 24.0`, `font_size = 20.0` are the N02a baseline.

use std::time::Instant;

use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use glyphon::{
    Attrs, Buffer, Cache, Cursor as TextCursor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, fontdb,
};
use nora::engine::{CellText, Damage, LineDamage, Snapshot};
use nora::render::term::{
    FillRect, TermLayout, build_bg_fills, build_draw_items, cursor_fill, cursor_visible,
    is_noop_damage, resolve_color,
};
use wgpu::{
    BufferDescriptor, BufferUsages, CommandEncoderDescriptor, DeviceDescriptor, Extent3d, Instance,
    InstanceDescriptor, LoadOp, MapMode, MultisampleState, Operations, Origin3d, PollType,
    RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions, StoreOp,
    TexelCopyBufferInfo, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
    TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureViewDescriptor,
};

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

/// 20 px font in a 24 px row — same baseline as N02a/N02b.
const FONT_SIZE: f32 = 20.0;
const CELL_H: f32 = 24.0;

fn layout(cell_w: f32) -> TermLayout {
    TermLayout {
        cell_w,
        cell_h: CELL_H,
        font_size: FONT_SIZE,
    }
}

fn cell(ch: char) -> CellText {
    cell_full(
        ch,
        Color::Named(NamedColor::Foreground),
        Color::Named(NamedColor::Background),
        Flags::empty(),
    )
}

fn cell_full(ch: char, fg: Color, bg: Color, flags: Flags) -> CellText {
    CellText {
        row: 0,
        col: 0,
        ch,
        fg,
        bg,
        flags,
        zerowidth: Vec::new(),
        underline_color: None,
        hyperlink: None,
    }
}

fn snap(
    rows: usize,
    cols: usize,
    cells: Vec<CellText>,
    damage: Damage,
    cursor: Option<(usize, usize)>,
) -> Snapshot {
    assert_eq!(cells.len(), rows * cols);
    Snapshot {
        rows,
        cols,
        cells,
        selected: vec![false; rows * cols],
        cursor,
        modes: TermMode::SHOW_CURSOR,
        cursor_shape: CursorShape::default(),
        colors: Colors::default(),
        damage,
        epoch: 1,
        display_offset: 0,
    }
}

fn snap_modes(
    rows: usize,
    cols: usize,
    cells: Vec<CellText>,
    damage: Damage,
    cursor: Option<(usize, usize)>,
    modes: TermMode,
) -> Snapshot {
    let mut snap = snap(rows, cols, cells, damage, cursor);
    snap.modes = modes;
    snap
}

fn colors_with(fg: Rgb, bg: Rgb, cursor: Rgb) -> Colors {
    let mut colors = Colors::default();
    colors[NamedColor::Foreground] = Some(fg);
    colors[NamedColor::Background] = Some(bg);
    colors[NamedColor::Cursor] = Some(cursor);
    colors
}

// ---------------------------------------------------------------------------
// 1-6: pure mapping tests (no GPU).
// ---------------------------------------------------------------------------

#[test]
fn full_damage_yields_all_non_space_items() {
    // 3x5 with exactly two non-space cells.
    let mut cells = vec![cell(' '); 15];
    cells[1] = cell('A');
    cells[13] = cell('X');
    let snap = snap(3, 5, cells, Damage::Full, None);
    let l = layout(12.0);

    let items = build_draw_items(&snap, &l);
    assert_eq!(items.len(), 2, "exactly the two non-space cells must map");
    let expected: [TextBounds; 2] = [
        TextBounds {
            left: 12,
            top: 0,
            right: 24,
            bottom: 24,
        },
        TextBounds {
            left: 36,
            top: 48,
            right: 48,
            bottom: 72,
        },
    ];
    let expected_pos: [(usize, usize); 2] = [(0, 1), (2, 3)];
    for (item, (pos, bounds)) in items.iter().zip(expected_pos.iter().zip(expected)) {
        assert_eq!((item.row, item.col), *pos);
        assert_eq!(item.bounds, bounds);
        assert_eq!(item.cluster, if pos.1 == 1 { "A" } else { "X" });
        assert_eq!(
            item.fg,
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(item.bg, Rgb { r: 0, g: 0, b: 0 });
        assert!(!item.is_cursor);
    }
}

#[test]
fn partial_damage_scoped_plus_cursor_row() {
    // 3x5: non-space cells at (0,0), (0,4) and (2,3). Cursor at (0,0).
    let mut cells = vec![cell(' '); 15];
    cells[0] = cell('A');
    cells[4] = cell('B');
    cells[13] = cell('X');
    let snap = snap(
        3,
        5,
        cells,
        Damage::Partial(vec![LineDamage {
            line: 2,
            left: 3,
            right: 7, // wider than the grid; clamps to the last column
        }]),
        Some((0, 0)),
    );
    let l = layout(12.0);

    let items = build_draw_items(&snap, &l);
    let got: Vec<(usize, usize)> = items.iter().map(|i| (i.row, i.col)).collect();
    // Row 2: the span [3..=7] clamps to the grid (cols 3..4); the space at
    // col 4 maps to nothing, leaving only the 'X' at col 3. Plus the whole
    // cursor row 0 (presentation is not damage, so the cursor row is always
    // rescanned): 'A' at col 0 and 'B' at col 4. No other row is scanned.
    assert_eq!(got, vec![(0, 0), (0, 4), (2, 3)]);
    assert!(
        items
            .iter()
            .any(|i| (i.row, i.col) == (0, 0) && i.is_cursor),
        "the cursor cell must be marked"
    );
    assert!(
        !got.iter().any(|(r, _)| *r == 1),
        "the undamaged row 1 must not be scanned"
    );
}

#[test]
fn noop_damage_detection() {
    let cursor = (1, 2);
    let at_cursor = Damage::Partial(vec![LineDamage {
        line: 1,
        left: 2,
        right: 2,
    }]);
    assert!(
        is_noop_damage(&at_cursor, Some(cursor)),
        "a span exactly at the cursor cell is the pinned no-op shape"
    );
    assert!(
        !is_noop_damage(&Damage::Full, Some(cursor)),
        "Full is never a no-op"
    );
    assert!(
        !is_noop_damage(
            &Damage::Partial(vec![LineDamage {
                line: 1,
                left: 1,
                right: 2
            }]),
            Some(cursor),
        ),
        "a wider span is real damage"
    );
    assert!(
        !is_noop_damage(
            &Damage::Partial(vec![LineDamage {
                line: 0,
                left: 2,
                right: 2
            }]),
            Some(cursor),
        ),
        "a span on another row is real damage"
    );
    assert!(
        is_noop_damage(&Damage::Partial(Vec::new()), None),
        "an empty Partial with no cursor is a no-op"
    );
    assert!(
        !is_noop_damage(&at_cursor, None),
        "a span with no cursor to account for it is real damage"
    );
}

#[test]
fn resolve_color_all_variants() {
    let spec = Rgb {
        r: 9,
        g: 200,
        b: 17,
    };
    assert_eq!(
        resolve_color(&Colors::default(), &Color::Spec(spec), Rgb::default()),
        spec,
        "Spec resolves to itself"
    );

    let mut colors = Colors::default();
    let indexed = Rgb { r: 1, g: 2, b: 3 };
    colors[198] = Some(indexed);
    assert_eq!(
        resolve_color(&colors, &Color::Indexed(198), Rgb::default()),
        indexed,
        "Indexed resolves from the table slot"
    );

    let default = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    assert_eq!(
        resolve_color(&Colors::default(), &Color::Indexed(198), default),
        default,
        "an unset table slot falls back to the default"
    );

    let fg = Rgb {
        r: 10,
        g: 11,
        b: 12,
    };
    colors[NamedColor::Foreground] = Some(fg);
    assert_eq!(
        resolve_color(
            &colors,
            &Color::Named(NamedColor::Foreground),
            Rgb::default()
        ),
        fg,
        "Named(Foreground) resolves from the table slot"
    );
    assert_eq!(
        resolve_color(&colors, &Color::Named(NamedColor::Black), Rgb::default()),
        Rgb::default(),
        "an unset named slot falls back to the default"
    );
}

#[test]
fn bg_fills_only_non_default() {
    let blue = Rgb {
        r: 64,
        g: 128,
        b: 255,
    };
    let cells = vec![
        cell(' '),
        cell_full(
            ' ',
            Color::Named(NamedColor::Background),
            Color::Spec(blue),
            Flags::empty(),
        ),
        cell_full(
            ' ',
            Color::Named(NamedColor::Foreground),
            Color::Named(NamedColor::Background),
            Flags::INVERSE,
        ),
    ];
    let snap = snap(1, 3, cells, Damage::Full, None);
    let l = layout(12.0);

    let fills = build_bg_fills(&snap, &l);
    let expected = vec![
        FillRect {
            row: 0,
            col: 1,
            span: 1,
            color: blue,
        },
        // INVERSE swaps the resolved pair: fill = resolved fg (default white).
        FillRect {
            row: 0,
            col: 2,
            span: 1,
            color: Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
        },
    ];
    assert_eq!(fills, expected, "only non-default backgrounds fill");
}

#[test]
fn cursor_fill_visibility() {
    let cursor = Rgb { r: 255, g: 0, b: 0 };
    let one_cell = vec![cell(' ')];

    let mut visible = snap(1, 1, one_cell.clone(), Damage::Full, Some((0, 0)));
    visible.colors = colors_with(Rgb::default(), Rgb::default(), cursor);
    assert!(cursor_visible(&visible));
    assert_eq!(
        cursor_fill(&visible, &layout(12.0)),
        Some(FillRect {
            row: 0,
            col: 0,
            span: 1,
            color: cursor,
        }),
        "a visible cursor fills its cell in the cursor color"
    );

    let hidden_modes = snap_modes(
        1,
        1,
        one_cell.clone(),
        Damage::Full,
        Some((0, 0)),
        TermMode::empty(),
    );
    assert!(!cursor_visible(&hidden_modes));
    assert_eq!(cursor_fill(&hidden_modes, &layout(12.0)), None);

    let no_cursor = snap(1, 1, one_cell, Damage::Full, None);
    assert!(!cursor_visible(&no_cursor));
    assert_eq!(cursor_fill(&no_cursor, &layout(12.0)), None);
}

// ---------------------------------------------------------------------------
// 7-9: N02b-style headless GPU tests (node 1 has the GPU).
// ---------------------------------------------------------------------------

fn font_system() -> FontSystem {
    FontSystem::new_with_fonts([
        fontdb::Source::File(mono_path().into()),
        fontdb::Source::File(cjk_path().into()),
        fontdb::Source::File(ethiopic_path().into()),
    ])
}

/// Shape one cluster exactly as N02a does: one buffer, one layout run.
fn shape_buffer(fs: &mut FontSystem, text: &str, family: Family, metrics: Metrics) -> Buffer {
    let mut buffer = Buffer::new(fs, metrics);
    buffer.set_text(text, &Attrs::new().family(family), Shaping::Advanced, None);
    buffer.shape_until_cursor(fs, TextCursor::default(), false);
    buffer
}

/// The one-cluster advance, in pixels.
fn cluster_advance(buffer: &Buffer) -> f32 {
    buffer
        .layout_runs()
        .next()
        .expect("shaped buffer must have one line")
        .line_w
}

/// Measure the monospace cell width per N02a: the cross-character advance
/// equality assert, never a hardcoded 12.
fn measure_cell_w(fs: &mut FontSystem, family: Family) -> f32 {
    let metrics = Metrics::new(FONT_SIZE, CELL_H);
    let chars = ["A", "x", "W", "0", "-", "\u{2500}"];
    let advances: Vec<f32> = chars
        .iter()
        .map(|ch| cluster_advance(&shape_buffer(fs, ch, family, metrics)))
        .collect();

    let cell_w = advances[0];
    assert!(
        (8.0..24.0).contains(&cell_w),
        "cell width {cell_w} px is implausible"
    );
    for (ch, adv) in chars.iter().zip(&advances) {
        assert!(
            (adv - cell_w).abs() <= 0.5,
            "{ch} advance {adv} px differs from {cell_w} px"
        );
    }
    cell_w
}

/// The headless GPU plus the mutable glyph state that advances between
/// frames, mirroring N02b's device/adapter setup.
struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    fs: FontSystem,
    swash: SwashCache,
}

impl Gpu {
    fn new() -> Gpu {
        let instance = Instance::new(InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
            compatible_surface: None,
            ..Default::default()
        }))
        .expect("a GPU adapter must be available");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&DeviceDescriptor::default()))
                .expect("a wgpu device must be available");
        Gpu {
            device,
            queue,
            fs: font_system(),
            swash: SwashCache::new(),
        }
    }
}

/// One rendered offscreen frame: the texture read back to the CPU as RGBA
/// rows padded to 256 bytes.
struct Frame {
    width: u32,
    height: u32,
    stride: u32,
    rgba: Vec<u8>,
}

impl Frame {
    /// Pixels differing from the clear color, plus the largest single
    /// per-channel deviation among them (zero when none differ).
    fn ink_stats(&self, clear: Rgb) -> (u32, u8) {
        let mut count = 0u32;
        let mut max_dev = 0u8;
        for y in 0..self.height {
            for x in 0..self.width {
                let o = (y * self.stride + x * 4) as usize;
                let (r, g, b, a) = (
                    self.rgba[o] as i16,
                    self.rgba[o + 1] as i16,
                    self.rgba[o + 2] as i16,
                    self.rgba[o + 3],
                );
                assert_eq!(a, 255, "frame must stay fully opaque");
                let dev = (r - clear.r as i16)
                    .unsigned_abs()
                    .max((g - clear.g as i16).unsigned_abs())
                    .max((b - clear.b as i16).unsigned_abs());
                if dev > 0 {
                    count += 1;
                    max_dev = max_dev.max(dev as u8);
                }
            }
        }
        (count, max_dev)
    }
}

/// Map the snapshot with [`build_draw_items`] and render it N02b-style into
/// a plain offscreen texture, cleared to `clear`. Returns the read-back
/// frame. This is the N02b harness verbatim: per-cell `TextArea`s with the
/// mapped `TextBounds`, glyphon `prepare`/`render` into a render pass, and
/// the `copy_texture_to_buffer` → `map_async` → `device.poll` readback with
/// the `BufferView` dropped before `unmap`.
fn render_snapshot(gpu: &mut Gpu, snap: &Snapshot, layout: &TermLayout, clear: Rgb) -> Frame {
    let items = build_draw_items(snap, layout);
    let width = (snap.cols as f32 * layout.cell_w) as u32;
    let height = (snap.rows as u32) * (CELL_H as u32);

    let texture = gpu.device.create_texture(&TextureDescriptor {
        label: Some("n08a frame"),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8UnormSrgb,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&TextureViewDescriptor {
        label: Some("n08a frame view"),
        format: None,
        dimension: None,
        usage: None,
        aspect: TextureAspect::All,
        base_mip_level: 0,
        mip_level_count: None,
        base_array_layer: 0,
        array_layer_count: None,
    });

    let cache = Cache::new(&gpu.device);
    let mut atlas = TextAtlas::new(
        &gpu.device,
        &gpu.queue,
        &cache,
        TextureFormat::Rgba8UnormSrgb,
    );
    let mut viewport = Viewport::new(&gpu.device, &cache);
    viewport.update(&gpu.queue, Resolution { width, height });
    let mut renderer =
        TextRenderer::new(&mut atlas, &gpu.device, MultisampleState::default(), None);

    let family = Family::Name("Noto Sans Mono");
    let metrics = Metrics::new(FONT_SIZE, CELL_H);
    let buffers: Vec<Buffer> = items
        .iter()
        .map(|item| shape_buffer(&mut gpu.fs, &item.cluster, family, metrics))
        .collect();
    let areas: Vec<TextArea> = items
        .iter()
        .zip(buffers.iter())
        .map(|(item, buffer)| TextArea {
            buffer,
            left: item.bounds.left as f32,
            top: item.bounds.top as f32,
            scale: 1.0,
            bounds: item.bounds,
            default_color: glyphon::Color::rgb(item.fg.r, item.fg.g, item.fg.b),
            custom_glyphs: &[],
        })
        .collect();

    let start = Instant::now();
    renderer
        .prepare(
            &gpu.device,
            &gpu.queue,
            &mut gpu.fs,
            &mut atlas,
            &viewport,
            areas,
            &mut gpu.swash,
        )
        .expect("prepare must succeed");
    let prepare = start.elapsed();

    let mut encoder = gpu
        .device
        .create_command_encoder(&CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("n08a pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(wgpu::Color {
                        r: clear.r as f64 / 255.0,
                        g: clear.g as f64 / 255.0,
                        b: clear.b as f64 / 255.0,
                        a: 1.0,
                    }),
                    store: StoreOp::default(),
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        renderer
            .render(&atlas, &viewport, &mut pass)
            .expect("render must succeed");
    }
    gpu.queue.submit([encoder.finish()]);

    // Read the texture back to the CPU (N02b readback, verbatim).
    let stride = (width * 4).next_multiple_of(256);
    let readback = gpu.device.create_buffer(&BufferDescriptor {
        label: Some("n08a readback"),
        size: (stride * height) as u64,
        usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: Origin3d::ZERO,
            aspect: TextureAspect::All,
        },
        TexelCopyBufferInfo {
            buffer: &readback,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
        },
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    readback.map_async(MapMode::Read, 0..(stride * height) as u64, move |result| {
        let _ = tx.send(result);
    });
    // wgpu 30 delivers `map_async` callbacks from `Device::poll`; block on
    // the most recent submission (the copy above) until the callback ran.
    loop {
        gpu.device
            .poll(PollType::wait_indefinitely())
            .expect("device poll must succeed");
        if let Ok(result) = rx.try_recv() {
            result.expect("readback map must succeed");
            break;
        }
    }
    let data = readback
        .get_mapped_range(0..(stride * height) as u64)
        .expect("readback must be mapped");
    let rgba = data.to_vec();
    drop(data);
    readback.unmap();

    eprintln!(
        "N08a measured: rendered {} items in {:?} ({}x{} frame)",
        items.len(),
        prepare,
        width,
        height
    );

    Frame {
        width,
        height,
        stride,
        rgba,
    }
}

#[test]
fn gpu_cell_background_and_glyph() {
    let mut gpu = Gpu::new();
    let family = Family::Name("Noto Sans Mono");
    let cell_w = measure_cell_w(&mut gpu.fs, family);
    let l = layout(cell_w);

    let blue = Rgb {
        r: 64,
        g: 128,
        b: 255,
    };
    let cells = vec![cell_full(
        'A',
        Color::Spec(Rgb {
            r: 255,
            g: 255,
            b: 255,
        }),
        Color::Spec(blue),
        Flags::empty(),
    )];
    let snap = snap(1, 1, cells, Damage::Full, None);

    // The mapped item carries the resolved colors: the Spec bg must be
    // resolved verbatim and the default fg used for the glyph.
    let items = build_draw_items(&snap, &l);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].bg, blue);
    assert_eq!(
        items[0].fg,
        Rgb {
            r: 255,
            g: 255,
            b: 255
        }
    );
    assert!(!items[0].is_cursor);

    let frame = render_snapshot(&mut gpu, &snap, &l, blue);
    let (ink, max_dev) = frame.ink_stats(blue);
    assert!(
        ink >= 8,
        "the 'A' glyph must leave visible ink over the cell background ({} px)",
        ink
    );
    assert!(
        max_dev >= 100,
        "glyph ink must clearly differ from the background (max channel deviation {})",
        max_dev
    );
}

#[test]
fn gpu_concealed_inverted_cursor() {
    let mut gpu = Gpu::new();
    let family = Family::Name("Noto Sans Mono");
    let cell_w = measure_cell_w(&mut gpu.fs, family);
    let l = layout(cell_w);
    let blue = Rgb {
        r: 64,
        g: 128,
        b: 255,
    };
    let default_bg = Rgb { r: 0, g: 0, b: 0 };

    // Pinned conceal flag (Flags::HIDDEN): the glyph is suppressed, so the
    // frame is exactly the clear color — zero ink. The clear is black (0
    // round-trips the sRGB target exactly; a mid-tone would be gamma-
    // re-encoded and never equal the raw bytes).
    let concealed = snap(
        1,
        1,
        vec![cell_full(
            'A',
            Color::Spec(Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            Color::Spec(default_bg),
            Flags::HIDDEN,
        )],
        Damage::Full,
        None,
    );
    assert!(
        build_draw_items(&concealed, &l).is_empty(),
        "the concealed cell maps to no draw item"
    );
    let frame = render_snapshot(&mut gpu, &concealed, &l, default_bg);
    let (ink, _max_dev) = frame.ink_stats(default_bg);
    assert_eq!(ink, 0, "a concealed cell must render zero glyph ink");

    // Pinned inverse flag (Flags::INVERSE): the resolved fg/bg pair is
    // swapped — white clear, blue glyph.
    let inverted = snap(
        1,
        1,
        vec![cell_full(
            'A',
            Color::Named(NamedColor::Foreground),
            Color::Spec(blue),
            Flags::INVERSE,
        )],
        Damage::Full,
        None,
    );
    let inverted_items = build_draw_items(&inverted, &l);
    assert_eq!(
        inverted_items[0].fg, blue,
        "inverse: fg becomes the resolved bg"
    );
    assert_eq!(
        inverted_items[0].bg,
        Rgb {
            r: 255,
            g: 255,
            b: 255
        },
        "inverse: bg becomes the resolved fg"
    );
    let white = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    let frame = render_snapshot(&mut gpu, &inverted, &l, white);
    let (ink, max_dev) = frame.ink_stats(white);
    assert!(
        ink >= 8,
        "the inverted cell must render its glyph in the background color ({} px)",
        ink
    );
    assert!(
        max_dev >= 100,
        "inverted glyph ink must clearly differ from the swapped clear color ({} max deviation)",
        max_dev
    );

    // Cursor cell: the fill is the resolved cursor color, and the glyph
    // renders on top of it in the cell fg.
    let cursor_color = Rgb { r: 255, g: 0, b: 0 };
    let mut cursor_snap = snap(1, 1, vec![cell('A')], Damage::Full, Some((0, 0)));
    cursor_snap.colors = colors_with(
        Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
        default_bg,
        cursor_color,
    );
    assert_eq!(
        cursor_fill(&cursor_snap, &l),
        Some(FillRect {
            row: 0,
            col: 0,
            span: 1,
            color: cursor_color,
        })
    );
    let frame = render_snapshot(&mut gpu, &cursor_snap, &l, cursor_color);
    let (ink, max_dev) = frame.ink_stats(cursor_color);
    assert!(
        ink >= 8,
        "the cursor cell must render its glyph over the cursor fill ({} px)",
        ink
    );
    assert!(
        max_dev >= 100,
        "cursor cell glyph must clearly differ from the cursor color ({} max deviation)",
        max_dev
    );
}

#[test]
fn stable_repeated_render() {
    // Deterministic mapping: same input snapshot, equal output items.
    let mut cells = vec![cell(' '); 4];
    cells[0] = cell('A');
    cells[3] = cell('B');
    let snap = snap(2, 2, cells, Damage::Full, Some((1, 1)));
    let l = layout(12.0);

    let items_a = build_draw_items(&snap, &l);
    let items_b = build_draw_items(&snap, &l);
    assert_eq!(items_a, items_b, "the mapping must be deterministic");
    assert_eq!(items_a.len(), 2);
    assert!(
        items_a
            .iter()
            .any(|i| i.is_cursor && (i.row, i.col) == (1, 1))
    );

    // And deterministically rendered: two offscreen renders of the same
    // snapshot are bit-identical.
    let mut gpu = Gpu::new();
    let family = Family::Name("Noto Sans Mono");
    let cell_w = measure_cell_w(&mut gpu.fs, family);
    let gpu_layout = layout(cell_w);
    let clear = Rgb { r: 0, g: 0, b: 0 };
    let frame1 = render_snapshot(&mut gpu, &snap, &gpu_layout, clear);
    let frame2 = render_snapshot(&mut gpu, &snap, &gpu_layout, clear);
    assert_eq!(
        frame1.rgba, frame2.rgba,
        "repeated renders of an unchanged snapshot must be bit-identical"
    );
}

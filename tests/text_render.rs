//! N02b: offscreen render of a tiny cell grid via per-cell `TextArea`s.
//!
//! Proves the per-cell model from N02a survives rasterization on the pinned
//! glyphon 0.12.0 / wgpu 30.0.1 tuple, headless:
//!
//! - The grid renders into a plain wgpu texture (no surface, no window, no
//!   display handle) and is read back to the CPU; frame 1 is written as a
//!   PPM for manual inspection.
//! - Each cell is one cosmic-text `Buffer` placed at `(col * cell_w, row *
//!   cell_h)` with `TextArea.bounds` set to the cell's allocation: one cell
//!   for narrow clusters, two for the CJK and Ethiopic wide clusters.
//! - Glyphon clips ink geometrically to `bounds`; the readback asserts
//!   every allocation contains ink and that no ink exists outside any
//!   allocation (no column drift, no vertical overflow).
//! - Frames 2 and 3 with unchanged cells must render pixel-identically,
//!   with frame 3 rendered through a faceless `FontSystem` so that only an
//!   atlas-cache hit can supply the glyphs (cache hit, no re-rasterization).
//!
//! Pinned test fonts: the same three faces as `tests/text_placement.rs`.
//!
//! Cache strategy record (glyphon 0.12 public API): the persistent glyph
//! cache is `TextAtlas`'s per-content `glyph_cache` — `pub(crate)`, an
//! unbounded `LruCache` protected from eviction while a glyph is in use,
//! with no size introspection on the public API. `prepare` rasterizes
//! misses through `SwashCache::get_image_uncached`, which never writes
//! `SwashCache.image_cache`, so the cache is observed behaviorally: frame 3
//! re-renders the unchanged cells through a hermetic `FontSystem` with an
//! empty `fontdb::Database`; with no faces resolvable, any cache miss would
//! fail rasterization and drop the glyph, so frame 3's pixel-identical
//! output proves every glyph was served from the atlas cache. The atlas's
//! growth factor is the distinct (font, size, codepoint) set the session
//! renders — the working set, not scrollback — and `TextAtlas::trim` is the
//! public mitigation if it ever grows unboundedly.

use std::io::Write;
use std::time::{Duration, Instant};

use glyphon::{
    Attrs, Buffer, Cache, Color, Cursor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, fontdb,
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
                    "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
                    "/usr/share/fonts/google-noto/NotoSansMono-Regular.ttf",
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
                    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
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
                    "/usr/share/fonts/truetype/noto/NotoSerifEthiopic-Regular.ttf",
                    "/usr/share/fonts/google-noto/NotoSerifEthiopic-Regular.ttf",
                    "/nix/store/8m9r3c9q6lx9rj9vffs5d53h0w0cs4a9-noto-fonts-2026.05.01/share/fonts/noto/NotoSerifEthiopic.ttf",
                ],
            )
        })
        .as_str()
}

/// 20 px font in a 24 px row — same metrics as N02a.
const FONT_SIZE: f32 = 20.0;
const CELL_H: f32 = 24.0;
const COLS: u32 = 5;
const ROWS: u32 = 2;

/// The background the frame is cleared to; ink is any pixel that differs.
const BG: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// Grid contents: (row, col, text, cell span). Wide clusters span two cells.
#[derive(Debug)]
struct CellSpec {
    row: u32,
    col: u32,
    text: &'static str,
    span: u32,
}

const CELLS: &[CellSpec] = &[
    CellSpec {
        row: 0,
        col: 0,
        text: "A",
        span: 1,
    },
    CellSpec {
        row: 0,
        col: 1,
        text: "e\u{301}",
        span: 1,
    },
    CellSpec {
        row: 0,
        col: 2,
        text: "你",
        span: 2,
    },
    CellSpec {
        row: 0,
        col: 4,
        text: "\u{2500}",
        span: 1,
    },
    CellSpec {
        row: 1,
        col: 1,
        text: "\u{1200}",
        span: 2,
    },
];

fn font_system() -> FontSystem {
    FontSystem::new_with_fonts([
        fontdb::Source::File(mono_path().into()),
        fontdb::Source::File(cjk_path().into()),
        fontdb::Source::File(ethiopic_path().into()),
    ])
}

/// Family name of the first face in the db whose family starts with `prefix`.
fn family_name(fs: &FontSystem, prefix: &str) -> String {
    fs.db()
        .faces()
        .find(|face| {
            face.families
                .first()
                .is_some_and(|(name, _)| name.starts_with(prefix))
        })
        .unwrap_or_else(|| panic!("pinned font with family starting {prefix:?} did not load"))
        .families[0]
        .0
        .clone()
}

/// Shape one cell exactly as N02a does: one buffer, one layout run.
fn shape_cell(fs: &mut FontSystem, text: &str, family: Family, metrics: Metrics) -> Buffer {
    let mut buffer = Buffer::new(fs, metrics);
    buffer.set_text(text, &Attrs::new().family(family), Shaping::Advanced, None);
    buffer.shape_until_cursor(fs, Cursor::default(), false);
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

/// The immutable offscreen scene: the render target, the viewport, and the
/// per-cell shaped buffers with their placement.
#[derive(Clone, Copy)]
struct Grid<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    view: &'a wgpu::TextureView,
    viewport: &'a Viewport,
    buffers: &'a [Buffer],
    cw: f32,
    cell_w: u32,
}

/// The mutable GPU state that advances between frames.
struct GpuState<'a> {
    renderer: &'a mut TextRenderer,
    atlas: &'a mut TextAtlas,
    fs: &'a mut FontSystem,
    swash: &'a mut SwashCache,
}

/// One frame: prepare the cell text areas (timed), then render them over a
/// cleared background.
fn render_frame(grid: Grid<'_>, state: &mut GpuState<'_>) -> Duration {
    let Grid {
        device,
        queue,
        view,
        viewport,
        buffers,
        cw,
        cell_w,
    } = grid;
    let GpuState {
        renderer,
        atlas,
        fs,
        swash,
    } = state;
    let mut areas: Vec<TextArea> = Vec::new();
    for (cell, buffer) in CELLS.iter().zip(buffers) {
        areas.push(TextArea {
            buffer,
            left: cell.col as f32 * cw,
            top: cell.row as f32 * CELL_H,
            scale: 1.0,
            bounds: TextBounds {
                left: cell.col as i32 * cell_w as i32,
                top: cell.row as i32 * CELL_H as i32,
                right: (cell.col + cell.span) as i32 * cell_w as i32,
                bottom: (cell.row + 1) as i32 * CELL_H as i32,
            },
            default_color: Color::rgb(0xFF, 0xFF, 0xFF),
            custom_glyphs: &[],
        });
    }

    let start = Instant::now();
    renderer
        .prepare(device, queue, fs, atlas, viewport, areas, swash)
        .expect("prepare must succeed");
    let prepare = start.elapsed();

    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("n02b pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(BG),
                    store: StoreOp::default(),
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        renderer
            .render(atlas, viewport, &mut pass)
            .expect("render must succeed");
    }
    queue.submit([encoder.finish()]);
    prepare
}

/// Read the rendered texture back to the CPU as RGBA rows padded to 256 bytes.
fn read_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    stride: u32,
    readback: &wgpu::Buffer,
) -> Vec<u8> {
    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: Origin3d::ZERO,
            aspect: TextureAspect::All,
        },
        TexelCopyBufferInfo {
            buffer: readback,
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
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    readback.map_async(MapMode::Read, 0..(stride * height) as u64, move |result| {
        let _ = tx.send(result);
    });
    // wgpu 30 delivers `map_async` callbacks from `Device::poll`; block on
    // the most recent submission (the copy above) until the callback ran.
    loop {
        device
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
    let out = data.to_vec();
    drop(data);
    readback.unmap();
    out
}

/// Write a PPM (P6) of an RGBA readback whose rows are padded to `stride`.
fn write_ppm(path: &str, rgba: &[u8], width: u32, height: u32, stride: u32) {
    let mut file =
        std::io::BufWriter::new(std::fs::File::create(path).expect("PPM output path writable"));
    file.write_all(format!("P6\n{width} {height}\n255\n").as_bytes())
        .unwrap();
    for y in 0..height as usize {
        for x in 0..width as usize {
            let o = y * stride as usize + x * 4;
            file.write_all(&[rgba[o], rgba[o + 1], rgba[o + 2]])
                .unwrap();
        }
    }
    file.flush().unwrap();
}

/// Inclusive bounding box of ink (any pixel differing from BG) inside the
/// given pixel rect, or None if the rect holds no ink.
fn ink_rect(
    rgba: &[u8],
    stride: u32,
    rect: (u32, u32, u32, u32), // (left, top, right, bottom)
) -> Option<(u32, u32, u32, u32)> {
    let (l, t, r, b) = rect;
    let mut min_x = u32::MAX;
    let mut min_y = u32::MAX;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for y in t..b {
        for x in l..r {
            let o = (y * stride + x * 4) as usize;
            if rgba[o] != 0 || rgba[o + 1] != 0 || rgba[o + 2] != 0 || rgba[o + 3] != 255 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x + 1);
                max_y = max_y.max(y + 1);
            }
        }
    }
    if min_x == u32::MAX {
        None
    } else {
        Some((min_x, min_y, max_x, max_y))
    }
}

#[test]
fn offscreen_cell_grid_renders_without_drift() {
    // The mono advance defines the terminal cell width; it must be a whole
    // pixel so terminal math and shaped geometry agree exactly.
    let mut fs = font_system();
    let mono = Family::Name("Noto Sans Mono");
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
    let cw = cell_w as f32;
    let width = COLS * cell_w;
    let height = ROWS * CELL_H as u32;

    // Headless GPU: no display handle, no surface — render into a texture.
    let instance = Instance::new(InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
        compatible_surface: None,
        ..Default::default()
    }))
    .expect("a GPU adapter must be available");
    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor::default()))
        .expect("a wgpu device must be available");

    let texture = device.create_texture(&TextureDescriptor {
        label: Some("n02b grid"),
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
        label: Some("n02b grid view"),
        format: None,
        dimension: None,
        usage: None,
        aspect: TextureAspect::All,
        base_mip_level: 0,
        mip_level_count: None,
        base_array_layer: 0,
        array_layer_count: None,
    });

    // Glyphon setup, plus the readback buffer (256-byte-padded rows).
    let cache = Cache::new(&device);
    let mut atlas = TextAtlas::new(&device, &queue, &cache, TextureFormat::Rgba8UnormSrgb);
    let mut viewport = Viewport::new(&device, &cache);
    viewport.update(&queue, Resolution { width, height });
    let mut renderer = TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
    let mut swash = SwashCache::new();
    let stride = (width * 4).next_multiple_of(256);
    let readback = device.create_buffer(&BufferDescriptor {
        label: Some("n02b readback"),
        size: (stride * height) as u64,
        usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    // One cosmic buffer per cell, under the family its allocation was
    // chosen for (pinned mono for narrow cells, the pinned fallback faces
    // for the wide ones).
    let cjk_name = family_name(&fs, "Noto Sans CJK");
    let ethiopic_name = family_name(&fs, "Noto Serif Ethiopic");
    let cjk = Family::Name(cjk_name.as_str());
    let ethiopic = Family::Name(ethiopic_name.as_str());
    let buffers: Vec<Buffer> = CELLS
        .iter()
        .map(|cell| match cell.text {
            "A" | "e\u{301}" | "\u{2500}" => shape_cell(&mut fs, cell.text, mono, metrics),
            "你" => shape_cell(&mut fs, cell.text, cjk, metrics),
            "\u{1200}" => shape_cell(&mut fs, cell.text, ethiopic, metrics),
            _ => panic!("unhandled cell text {cell:?}"),
        })
        .collect();

    // The scene is static; only the GPU state and the FontSystem advance
    // between frames.
    let grid = Grid {
        device: &device,
        queue: &queue,
        view: &view,
        viewport: &viewport,
        buffers: &buffers,
        cw,
        cell_w,
    };
    let mut warm = GpuState {
        renderer: &mut renderer,
        atlas: &mut atlas,
        fs: &mut fs,
        swash: &mut swash,
    };

    // Frame 1 (cold): rasterizes every glyph into the atlas cache.
    let frame1_prepare = render_frame(grid, &mut warm);
    let frame1 = read_frame(&device, &queue, &texture, width, height, stride, &readback);
    write_ppm(
        &format!("{}/n02b_grid.ppm", env!("CARGO_TARGET_TMPDIR")),
        &frame1,
        width,
        height,
        stride,
    );

    // Per-cell ink must sit inside its allocation, and every allocation
    // must hold ink (an empty allocation means the glyph never rendered).
    for cell in CELLS {
        let rect = (
            cell.col * cell_w,
            cell.row * CELL_H as u32,
            (cell.col + cell.span) * cell_w,
            (cell.row + 1) * CELL_H as u32,
        );
        let (ix, iy, wx, wy) = ink_rect(&frame1, stride, rect)
            .unwrap_or_else(|| panic!("cell {:?} produced no ink", cell.text));
        assert!(
            ix >= rect.0 && wx <= rect.2 && iy >= rect.1 && wy <= rect.3,
            "cell {:?} ink ({ix},{iy})-({wx},{wy}) escapes its allocation {:?} px",
            cell.text,
            rect
        );
    }

    // No ink may exist outside every allocation: that is where column
    // drift or vertical overflow would land.
    for y in 0..height {
        for x in 0..width {
            let in_any = CELLS.iter().any(|cell| {
                (cell.col * cell_w..(cell.col + cell.span) * cell_w).contains(&x)
                    && (cell.row * CELL_H as u32..(cell.row + 1) * CELL_H as u32).contains(&y)
            });
            let o = (y * stride + x * 4) as usize;
            assert!(
                in_any
                    || (frame1[o] == 0
                        && frame1[o + 1] == 0
                        && frame1[o + 2] == 0
                        && frame1[o + 3] == 255),
                "unexpected ink at ({x},{y}) outside every cell allocation"
            );
        }
    }

    // Frame 2 (warm, unchanged cells, same FontSystem a real app would
    // hold): the frame must come back pixel-identical.
    let frame2_prepare = render_frame(grid, &mut warm);
    let frame2 = read_frame(&device, &queue, &texture, width, height, stride, &readback);
    assert_eq!(
        frame1, frame2,
        "unchanged frame 2 must render pixel-identically"
    );

    // Frame 3 (proof of zero re-rasterization on an unchanged frame): the
    // same cells are re-rendered through a hermetic FontSystem whose font
    // database is empty, so no face can be rasterized — a glyph can only
    // be served from the atlas cache, and a miss would drop it.
    let mut bare_fs = FontSystem::new_with_locale_and_db_and_fallback(
        "en-US".into(),
        fontdb::Database::new(),
        glyphon::cosmic_text::PlatformFallback,
    );
    let mut cold = GpuState {
        renderer: &mut renderer,
        atlas: &mut atlas,
        fs: &mut bare_fs,
        swash: &mut swash,
    };
    let frame3_prepare = render_frame(grid, &mut cold);
    let frame3 = read_frame(&device, &queue, &texture, width, height, stride, &readback);
    assert_eq!(
        frame1, frame3,
        "frame 3 (faceless FontSystem) must render pixel-identically: \
         every glyph must have been served from the atlas cache"
    );

    eprintln!(
        "N02b measured: cell {cw:.1} px; frame 1 prepare {:?}, frame 2 prepare {:?}, \
         frame 3 (faceless fs) prepare {:?}; frames 2-3 pixel-identical to frame 1, so the \
         unchanged frame served every glyph from the atlas cache; \
         PPM /tmp/opencode/n02b_grid.ppm ({width}x{height})",
        frame1_prepare, frame2_prepare, frame3_prepare
    );
}

//! wgpu surface/device setup for a nora window (N01), extended with the
//! pinned wgpu 30 live-frame path (N08b2b).
//!
//! The wgpu instance is bound to the winit event loop's display handle so the
//! surface presents to whichever native compositor (Wayland or X11) owns the
//! window. Terminal state does not live here; this module owns GPU resources
//! only.
//!
//! Live frames are driven acquire → draw → present: [`Graphics::acquire`]
//! fetches the current surface texture and maps the pinned wgpu 30
//! `CurrentSurfaceTexture` outcome onto nora's in-tree [`SurfaceStatus`];
//! [`Graphics::draw_frame`] renders the visible cells exactly the way
//! `tests/text_render.rs` does; [`Graphics::present`] finishes, submits and
//! presents the frame (submit before `queue.present`, the pinned wgpu 30
//! order).

use std::sync::Arc;

use super::glyphs;
use super::images::Band;
use glyphon::{
    Cache, Color, Metrics, Resolution, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer,
    Viewport,
};
use wgpu::{
    Adapter, BlendState, BufferDescriptor, BufferUsages, ColorTargetState, CommandEncoder,
    CommandEncoderDescriptor, CurrentSurfaceTexture, DeviceDescriptor, DeviceLostReason,
    FragmentState, Instance, InstanceDescriptor, LoadOp, MultisampleState, Operations,
    PipelineCompilationOptions, PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor,
    RequestAdapterOptions, ShaderModuleDescriptor, ShaderSource, StoreOp, Surface, SurfaceTexture,
    TextureAspect, TextureFormat, TextureView, TextureViewDescriptor, VertexAttribute,
    VertexBufferLayout, VertexFormat, VertexState,
};
use winit::{dpi::PhysicalSize, event_loop::ActiveEventLoop, window::Window};

use nora::render::glyph::GlyphCache;
use nora::render::state::SurfaceStatus;
use nora::render::style::{CellStyle, decoration_style};

/// One cell to draw in a live frame (N13a): its grid position, the full
/// cluster text (the cell character plus its combining marks), the resolved
/// [`CellStyle`], and the advance width in cells (2 for a wide char, 1
/// otherwise; spacers are filtered out before reaching the render pass).
pub struct DrawCell {
    /// Viewport row (0 = top).
    pub row: u32,
    /// Viewport column (0 = left).
    pub col: u32,
    /// The full cluster: the cell character followed by its zerowidth marks.
    pub text: CellText,
    /// Resolved visual style (fg/bg/bold/italic/faint/conceal/underline/
    /// strikethrough).
    pub style: CellStyle,
    /// Advance width in cells (1 or 2).
    pub width: u16,
}

/// A cell's cluster text, kept inline when it fits (a character and a few
/// combining marks), so building a frame does not allocate per cell (audit
/// R12). Longer clusters use the heap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellText {
    Inline { len: u8, bytes: [u8; 15] },
    Heap(String),
}

impl CellText {
    /// The cluster of `ch` followed by its combining `marks`.
    pub fn cluster(ch: char, marks: &[char]) -> Self {
        let len = ch.len_utf8() + marks.iter().map(|m| m.len_utf8()).sum::<usize>();
        if len > 15 {
            let mut text = String::with_capacity(len);
            text.push(ch);
            text.extend(marks);
            return Self::Heap(text);
        }
        let mut bytes = [0u8; 15];
        let mut at = ch.encode_utf8(&mut bytes).len();
        for mark in marks {
            at += mark.encode_utf8(&mut bytes[at..]).len();
        }
        Self::Inline {
            len: at as u8,
            bytes,
        }
    }

    /// The cluster as a string slice.
    pub fn as_str(&self) -> &str {
        match self {
            // Built only from whole UTF-8 encodings in `cluster`.
            Self::Inline { len, bytes } => {
                std::str::from_utf8(&bytes[..usize::from(*len)]).unwrap_or_default()
            }
            Self::Heap(text) => text,
        }
    }
}

impl std::ops::Deref for CellText {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl From<char> for CellText {
    fn from(ch: char) -> Self {
        Self::cluster(ch, &[])
    }
}

impl From<&str> for CellText {
    fn from(text: &str) -> Self {
        let mut chars = text.chars();
        match chars.next() {
            Some(ch) => {
                let marks: Vec<char> = chars.collect();
                Self::cluster(ch, &marks)
            }
            None => Self::Inline {
                len: 0,
                bytes: [0; 15],
            },
        }
    }
}

/// One solid rectangle under the text: a cell background or the block
/// cursor, `span` cells wide at (`row`, `col`), drawn before the selection
/// and search fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellFill {
    pub row: u32,
    pub col: u32,
    pub span: u16,
    pub color: [u8; 4],
}

/// The glyphon atlas for `format` and the text renderer bound to it. Used at
/// startup and again whenever the font system is replaced, so both paths
/// build the same pipeline.
fn text_pipeline(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cache: &Cache,
    format: TextureFormat,
) -> (TextAtlas, TextRenderer) {
    let mut atlas = TextAtlas::new(device, queue, cache, format);
    let renderer = TextRenderer::new(&mut atlas, device, MultisampleState::default(), None);
    (atlas, renderer)
}

/// One vertex for the fill/decoration pass: position in normalized device
/// coordinates (location 0, offset 0), two padding floats, and the resolved
/// sRGB-space RGBA (location 1, offset 16), in a 32-byte stride.
#[repr(C)]
#[derive(Clone, Copy)]
struct DecoVertex {
    pos: [f32; 2],
    _pad: [f32; 2],
    color: [f32; 4],
}

/// Build the fill pipeline shared by cell backgrounds, the cursor, the
/// selection/search highlights and underline/strikethrough lines: solid
/// per-vertex-colored rectangles in the surface format, no blending.
fn decoration_pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("nora decoration shader"),
        source: ShaderSource::Wgsl(
            r#"
struct VSOut {
    @builtin(position) pos: vec4f,
    @location(0) color: vec4f,
};

// Set when the render target is an `*Srgb` format: the hardware encodes
// the fragment output linear -> sRGB on write, so the sRGB-space input
// color is decoded to linear first (glyphon does the same for text).
override SRGB_TARGET: bool = false;

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

@vertex
fn vs_main(@location(0) pos: vec2f, @location(1) color: vec4f) -> VSOut {
    var out: VSOut;
    out.pos = vec4f(pos, 0.0, 1.0);
    if SRGB_TARGET {
        out.color = vec4f(
            srgb_to_linear(color.r),
            srgb_to_linear(color.g),
            srgb_to_linear(color.b),
            color.a,
        );
    } else {
        out.color = color;
    }
    return out;
}

@fragment
fn fs_main(@location(0) color: vec4f) -> @location(0) vec4f {
    return color;
}"#
            .into(),
        ),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("nora decoration pipeline"),
        layout: None,
        vertex: VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: PipelineCompilationOptions {
                constants: &[("SRGB_TARGET", if format.is_srgb() { 1.0 } else { 0.0 })],
                ..PipelineCompilationOptions::default()
            },
            buffers: &[Some(VertexBufferLayout {
                array_stride: 32,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    VertexAttribute {
                        shader_location: 0,
                        offset: 0,
                        format: VertexFormat::Float32x2,
                    },
                    // `DecoVertex::color`, after `pos` and the 8-byte pad.
                    VertexAttribute {
                        shader_location: 1,
                        offset: 16,
                        format: VertexFormat::Float32x4,
                    },
                ],
            })],
        },
        fragment: Some(FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: PipelineCompilationOptions::default(),
            targets: &[Some(ColorTargetState {
                format,
                blend: Some(BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::default(),
            })],
        }),
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// One decoration line in pixel space, before NDC conversion (N13c).
#[derive(Clone, Copy)]
struct DecoLine {
    /// Left edge (px).
    x0: f32,
    /// Right edge (px).
    x1: f32,
    /// Top of the cell's row band (px).
    top: f32,
    /// Bottom of the cell's row band (px).
    bottom: f32,
    /// Top of the line (px, from the baseline).
    y: f32,
    /// Line thickness (px).
    thickness: f32,
    /// Resolved RGBA.
    color: [u8; 4],
}

/// Push one decoration line (clipped to the cell's row band) as two
/// triangles of NDC [`DecoVertex`]es (N13c).
fn push_deco_rect(out: &mut Vec<DecoVertex>, line: &DecoLine, frame_w: f32, frame_h: f32) {
    let y0 = line.y.max(line.top);
    let y1 = (line.y + line.thickness).min(line.bottom);
    if y1 <= y0 {
        return;
    }
    let c: [f32; 4] = [
        line.color[0] as f32 / 255.0,
        line.color[1] as f32 / 255.0,
        line.color[2] as f32 / 255.0,
        line.color[3] as f32 / 255.0,
    ];
    let ndc =
        |px: f32, py: f32| -> [f32; 2] { [2.0 * px / frame_w - 1.0, 1.0 - 2.0 * py / frame_h] };
    let v = |pos: [f32; 2]| DecoVertex {
        pos,
        _pad: [0.0; 2],
        color: c,
    };
    let a = ndc(line.x0, y0);
    let b = ndc(line.x1, y0);
    let d = ndc(line.x1, y1);
    let e = ndc(line.x0, y1);
    out.extend([v(a), v(b), v(d), v(a), v(d), v(e)]);
}

/// The frame clear color for an sRGB-space RGBA background: decoded to
/// linear when the target is an `*Srgb` format (the hardware re-encodes on
/// write), passed through otherwise.
fn clear_color(rgba: [u8; 4], srgb_target: bool) -> wgpu::Color {
    let channel = |v: u8| {
        let c = f64::from(v) / 255.0;
        if !srgb_target {
            c
        } else if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    wgpu::Color {
        r: channel(rgba[0]),
        g: channel(rgba[1]),
        b: channel(rgba[2]),
        a: f64::from(rgba[3]) / 255.0,
    }
}

/// The selection highlight background (N14b), as resolved RGBA: a muted
/// blue drawn under the selected cells before the glyphs.
pub const SELECTION_BG: [u8; 4] = [0x33, 0x55, 0x77, 255];

/// The search highlight background (N20b), as resolved RGBA: a muted
/// amber drawn under the current match's span; drawn after the selection
/// fill so an active match wins over a selection.
const SEARCH_BG: [u8; 4] = [0x77, 0x55, 0x22, 255];

/// The seven pinned wgpu 30 surface-acquire outcomes, payload-stripped:
/// `Success` and `Suboptimal` carry a surface texture that nora keeps on the
/// in-flight [`Frame`], not on the status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireStatus {
    /// Acquired a usable texture.
    Success,
    /// Acquired a texture that no longer matches the surface properties.
    Suboptimal,
    /// Acquire timed out.
    Timeout,
    /// The window is occluded.
    Occluded,
    /// The surface configuration is outdated.
    Outdated,
    /// The surface has been lost.
    Lost,
    /// Acquire failed validation.
    Validation,
}

/// Extract the payload-stripped outcome from a pinned wgpu 30
/// [`CurrentSurfaceTexture`] (1:1 with the pinned variants).
pub fn classify(texture: &CurrentSurfaceTexture) -> AcquireStatus {
    match texture {
        CurrentSurfaceTexture::Success(_) => AcquireStatus::Success,
        CurrentSurfaceTexture::Suboptimal(_) => AcquireStatus::Suboptimal,
        CurrentSurfaceTexture::Timeout => AcquireStatus::Timeout,
        CurrentSurfaceTexture::Occluded => AcquireStatus::Occluded,
        CurrentSurfaceTexture::Outdated => AcquireStatus::Outdated,
        CurrentSurfaceTexture::Lost => AcquireStatus::Lost,
        CurrentSurfaceTexture::Validation => AcquireStatus::Validation,
    }
}

/// Pure boundary mapping (N08b1) from an acquire outcome to nora's in-tree
/// [`SurfaceStatus`].
///
/// The pinned wgpu 30.0.1 `Validation` variant carries no payload
/// (`wgpu-30.0.1/src/api/surface_texture.rs:79`), so the message is fixed
/// here; every other variant maps to the identically named status.
pub fn surface_status(status: AcquireStatus) -> SurfaceStatus {
    match status {
        AcquireStatus::Success => SurfaceStatus::Success,
        AcquireStatus::Suboptimal => SurfaceStatus::Suboptimal,
        AcquireStatus::Timeout => SurfaceStatus::Timeout,
        AcquireStatus::Occluded => SurfaceStatus::Occluded,
        AcquireStatus::Outdated => SurfaceStatus::Outdated,
        AcquireStatus::Lost => SurfaceStatus::Lost,
        AcquireStatus::Validation => {
            SurfaceStatus::Validation("surface texture validation failed".to_owned())
        }
    }
}

/// One in-flight surface frame: the mapped acquire [`SurfaceStatus`] and, on
/// `Success`/`Suboptimal`, the live surface texture with its render-target
/// view.
///
/// Drive it through [`Graphics::draw_frame`] and [`Graphics::present`].
/// Dropping a frame discards its un-presented texture (pinned wgpu 30
/// `SurfaceTexture` drop semantics), so an errored frame must be presented
/// or dropped before the next [`Graphics::set_size`]: `Surface::configure`
/// panics while a `SurfaceTexture` is alive.
pub struct Frame {
    /// The pinned wgpu acquire outcome, mapped to nora's in-tree status.
    pub status: SurfaceStatus,
    texture: Option<SurfaceTexture>,
    view: Option<TextureView>,
    encoder: CommandEncoder,
}

/// GPU side of a nora window: a surface and the device presenting to it, plus
/// the glyph rasterization state for live frames (N08b2b).
pub struct Graphics {
    /// Kept to recreate the surface after `SurfaceError::Lost`.
    instance: Instance,
    window: Arc<Window>,
    adapter: Adapter,
    device: wgpu::Device,
    /// Submitted by the first render pass (N02 text adapter).
    #[allow(dead_code)]
    pub queue: wgpu::Queue,
    surface: Surface<'static>,
    /// Tracked so a zero-sized `set_size` never reconfigures the swapchain.
    configured_size: Option<PhysicalSize<u32>>,
    format: Option<wgpu::TextureFormat>,
    /// Kept alive for the atlas and viewport (N02 pattern), and reused when
    /// the atlas is rebuilt for a new font system.
    cache: Cache,
    atlas: TextAtlas,
    swash: SwashCache,
    viewport: Viewport,
    renderer: TextRenderer,
    /// Retained per-cell glyph cache (N27b): reuses an already-shaped buffer
    /// for an unchanged cell instead of re-shaping every cell every frame.
    glyph_cache: GlyphCache,
    /// N13c decoration (underline/strikethrough) fill pipeline.
    deco_pipeline: wgpu::RenderPipeline,
    /// Kitty graphics (ADR-014).
    image_renderer: super::images::ImageRenderer,
    /// Selection highlight color (`[colors] selection_background`).
    selection_bg: [u8; 4],
}

impl Graphics {
    /// Create the instance, surface, device and glyph state for `window`.
    ///
    /// The surface is not configured until [`Self::set_size`] receives a
    /// non-zero physical size, so zero-sized surfaces never reach the
    /// swapchain. `device_lost` is wired to the pinned wgpu 30
    /// `set_device_lost_callback` so the window loop can signal loss (b-2);
    /// b-1 passes a no-op.
    pub async fn new(
        event_loop: &ActiveEventLoop,
        window: &Arc<Window>,
        device_lost: impl Fn(DeviceLostReason, String) + Send + 'static,
    ) -> Result<Self, String> {
        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));

        let surface = instance
            .create_surface(window.clone())
            .map_err(|err| format!("failed to create window surface: {err}"))?;

        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|err| format!("no GPU adapter compatible with the window surface: {err}"))?;

        // Negotiate the presentation format before creating the
        // glyphon atlas: glyphon's `TextAtlas` builds the render
        // pipeline from the atlas format, which must match the
        // surface's render pass. Prefer Rgba8UnormSrgb (the N02
        // offscreen default); otherwise take the adapter's preferred
        // format (e.g. Bgra8UnormSrgb on radv).
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .find(|f| **f == TextureFormat::Rgba8UnormSrgb)
            .copied()
            .or_else(|| caps.formats.first().copied())
            .ok_or("surface reports no usable texture formats")?;

        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default())
            .await
            .map_err(|err| format!("failed to create wgpu device: {err}"))?;

        device.set_device_lost_callback(device_lost);

        // The N02 offscreen pattern (tests/text_render.rs), live.
        let cache = Cache::new(&device);
        let (atlas, renderer) = text_pipeline(&device, &queue, &cache, format);
        let swash = SwashCache::new();
        let viewport = Viewport::new(&device, &cache);
        let deco_pipeline = decoration_pipeline(&device, format);
        let image_renderer = super::images::ImageRenderer::new(&device, format);

        Ok(Self {
            instance,
            window: window.clone(),
            adapter,
            device,
            queue,
            surface,
            configured_size: None,
            format: Some(format),
            cache,
            atlas,
            swash,
            viewport,
            renderer,
            glyph_cache: GlyphCache::new(),
            deco_pipeline,
            image_renderer,
            selection_bg: SELECTION_BG,
        })
    }

    /// Metadata about the adapter presenting to this window.
    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }

    /// Physical size the surface is configured for, if ever.
    #[allow(dead_code)]
    pub fn configured_size(&self) -> Option<PhysicalSize<u32>> {
        self.configured_size
    }

    /// Texture format in use, once configured.
    pub fn format(&self) -> Option<wgpu::TextureFormat> {
        self.format
    }

    /// (Re)configure the surface for a new physical size, and resize the
    /// glyph viewport to match (N08b2b).
    ///
    /// Zero-sized surfaces are ignored: the last valid configuration remains
    /// until a non-zero size arrives.
    pub fn set_size(&mut self, size: PhysicalSize<u32>) -> Result<(), String> {
        if self.configured_size == Some(size) {
            return Ok(());
        }
        self.configure(size)
    }

    /// Reconfigure the surface at the window's current size even when the
    /// size is unchanged: the recovery for `Suboptimal` and `Outdated`
    /// acquires, which `set_size` would skip.
    pub fn reconfigure(&mut self) -> Result<(), String> {
        let size = self.window.inner_size();
        self.configure(size)
    }

    /// Replace a lost surface with a new one for the same window and
    /// configure it (`SurfaceError::Lost` cannot be fixed by reconfiguring).
    pub fn recreate_surface(&mut self) -> Result<(), String> {
        self.surface = self
            .instance
            .create_surface(self.window.clone())
            .map_err(|err| format!("failed to recreate window surface: {err}"))?;
        self.configured_size = None;
        self.reconfigure()
    }

    fn configure(&mut self, size: PhysicalSize<u32>) -> Result<(), String> {
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let mut config = self
            .surface
            .get_default_config(&self.adapter, size.width, size.height)
            .ok_or("adapter does not support this surface at the requested size")?;
        // Force the negotiated presentation format; the default
        // config may pick another supported one (e.g. Bgra on radv).
        config.format = self.format.unwrap_or(config.format);
        self.surface.configure(&self.device, &config);
        self.viewport.update(
            &self.queue,
            Resolution {
                width: size.width,
                height: size.height,
            },
        );
        self.configured_size = Some(size);
        self.format = Some(config.format);
        Ok(())
    }

    /// Acquire the next surface frame (pinned wgpu 30
    /// `Surface::get_current_texture` flow).
    ///
    /// The frame carries the mapped [`SurfaceStatus`] and, on
    /// `Success`/`Suboptimal`, the surface texture with a render-target view
    /// over it (wgpu 30 has no `SurfaceTexture::create_view`; the view is
    /// created on the underlying texture).
    pub fn acquire(&mut self) -> Frame {
        let cst = self.surface.get_current_texture();
        let status = surface_status(classify(&cst));
        let (texture, view) = match cst {
            CurrentSurfaceTexture::Success(st) | CurrentSurfaceTexture::Suboptimal(st) => {
                let view = st.texture.create_view(&TextureViewDescriptor {
                    label: Some("nora surface frame view"),
                    format: None,
                    dimension: None,
                    usage: None,
                    aspect: TextureAspect::All,
                    base_mip_level: 0,
                    mip_level_count: None,
                    base_array_layer: 0,
                    array_layer_count: None,
                });
                (Some(st), Some(view))
            }
            _ => (None, None),
        };
        let encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor::default());
        Frame {
            status,
            texture,
            view,
            encoder,
        }
    }

    /// Render one frame of terminal content into an acquired [`Frame`]
    /// (N08b2b), restructured for per-cell style (N13a).
    ///
    /// Each [`DrawCell`] is shaped with its resolved weight/style and placed
    /// with a cell-allocated [`TextBounds`] whose right edge spans the cell's
    /// advance width (a wide char spans 2 cells). Spacer cells (advance
    /// width 0) are filtered out by the caller before reaching here. The
    /// cell's resolved foreground becomes the area's [`Color`]; concealed
    /// cells are filtered out by the caller. An empty `cells` clears the
    /// frame to the background. A frame without a texture (an errored
    /// acquire) is left untouched.
    ///
    /// N13c: each cell's shaped buffer carries its underline and
    /// strikethrough decorations in `LayoutRun.decorations`; those spans are
    /// resolved by [`decoration_style`] and drawn as fill lines in the same
    /// pass, after the text.
    ///
    /// Geometry is physical pixels throughout: `fonts.metrics` already
    /// includes the display scale factor and the zoom level, so the grid,
    /// fills, glyph bounds and mouse mapping agree at every scale.
    ///
    /// `search_cells` (N20b) are the viewport cells of the current
    /// search match's span, drawn as fill rectangles under the text.
    #[allow(clippy::too_many_arguments)] // fixed render wiring: frame + fonts + cells + background + fills + highlights
    pub fn draw_frame(
        &mut self,
        frame: &mut Frame,
        fonts: &mut glyphs::Fonts,
        cells: &[DrawCell],
        background: [u8; 4],
        fills: &[CellFill],
        rects: &[super::scene::PixelRect],
        selection_cells: &[(u32, u32)],
        search_cells: &[(u32, u32)],
        images: &[super::images::ImageDraw],
    ) -> Result<(), String> {
        let Some(view) = frame.view.as_ref() else {
            return Ok(());
        };

        // Everything is laid out in physical pixels with the measured cell
        // metrics (font size already includes the scale factor and zoom).
        let cell_w = fonts.metrics.cell_w;
        let cw = cell_w as f32;
        let cell_h = fonts.metrics.cell_h as f32;
        let metrics = Metrics::new(fonts.metrics.font_px, cell_h);

        // Shape every cell first (N27b): glyphon's `TextArea` borrows its
        // `Buffer`, so every cell is prepared before any area borrows the
        // cache. The retained glyph cache reuses an already-shaped buffer for
        // an unchanged cell (same text, bold/italic/underline/strike and
        // zoom-scaled size) instead of re-running cosmic-text's shaping, and
        // the areas borrow its buffers rather than cloning them (audit R12).
        self.glyph_cache.begin_frame(cells.len());
        let slots: Vec<usize> = cells
            .iter()
            .map(|cell| {
                self.glyph_cache.prepare(
                    &mut fonts.fs,
                    metrics,
                    &cell.text,
                    cell.style.bold,
                    cell.style.italic,
                    cell.style.underline,
                    cell.style.strikethrough,
                    metrics.font_size,
                )
            })
            .collect();
        let glyph_cache = &self.glyph_cache;

        let mut areas: Vec<TextArea> = Vec::with_capacity(cells.len());
        for (cell, buffer) in cells
            .iter()
            .zip(slots.iter().map(|&s| glyph_cache.buffer(s)))
        {
            // Advance by the cell's width: a wide char spans 2 cells,
            // everything else 1 (spacers never reach here).
            let span = cell.width.max(1) as i32;
            areas.push(TextArea {
                buffer,
                left: cell.col as f32 * cw,
                top: cell.row as f32 * cell_h,
                scale: 1.0,
                bounds: TextBounds {
                    left: cell.col as i32 * cell_w as i32,
                    top: cell.row as i32 * cell_h as i32,
                    right: (cell.col as i32 + span) * cell_w as i32,
                    bottom: (cell.row + 1) as i32 * cell_h as i32,
                },
                default_color: Color::rgba(
                    cell.style.fg[0],
                    cell.style.fg[1],
                    cell.style.fg[2],
                    cell.style.fg[3],
                ),
                custom_glyphs: &[],
            });
        }

        // Kitty graphics (ADR-014): textures uploaded and quads laid out
        // before the pass; drawn in three bands around the text below.
        let frame_size = frame
            .texture
            .as_ref()
            .map(|t| (t.texture.size().width, t.texture.size().height))
            .unwrap_or((1, 1));
        let images = self
            .image_renderer
            .prepare(&self.device, &self.queue, images, frame_size);
        let mut pass = frame.encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("nora pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(clear_color(
                        background,
                        self.format.is_some_and(|f| f.is_srgb()),
                    )),
                    store: StoreOp::default(),
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.image_renderer
            .draw(&mut pass, &images, Band::UnderBackgrounds, frame_size);
        // N14b: selection highlight — fill rectangles under the selected
        // cells, drawn before the glyphs so text composites on top.
        // Cell backgrounds and the block cursor: solid rectangles drawn
        // first, so selection, search and glyphs composite on top.
        let mut fill_vertices: Vec<DecoVertex> = Vec::new();
        if let Some(texture) = frame.texture.as_ref() {
            let size = texture.texture.size();
            let frame_w = size.width as f32;
            let frame_h = size.height as f32;
            for fill in fills {
                let x0 = fill.col as f32 * cw;
                let y0 = fill.row as f32 * cell_h;
                let line = DecoLine {
                    x0,
                    x1: x0 + cw * fill.span.max(1) as f32,
                    top: y0,
                    bottom: y0 + cell_h,
                    y: y0,
                    thickness: cell_h,
                    color: fill.color,
                };
                push_deco_rect(&mut fill_vertices, &line, frame_w, frame_h);
            }
        }
        if !fill_vertices.is_empty() {
            let mut bytes = Vec::with_capacity(fill_vertices.len() * 32);
            for v in &fill_vertices {
                for f in v.pos.iter().chain(v._pad.iter()).chain(v.color.iter()) {
                    bytes.extend_from_slice(&f.to_bits().to_ne_bytes());
                }
            }
            let buffer = self.device.create_buffer(&BufferDescriptor {
                label: Some("nora cell fill vertices"),
                size: bytes.len() as u64,
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buffer, 0, &bytes);
            pass.set_pipeline(&self.deco_pipeline);
            pass.set_vertex_buffer(0, buffer.slice(0..));
            pass.draw(0..fill_vertices.len() as u32, 0..1);
        }
        let mut selection: Vec<DecoVertex> = Vec::new();
        if let Some(texture) = frame.texture.as_ref() {
            let size = texture.texture.size();
            let frame_w = size.width as f32;
            let frame_h = size.height as f32;
            for &(row, col) in selection_cells {
                let x0 = col as f32 * cw;
                let y0 = row as f32 * cell_h;
                let line = DecoLine {
                    x0,
                    x1: x0 + cw,
                    top: y0,
                    bottom: y0 + cell_h,
                    y: y0,
                    thickness: cell_h,
                    color: self.selection_bg,
                };
                push_deco_rect(&mut selection, &line, frame_w, frame_h);
            }
        }
        if !selection.is_empty() {
            let mut bytes = Vec::with_capacity(selection.len() * 32);
            for v in &selection {
                for f in v.pos.iter().chain(v._pad.iter()).chain(v.color.iter()) {
                    bytes.extend_from_slice(&f.to_bits().to_ne_bytes());
                }
            }
            let buffer = self.device.create_buffer(&BufferDescriptor {
                label: Some("nora selection vertices"),
                size: bytes.len() as u64,
                // Written with `Queue::write_buffer`, which requires COPY_DST.
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buffer, 0, &bytes);
            pass.set_pipeline(&self.deco_pipeline);
            pass.set_vertex_buffer(0, buffer.slice(0..));
            pass.draw(0..selection.len() as u32, 0..1);
        }
        // N20b: search highlight — fill rectangles under the current
        // match's span, drawn after the selection fill (the match wins)
        // and before the glyphs, so the text composites on top.
        let mut search: Vec<DecoVertex> = Vec::new();
        if let Some(texture) = frame.texture.as_ref() {
            let size = texture.texture.size();
            let frame_w = size.width as f32;
            let frame_h = size.height as f32;
            for &(row, col) in search_cells {
                let x0 = col as f32 * cw;
                let y0 = row as f32 * cell_h;
                let line = DecoLine {
                    x0,
                    x1: x0 + cw,
                    top: y0,
                    bottom: y0 + cell_h,
                    y: y0,
                    thickness: cell_h,
                    color: SEARCH_BG,
                };
                push_deco_rect(&mut search, &line, frame_w, frame_h);
            }
        }
        if !search.is_empty() {
            let mut bytes = Vec::with_capacity(search.len() * 32);
            for v in &search {
                for f in v.pos.iter().chain(v._pad.iter()).chain(v.color.iter()) {
                    bytes.extend_from_slice(&f.to_bits().to_ne_bytes());
                }
            }
            let buffer = self.device.create_buffer(&BufferDescriptor {
                label: Some("nora search vertices"),
                size: bytes.len() as u64,
                // Written with `Queue::write_buffer`, which requires COPY_DST.
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buffer, 0, &bytes);
            pass.set_pipeline(&self.deco_pipeline);
            pass.set_vertex_buffer(0, buffer.slice(0..));
            pass.draw(0..search.len() as u32, 0..1);
        }
        self.image_renderer
            .draw(&mut pass, &images, Band::UnderText, frame_size);
        self.renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut fonts.fs,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash,
            )
            .map_err(|err| format!("glyph prepare failed: {err}"))?;
        self.renderer
            .render(&self.atlas, &self.viewport, &mut pass)
            .map_err(|err| format!("glyph render failed: {err}"))?;

        // N13c: decoration lines (underline/strikethrough) from each cell's
        // shaped buffer, drawn as fill rectangles in the same pass.
        let mut deco: Vec<DecoVertex> = Vec::new();
        if let Some(texture) = frame.texture.as_ref() {
            let size = texture.texture.size();
            let frame_w = size.width as f32;
            let frame_h = size.height as f32;
            for (cell, &slot) in cells.iter().zip(slots.iter()) {
                let Some(run) = glyph_cache.buffer(slot).layout_runs().next() else {
                    continue;
                };
                for span in run.decorations {
                    let ds = decoration_style(span, cell.style.fg);
                    // The run's baseline relative to the area's top-left
                    // (glyphon places glyphs at `area.top + line_y + y`);
                    // decoration offsets are EM units from the baseline,
                    // positive up.
                    let baseline = cell.row as f32 * cell_h + run.line_y;
                    let font_size = span.font_size;
                    let span_w = cell.width.max(1) as i32;
                    let x0 = cell.col as f32 * cw;
                    let x1 = (cell.col as i32 + span_w) as f32 * cw;
                    let top = cell.row as f32 * cell_h;
                    let bottom = (cell.row + 1) as f32 * cell_h;
                    let band = |y: f32, thickness: f32, color: [u8; 4]| DecoLine {
                        x0,
                        x1,
                        top,
                        bottom,
                        y,
                        thickness,
                        color,
                    };
                    if let Some(color) = ds.underline {
                        let thickness = (span.data.underline_metrics.thickness * font_size)
                            .max(1.0)
                            .ceil();
                        let y = baseline - span.data.underline_metrics.offset * font_size;
                        push_deco_rect(&mut deco, &band(y, thickness, color), frame_w, frame_h);
                        if ds.underline_double {
                            // A second line one thickness below the first.
                            push_deco_rect(
                                &mut deco,
                                &band(y + thickness * 2.0, thickness, color),
                                frame_w,
                                frame_h,
                            );
                        }
                    }
                    if let Some(color) = ds.strikethrough {
                        let thickness = (span.data.strikethrough_metrics.thickness * font_size)
                            .max(1.0)
                            .ceil();
                        let y = baseline - span.data.strikethrough_metrics.offset * font_size;
                        push_deco_rect(&mut deco, &band(y, thickness, color), frame_w, frame_h);
                    }
                }
            }
            // Pane separators, on top of the text.
            for r in rects {
                let line = DecoLine {
                    x0: r.x,
                    x1: r.x + r.w,
                    top: r.y,
                    bottom: r.y + r.h,
                    y: r.y,
                    thickness: r.h,
                    color: r.color,
                };
                push_deco_rect(&mut deco, &line, frame_w, frame_h);
            }
        }
        if !deco.is_empty() {
            let mut bytes = Vec::with_capacity(deco.len() * 32);
            for v in &deco {
                for f in v.pos.iter().chain(v._pad.iter()).chain(v.color.iter()) {
                    bytes.extend_from_slice(&f.to_bits().to_ne_bytes());
                }
            }
            let buffer = self.device.create_buffer(&BufferDescriptor {
                label: Some("nora decoration vertices"),
                size: bytes.len() as u64,
                // Written with `Queue::write_buffer`, which requires COPY_DST.
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buffer, 0, &bytes);
            pass.set_pipeline(&self.deco_pipeline);
            pass.set_vertex_buffer(0, buffer.slice(0..));
            pass.draw(0..deco.len() as u32, 0..1);
        }

        self.image_renderer
            .draw(&mut pass, &images, Band::OverText, frame_size);

        // Dropping `pass` ends it (pinned wgpu 30 `RenderPass` has no
        // explicit finish; the N02 offscreen test follows the same pattern).
        Ok(())
    }

    /// Set the selection highlight color.
    pub fn set_selection_color(&mut self, color: [u8; 4]) {
        self.selection_bg = color;
    }

    /// Drop every shaped cell and every rasterized glyph: required when the
    /// font system is replaced. Each new fontdb database hands out the same
    /// font ids again (the monospace face is always the first), so the
    /// glyphon atlas and the swash image cache, both keyed by
    /// `CacheKey { font_id, glyph_id, .. }`, would otherwise keep drawing
    /// the old font's bitmaps. The atlas is recreated with the same format
    /// (and so the same color mode), and the renderer bound to it.
    pub fn clear_glyph_cache(&mut self) {
        self.glyph_cache = GlyphCache::new();
        if let Some(format) = self.format {
            let (atlas, renderer) = text_pipeline(&self.device, &self.queue, &self.cache, format);
            self.atlas = atlas;
            self.renderer = renderer;
        }
        self.swash = SwashCache::new();
    }

    /// Finish, submit and present an acquired [`Frame`] (pinned wgpu 30
    /// order: the command buffer is submitted before `queue.present`).
    ///
    /// On a frame without a texture the (possibly empty) encoder is still
    /// finished and submitted, and dropping the frame releases the failed
    /// acquire before the caller reconfigures the surface.
    pub fn present(&mut self, frame: Frame) {
        let cmd = frame.encoder.finish();
        self.queue.submit([cmd]);
        if let Some(st) = frame.texture {
            self.queue.present(st);
        }
        // Release atlas space for glyphs this frame did not use; without
        // it the atlas only grows until `prepare` fails with AtlasFull and
        // text stops rendering (audit R8).
        self.atlas.trim();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clusters up to 15 bytes stay inline; longer ones use the heap; both
    /// read back exactly.
    #[test]
    fn cell_text_keeps_clusters_exact() {
        let plain = CellText::from('a');
        assert!(matches!(plain, CellText::Inline { .. }));
        assert_eq!(plain.as_str(), "a");
        let marked = CellText::cluster('e', &['\u{301}', '\u{323}']);
        assert!(matches!(marked, CellText::Inline { .. }));
        assert_eq!(&*marked, "e\u{301}\u{323}");
        let wide = CellText::from("日");
        assert_eq!(wide.as_str(), "日");
        let marks = ['\u{301}'; 8];
        let long = CellText::cluster('x', &marks);
        assert!(matches!(long, CellText::Heap(_)));
        assert_eq!(long.as_str().chars().count(), 9);
        assert_eq!(CellText::from("").as_str(), "");
    }

    /// One case per pinned wgpu 30 acquire outcome, headless: no surface, no
    /// adapter. (The texture-carrying `Success`/`Suboptimal` wgpu variants
    /// are not constructible without a live surface, so the mapping is
    /// verified through [`AcquireStatus`], which they classify to 1:1.)
    #[test]
    fn surface_status_maps_every_acquire_variant() {
        assert_eq!(
            surface_status(AcquireStatus::Success),
            SurfaceStatus::Success
        );
        assert_eq!(
            surface_status(AcquireStatus::Suboptimal),
            SurfaceStatus::Suboptimal
        );
        assert_eq!(
            surface_status(AcquireStatus::Timeout),
            SurfaceStatus::Timeout
        );
        assert_eq!(
            surface_status(AcquireStatus::Occluded),
            SurfaceStatus::Occluded
        );
        assert_eq!(
            surface_status(AcquireStatus::Outdated),
            SurfaceStatus::Outdated
        );
        assert_eq!(surface_status(AcquireStatus::Lost), SurfaceStatus::Lost);
        assert_eq!(
            surface_status(AcquireStatus::Validation),
            SurfaceStatus::Validation("surface texture validation failed".to_owned())
        );
    }

    /// The payload-less pinned wgpu 30 variants classify 1:1 (the
    /// texture-carrying ones need a live surface and are exercised by the
    /// smoke cycle).
    #[test]
    fn classify_maps_the_payload_less_wgpu_variants() {
        assert_eq!(
            classify(&CurrentSurfaceTexture::Timeout),
            AcquireStatus::Timeout
        );
        assert_eq!(
            classify(&CurrentSurfaceTexture::Occluded),
            AcquireStatus::Occluded
        );
        assert_eq!(
            classify(&CurrentSurfaceTexture::Outdated),
            AcquireStatus::Outdated
        );
        assert_eq!(classify(&CurrentSurfaceTexture::Lost), AcquireStatus::Lost);
        assert_eq!(
            classify(&CurrentSurfaceTexture::Validation),
            AcquireStatus::Validation
        );
    }
}

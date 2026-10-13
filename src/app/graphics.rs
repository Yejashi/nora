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
use glyphon::{
    Buffer, Cache, Color, Resolution, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer,
    Viewport,
};
use wgpu::{
    Adapter, CommandEncoder, CommandEncoderDescriptor, CurrentSurfaceTexture, DeviceDescriptor,
    DeviceLostReason, Instance, InstanceDescriptor, LoadOp, MultisampleState, Operations,
    RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions, StoreOp, Surface,
    SurfaceTexture, TextureAspect, TextureFormat, TextureView, TextureViewDescriptor,
};
use winit::{dpi::PhysicalSize, event_loop::ActiveEventLoop, window::Window};

use nora::render::state::SurfaceStatus;

/// The background a live frame is cleared to (N02b pattern).
const BG: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

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
    adapter: Adapter,
    device: wgpu::Device,
    /// Submitted by the first render pass (N02 text adapter).
    #[allow(dead_code)]
    pub queue: wgpu::Queue,
    surface: Surface<'static>,
    /// Tracked so a zero-sized `set_size` never reconfigures the swapchain.
    configured_size: Option<PhysicalSize<u32>>,
    format: Option<wgpu::TextureFormat>,
    /// Kept alive for the atlas and viewport (N02 pattern).
    #[allow(dead_code)]
    cache: Cache,
    atlas: TextAtlas,
    swash: SwashCache,
    viewport: Viewport,
    renderer: TextRenderer,
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
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let swash = SwashCache::new();
        let viewport = Viewport::new(&device, &cache);
        let renderer = TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        Ok(Self {
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
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        if self.configured_size == Some(size) {
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
    /// (N08b2b).
    ///
    /// `cells` are the visible cells as `(row, col, text)`: each is shaped
    /// with `glyphs::Fonts::shape_cell` and placed with a cell-allocated
    /// `TextBounds`, exactly as `tests/text_render.rs` does. An empty `cells`
    /// clears the frame to the background. A frame without a texture (an
    /// errored acquire) is left untouched.
    pub fn draw_frame(
        &mut self,
        frame: &mut Frame,
        fonts: &mut glyphs::Fonts,
        cells: &[(u32, u32, &str)],
    ) -> Result<(), String> {
        let Some(view) = frame.view.as_ref() else {
            return Ok(());
        };

        let mut buffers: Vec<Buffer> = Vec::with_capacity(cells.len());
        for (_row, _col, text) in cells {
            buffers.push(fonts.shape_cell(text));
        }
        let cell_w = fonts.cell_w;
        let cw = cell_w as f32;

        let mut areas: Vec<TextArea> = Vec::with_capacity(buffers.len());
        for (buffer, (row, col, _)) in buffers.iter().zip(cells.iter()) {
            areas.push(TextArea {
                buffer,
                left: *col as f32 * cw,
                top: *row as f32 * glyphs::CELL_H,
                scale: 1.0,
                bounds: TextBounds {
                    left: *col as i32 * cell_w as i32,
                    top: *row as i32 * glyphs::CELL_H as i32,
                    right: (*col + 1) as i32 * cell_w as i32,
                    bottom: (*row + 1) as i32 * glyphs::CELL_H as i32,
                },
                default_color: Color::rgb(0xFF, 0xFF, 0xFF),
                custom_glyphs: &[],
            });
        }

        let mut pass = frame.encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("nora pass"),
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
        // Dropping `pass` ends it (pinned wgpu 30 `RenderPass` has no
        // explicit finish; the N02 offscreen test follows the same pattern).
        Ok(())
    }

    /// Finish, submit and present an acquired [`Frame`] (pinned wgpu 30
    /// order: the command buffer is submitted before `queue.present`).
    ///
    /// On a frame without a texture the (possibly empty) encoder is still
    /// finished and submitted, and dropping the frame releases the failed
    /// acquire before the caller reconfigures the surface.
    pub fn present(&self, frame: Frame) {
        let cmd = frame.encoder.finish();
        self.queue.submit([cmd]);
        if let Some(st) = frame.texture {
            self.queue.present(st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

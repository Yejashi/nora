//! wgpu surface/device setup for a nora window (N01).
//!
//! The wgpu instance is bound to the winit event loop's display handle so the
//! surface presents to whichever native compositor (Wayland or X11) owns the
//! window. Terminal state does not live here; this module owns GPU resources
//! only.

use std::sync::Arc;

use wgpu::{
    Adapter, DeviceDescriptor, Instance, InstanceDescriptor, RequestAdapterOptions, Surface,
};
use winit::{dpi::PhysicalSize, event_loop::ActiveEventLoop, window::Window};

/// GPU side of a nora window: a surface and the device presenting to it.
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
}

impl Graphics {
    /// Create the instance, surface and device for `window`.
    ///
    /// The surface is not configured until [`Self::set_size`] receives a
    /// non-zero physical size, so zero-sized surfaces never reach the swapchain.
    pub async fn new(event_loop: &ActiveEventLoop, window: &Arc<Window>) -> Result<Self, String> {
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

        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default())
            .await
            .map_err(|err| format!("failed to create wgpu device: {err}"))?;

        Ok(Self {
            adapter,
            device,
            queue,
            surface,
            configured_size: None,
            format: None,
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

    /// (Re)configure the surface for a new physical size.
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
        let config = self
            .surface
            .get_default_config(&self.adapter, size.width, size.height)
            .ok_or("adapter does not support this surface at the requested size")?;
        self.surface.configure(&self.device, &config);
        self.configured_size = Some(size);
        self.format = Some(config.format);
        Ok(())
    }
}

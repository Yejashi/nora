//! Kitty graphics on the GPU (ADR-014): one texture per image (keyed by
//! image id and upload generation), textured quads in three z bands, each
//! clipped to its pane. Textures not drawn for a while, or past the
//! budget, are dropped.

use std::collections::HashMap;

use nora::graphics_image::VisibleImage;
use wgpu::{
    BindGroup, BindGroupLayout, BlendState, Buffer, BufferDescriptor, BufferUsages,
    ColorTargetState, FragmentState, MultisampleState, PipelineCompilationOptions, PrimitiveState,
    RenderPass, RenderPipeline, Sampler, ShaderModuleDescriptor, ShaderSource, VertexAttribute,
    VertexBufferLayout, VertexFormat, VertexState,
};

/// Textures kept for images (bytes); the least recently drawn go first.
const TEXTURE_BUDGET: usize = 256 * 1024 * 1024;
/// Frames a texture survives without being drawn.
const IDLE_FRAMES: u64 = 600;

/// An image placement to draw, in physical pixels.
pub struct ImageDraw {
    pub image: VisibleImage,
    /// x, y, width, height.
    pub rect: [f32; 4],
    /// The pane's area: the image is clipped to it.
    pub clip: [f32; 4],
}

/// Where a placement is drawn relative to the text (by z-index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// z < -1 073 741 824: under cell backgrounds.
    UnderBackgrounds,
    /// Other negative z: under the text.
    UnderText,
    /// z >= 0: over the text.
    OverText,
}

impl Band {
    fn of(z: i32) -> Self {
        if z < i32::MIN / 2 {
            Band::UnderBackgrounds
        } else if z < 0 {
            Band::UnderText
        } else {
            Band::OverText
        }
    }
}

struct Cached {
    bind_group: BindGroup,
    bytes: usize,
    last_used: u64,
}

struct Draw {
    band: Band,
    key: (u32, u64),
    first_vertex: u32,
    scissor: [u32; 4],
}

/// One frame's images, uploaded and laid out.
#[derive(Default)]
pub struct PreparedImages {
    buffer: Option<Buffer>,
    draws: Vec<Draw>,
}

pub struct ImageRenderer {
    pipeline: RenderPipeline,
    layout: BindGroupLayout,
    sampler: Sampler,
    texture_format: wgpu::TextureFormat,
    textures: HashMap<(u32, u64), Cached>,
    frame: u64,
}

impl ImageRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nora image layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nora image pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("nora image shader"),
            source: ShaderSource::Wgsl(
                r#"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

struct VSOut {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs_main(@location(0) pos: vec2f, @location(1) uv: vec2f) -> VSOut {
    var out: VSOut;
    out.pos = vec4f(pos, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4f {
    return textureSample(image, image_sampler, in.uv);
}"#
                .into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("nora image pipeline"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[Some(VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        VertexAttribute {
                            shader_location: 0,
                            offset: 0,
                            format: VertexFormat::Float32x2,
                        },
                        VertexAttribute {
                            shader_location: 1,
                            offset: 8,
                            format: VertexFormat::Float32x2,
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
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::default(),
                })],
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nora image sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // Image pixels are sRGB, like the text colors: on an sRGB target the
        // texture decodes them to linear and the target encodes them back.
        let texture_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        Self {
            pipeline,
            layout,
            sampler,
            texture_format,
            textures: HashMap::new(),
            frame: 0,
        }
    }

    /// Upload new images and lay out this frame's quads.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        images: &[ImageDraw],
        frame_size: (u32, u32),
    ) -> PreparedImages {
        self.frame += 1;
        let (fw, fh) = (frame_size.0.max(1) as f32, frame_size.1.max(1) as f32);
        let mut vertices: Vec<[f32; 4]> = Vec::new();
        let mut draws = Vec::new();
        for draw in images {
            let image = &draw.image;
            let [x, y, w, h] = draw.rect;
            let clip = intersect(draw.clip, [x, y, w, h], frame_size);
            let Some(scissor) = clip else { continue };
            let key = (image.image_id, image.generation);
            if !self.textures.contains_key(&key) {
                let cached = self.upload(device, queue, image);
                self.textures.insert(key, cached);
            }
            if let Some(cached) = self.textures.get_mut(&key) {
                cached.last_used = self.frame;
            }
            let [sx, sy, sw, sh] = image.source;
            let (iw, ih) = (image.width.max(1) as f32, image.height.max(1) as f32);
            let (u0, v0) = (sx as f32 / iw, sy as f32 / ih);
            let (u1, v1) = ((sx + sw) as f32 / iw, (sy + sh) as f32 / ih);
            let ndc = |px: f32, py: f32| [px / fw * 2.0 - 1.0, 1.0 - py / fh * 2.0];
            let [l, t] = ndc(x, y);
            let [r, b] = ndc(x + w, y + h);
            let first_vertex = vertices.len() as u32;
            vertices.extend_from_slice(&[
                [l, t, u0, v0],
                [r, t, u1, v0],
                [l, b, u0, v1],
                [r, t, u1, v0],
                [r, b, u1, v1],
                [l, b, u0, v1],
            ]);
            draws.push(Draw {
                band: Band::of(image.z),
                key,
                first_vertex,
                scissor,
            });
        }
        self.evict();
        if vertices.is_empty() {
            return PreparedImages::default();
        }
        let bytes: Vec<u8> = vertices
            .iter()
            .flatten()
            .flat_map(|f| f.to_ne_bytes())
            .collect();
        let buffer = device.create_buffer(&BufferDescriptor {
            label: Some("nora image vertices"),
            size: bytes.len() as u64,
            // Written with `Queue::write_buffer`, which requires COPY_DST.
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, &bytes);
        PreparedImages {
            buffer: Some(buffer),
            draws,
        }
    }

    /// Draw the prepared images of one band into `pass`.
    pub fn draw(
        &self,
        pass: &mut RenderPass<'_>,
        prepared: &PreparedImages,
        band: Band,
        frame_size: (u32, u32),
    ) {
        let Some(buffer) = prepared.buffer.as_ref() else {
            return;
        };
        let mut bound = false;
        for draw in prepared.draws.iter().filter(|d| d.band == band) {
            let Some(cached) = self.textures.get(&draw.key) else {
                continue;
            };
            if !bound {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, buffer.slice(0..));
                bound = true;
            }
            let [x, y, w, h] = draw.scissor;
            pass.set_scissor_rect(x, y, w, h);
            pass.set_bind_group(0, &cached.bind_group, &[]);
            pass.draw(draw.first_vertex..draw.first_vertex + 6, 0..1);
        }
        if bound {
            pass.set_scissor_rect(0, 0, frame_size.0.max(1), frame_size.1.max(1));
        }
    }

    fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, image: &VisibleImage) -> Cached {
        let size = wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("nora image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.texture_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &image.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * image.width),
                rows_per_image: Some(image.height),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nora image bind group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Cached {
            bind_group,
            bytes: image.pixels.len(),
            last_used: self.frame,
        }
    }

    /// Drop textures idle for [`IDLE_FRAMES`], then the least recently
    /// drawn while over [`TEXTURE_BUDGET`] (never this frame's).
    fn evict(&mut self) {
        let frame = self.frame;
        self.textures
            .retain(|_, cached| cached.last_used + IDLE_FRAMES > frame);
        let mut total: usize = self.textures.values().map(|c| c.bytes).sum();
        while total > TEXTURE_BUDGET {
            let oldest = self
                .textures
                .iter()
                .filter(|(_, c)| c.last_used < frame)
                .min_by_key(|(_, c)| c.last_used)
                .map(|(key, _)| *key);
            let Some(key) = oldest else { break };
            if let Some(cached) = self.textures.remove(&key) {
                total -= cached.bytes;
            }
        }
    }
}

/// The scissor rectangle: `clip` ∩ `rect` ∩ the frame, in whole pixels, or
/// `None` when empty.
fn intersect(clip: [f32; 4], rect: [f32; 4], frame: (u32, u32)) -> Option<[u32; 4]> {
    let x0 = clip[0].max(rect[0]).max(0.0);
    let y0 = clip[1].max(rect[1]).max(0.0);
    let x1 = (clip[0] + clip[2])
        .min(rect[0] + rect[2])
        .min(frame.0 as f32);
    let y1 = (clip[1] + clip[3])
        .min(rect[1] + rect[3])
        .min(frame.1 as f32);
    let (x0, y0) = (x0.floor() as u32, y0.floor() as u32);
    let (x1, y1) = (x1.ceil().max(0.0) as u32, y1.ceil().max(0.0) as u32);
    (x1 > x0 && y1 > y0).then(|| [x0, y0, x1 - x0, y1 - y0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_follow_the_z_index() {
        assert_eq!(Band::of(0), Band::OverText);
        assert_eq!(Band::of(5), Band::OverText);
        assert_eq!(Band::of(-1), Band::UnderText);
        assert_eq!(Band::of(i32::MIN / 2), Band::UnderText);
        assert_eq!(Band::of(i32::MIN / 2 - 1), Band::UnderBackgrounds);
    }

    #[test]
    fn scissor_is_the_visible_part() {
        let frame = (100, 100);
        assert_eq!(
            intersect([0.0, 0.0, 50.0, 50.0], [10.0, 10.0, 80.0, 20.0], frame),
            Some([10, 10, 40, 20]),
            "clipped to the pane"
        );
        assert_eq!(
            intersect([0.0, 0.0, 50.0, 50.0], [60.0, 0.0, 10.0, 10.0], frame),
            None
        );
        assert_eq!(
            intersect([0.0, 0.0, 200.0, 200.0], [-5.0, 90.0, 20.0, 20.0], frame),
            Some([0, 90, 15, 10]),
            "and to the frame"
        );
    }
}

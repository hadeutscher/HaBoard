use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::drawables::TextureUploader;
use crate::texture::Texture;

/// Maximum number of quads that can be drawn in a single `draw_quads` call.
const MAX_QUADS: usize = 10_000;

// ---------------------------------------------------------------------------
// WGSL shader
// ---------------------------------------------------------------------------

const SHADER_SRC: &str = r#"
struct ScreenUniform {
    size: vec2<f32>,
}

@group(0) @binding(0)
var<uniform> screen: ScreenUniform;

struct VertIn {
    @location(0) position: vec2<f32>,
    @location(1) uv:       vec2<f32>,
    /// RGB tint colour + mix factor in the alpha channel.
    /// mix factor 0.0 = no tint, 1.0 = full tint colour.
    @location(2) tint:     vec4<f32>,
}

struct VertOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0)       uv:       vec2<f32>,
    @location(1)       tint:     vec4<f32>,
}

@vertex
fn vs_main(in: VertIn) -> VertOut {
    var out: VertOut;
    out.clip_pos = vec4<f32>(
        (in.position.x / screen.size.x) * 2.0 - 1.0,
        1.0 - (in.position.y / screen.size.y) * 2.0,
        0.0,
        1.0,
    );
    out.uv = in.uv;
    out.tint = in.tint;
    return out;
}

@group(1) @binding(0) var t_diffuse: texture_2d<f32>;
@group(1) @binding(1) var s_diffuse: sampler;

@fragment
fn fs_main(in: VertOut) -> @location(0) vec4<f32> {
    let color = textureSample(t_diffuse, s_diffuse, in.uv);
    // Mix tint into RGB only; alpha is preserved so transparent areas stay transparent.
    let rgb = mix(color.rgb, in.tint.rgb, in.tint.a);
    return vec4<f32>(rgb, color.a);
}
"#;

// ---------------------------------------------------------------------------
// Vertex layout
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
    tint: [f32; 4],
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

// ---------------------------------------------------------------------------
// Quad — the unit of rendering
// ---------------------------------------------------------------------------

/// A single textured quad submitted to [`Engine::draw_quads`].
pub(crate) struct Quad<'a> {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub texture: &'a Texture,
    /// RGB tint + mix factor. `[r, g, b, mix]` where `mix = 0.0` means no tint.
    pub tint: [f32; 4],
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The low-level GPU rendering engine.
///
/// `Engine` manages all wgpu resources for a window. It is a pure renderer:
/// given a list of [`Quad`]s it rasterises them each frame and holds no scene
/// state. Use [`Scene`](crate::Scene) to pair the engine with a managed
/// drawable collection.
pub struct Engine {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    render_pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    screen_uniform: wgpu::Buffer,
    screen_bind_group: wgpu::BindGroup,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    /// Background clear colour. Default: dark grey.
    pub clear_color: wgpu::Color,
}

impl Engine {
    /// Initialise wgpu and build all fixed GPU resources.
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();

        let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_desc.backends = wgpu::Backends::all();
        let instance = wgpu::Instance::new(instance_desc);

        let surface = instance.create_surface(Arc::clone(&window)).unwrap();

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .unwrap();

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Engine device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .unwrap();

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // ── Shader ───────────────────────────────────────────────────────────
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Sprite shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SRC.into()),
        });

        // ── Screen uniform ───────────────────────────────────────────────────
        let screen_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Screen uniform"),
            contents: bytemuck::cast_slice(&[size.width as f32, size.height as f32]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let screen_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Screen BGL"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let screen_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Screen BG"),
            layout: &screen_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_uniform.as_entire_binding(),
            }],
        });

        // ── Texture bind group layout ─────────────────────────────────────────
        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Texture BGL"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
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

        // ── Render pipeline ───────────────────────────────────────────────────
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Pipeline layout"),
            bind_group_layouts: &[
                Some(&screen_bind_group_layout),
                Some(&texture_bind_group_layout),
            ],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Sprite pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // ── Vertex buffer (pre-allocated) ─────────────────────────────────────
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Vertex buffer"),
            size: (MAX_QUADS * 4 * std::mem::size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Index buffer (static: [0,1,2, 0,2,3] repeated) ───────────────────
        let indices: Vec<u32> = (0..MAX_QUADS as u32)
            .flat_map(|i| {
                let b = i * 4;
                [b, b + 1, b + 2, b, b + 2, b + 3]
            })
            .collect();
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Index buffer"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        Self {
            window,
            surface,
            device,
            queue,
            config,
            render_pipeline,
            vertex_buffer,
            index_buffer,
            screen_uniform,
            screen_bind_group,
            texture_bind_group_layout,
            clear_color: wgpu::Color {
                r: 0.1,
                g: 0.1,
                b: 0.1,
                a: 1.0,
            },
        }
    }

    /// Return a reference to the window.
    pub fn window(&self) -> &Arc<Window> {
        &self.window
    }

    /// Handle a window resize.
    pub fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.queue.write_buffer(
            &self.screen_uniform,
            0,
            bytemuck::cast_slice(&[size.width as f32, size.height as f32]),
        );
    }

    /// Build a [`TextureUploader`] that shares this engine's device, queue, and
    /// texture bind-group layout.  Used by [`Drawables`](crate::Drawables) to
    /// upload images independently of the engine.
    pub(crate) fn make_uploader(&self) -> TextureUploader {
        TextureUploader {
            device: self.device.clone(),
            queue: self.queue.clone(),
            layout: self.texture_bind_group_layout.clone(),
        }
    }

    /// Render a list of textured quads to the surface.
    ///
    /// Pass 1 (user drawables, Z-sorted) and pass 2 (overlays) are both
    /// submitted as a single flat `quads` slice by the caller ([`Scene`](crate::Scene)).
    pub(crate) fn draw_quads(&mut self, quads: &[Quad<'_>]) {
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex)
            | wgpu::CurrentSurfaceTexture::Suboptimal(tex) => tex,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            other => {
                eprintln!("Surface unavailable: {other:?}");
                return;
            }
        };

        let count = quads.len().min(MAX_QUADS);

        if count > 0 {
            let mut verts: Vec<Vertex> = Vec::with_capacity(count * 4);
            for q in &quads[..count] {
                let (x0, y0) = (q.x, q.y);
                let (x1, y1) = (q.x + q.width, q.y + q.height);
                verts.extend_from_slice(&[
                    Vertex {
                        position: [x0, y0],
                        uv: [0.0, 0.0],
                        tint: q.tint,
                    },
                    Vertex {
                        position: [x0, y1],
                        uv: [0.0, 1.0],
                        tint: q.tint,
                    },
                    Vertex {
                        position: [x1, y1],
                        uv: [1.0, 1.0],
                        tint: q.tint,
                    },
                    Vertex {
                        position: [x1, y0],
                        uv: [1.0, 0.0],
                        tint: q.tint,
                    },
                ]);
            }
            self.queue
                .write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&verts));
        }

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Frame encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Sprite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });

            pass.set_pipeline(&self.render_pipeline);
            pass.set_bind_group(0, &self.screen_bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);

            for (i, quad) in quads[..count].iter().enumerate() {
                pass.set_bind_group(1, &quad.texture.bind_group, &[]);
                pass.draw_indexed(0..6, (i * 4) as i32, 0..1);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
    }
}

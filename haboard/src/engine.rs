use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::drawable::Drawable;
use crate::texture::Texture;

/// Maximum number of drawables that can be drawn in a single `render_drawables` call.
const MAX_DRAWABLES: usize = 10_000;

// ---------------------------------------------------------------------------
// WGSL shader
// ---------------------------------------------------------------------------
//
// Bind group 0: screen-size uniform (vertex stage)
// Bind group 1: texture + sampler   (fragment stage)
//
// Vertices carry pixel-space positions; the vertex shader converts them to
// normalised device coordinates (NDC) using the screen size uniform so that
// (0, 0) maps to the top-left corner of the window.

const SHADER_SRC: &str = r#"
struct ScreenUniform {
    size: vec2<f32>,
}

@group(0) @binding(0)
var<uniform> screen: ScreenUniform;

struct VertIn {
    @location(0) position: vec2<f32>,
    @location(1) uv:       vec2<f32>,
}

struct VertOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0)       uv:       vec2<f32>,
}

@vertex
fn vs_main(in: VertIn) -> VertOut {
    var out: VertOut;
    // Convert pixel coordinates to [-1, 1] NDC.
    // x: 0 → -1,  width → +1
    // y: 0 → +1,  height → -1  (window top = NDC top)
    out.clip_pos = vec4<f32>(
        (in.position.x / screen.size.x) * 2.0 - 1.0,
        1.0 - (in.position.y / screen.size.y) * 2.0,
        0.0,
        1.0,
    );
    out.uv = in.uv;
    return out;
}

@group(1) @binding(0) var t_diffuse: texture_2d<f32>;
@group(1) @binding(1) var s_diffuse: sampler;

@fragment
fn fs_main(in: VertOut) -> @location(0) vec4<f32> {
    return textureSample(t_diffuse, s_diffuse, in.uv);
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
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The low-level rendering engine.
///
/// `Engine` manages all wgpu resources for a window.  It is a pure renderer:
/// it draws whatever [`Drawable`] objects it is given each frame and holds no
/// scene state itself.  Use [`Scene`](crate::Scene) to pair the engine with a
/// managed drawable collection.
pub struct Engine {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    render_pipeline: wgpu::RenderPipeline,

    /// Pre-allocated vertex buffer (capacity: MAX_DRAWABLES * 4 vertices).
    vertex_buffer: wgpu::Buffer,
    /// Static index buffer: [0,1,2, 0,2,3].  Re-used for every quad via
    /// `draw_indexed`'s `base_vertex` parameter.
    index_buffer: wgpu::Buffer,

    /// Uniform buffer holding the current window size as `vec2<f32>`.
    screen_uniform: wgpu::Buffer,
    screen_bind_group: wgpu::BindGroup,

    /// Shared layout for every texture bind group created by this engine.
    texture_bind_group_layout: wgpu::BindGroupLayout,

    /// Background clear colour (default: dark grey).
    pub clear_color: wgpu::Color,
}

impl Engine {
    /// Initialise wgpu and build all fixed GPU resources.
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();

        // ── Instance & surface ──────────────────────────────────────────────
        let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_desc.backends = wgpu::Backends::all();
        let instance = wgpu::Instance::new(instance_desc);

        // SAFETY: the window is kept alive inside `Self`, so the surface is
        // valid for the lifetime of the engine.
        let surface = instance.create_surface(window.clone()).unwrap();

        // ── Adapter ─────────────────────────────────────────────────────────
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("No suitable GPU adapter found");

        // ── Device & queue ───────────────────────────────────────────────────
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Engine device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .expect("Failed to create device");

        // ── Surface configuration ────────────────────────────────────────────
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .expect("Surface is not supported by the adapter");
        // Prefer FIFO (vsync) for the demo.
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        // ── Screen-size uniform ──────────────────────────────────────────────
        let screen_data: [f32; 2] = [size.width as f32, size.height as f32];
        let screen_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Screen uniform"),
            contents: bytemuck::cast_slice(&screen_data),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let screen_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            layout: &screen_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_uniform.as_entire_binding(),
            }],
        });

        // ── Texture bind group layout (shared by all textures) ───────────────
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

        // ── Render pipeline ──────────────────────────────────────────────────
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Sprite shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SRC.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Pipeline layout"),
            bind_group_layouts: &[Some(&screen_bgl), Some(&texture_bind_group_layout)],
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
                    // Alpha blending so transparent images composite correctly.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None, // no back-face culling for 2-D sprites
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // ── Vertex buffer (dynamic, rewritten every frame) ───────────────────
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Vertex buffer"),
            size: (MAX_DRAWABLES * 4 * std::mem::size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Index buffer (static: one quad = 6 indices) ──────────────────────
        // draw_indexed() offsets these indices via base_vertex so a single
        // six-entry buffer serves every drawable in the batch.
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Index buffer"),
            contents: bytemuck::cast_slice(&[0u32, 1, 2, 0, 2, 3]),
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
                r: 0.08,
                g: 0.08,
                b: 0.08,
                a: 1.0,
            },
        }
    }

    // ── Public helpers ───────────────────────────────────────────────────────

    pub fn window(&self) -> &Window {
        &self.window
    }

    /// Call this whenever the window is resized.
    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);

        let data: [f32; 2] = [new_size.width as f32, new_size.height as f32];
        self.queue
            .write_buffer(&self.screen_uniform, 0, bytemuck::cast_slice(&data));
    }

    // ── Texture loading ──────────────────────────────────────────────────────

    /// Load a texture from an image file on disk (PNG, JPEG, …).
    #[allow(dead_code)]
    pub fn load_texture(&self, path: &str) -> Result<Arc<Texture>, image::ImageError> {
        Texture::from_path(
            &self.device,
            &self.queue,
            &self.texture_bind_group_layout,
            path,
        )
        .map(Arc::new)
    }

    /// Create a texture from raw RGBA pixel data.
    pub fn create_texture_from_rgba(&self, rgba: &[u8], width: u32, height: u32) -> Arc<Texture> {
        Arc::new(Texture::from_rgba_bytes(
            &self.device,
            &self.queue,
            &self.texture_bind_group_layout,
            rgba,
            width,
            height,
            None,
        ))
    }

    /// Decode and upload an in-memory image file (PNG, JPEG, …).
    #[allow(dead_code)]
    pub fn create_texture_from_image_bytes(
        &self,
        bytes: &[u8],
    ) -> Result<Arc<Texture>, image::ImageError> {
        Texture::from_image_bytes(
            &self.device,
            &self.queue,
            &self.texture_bind_group_layout,
            bytes,
            None,
        )
        .map(Arc::new)
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// Draw a list of [`Drawable`] objects and present the frame.
    ///
    /// Objects are drawn back-to-front in the order they appear in the slice,
    /// so the last element appears on top.
    pub fn render_drawables<D: Drawable>(&mut self, drawables: &[D]) {
        // Acquire the next surface texture to render into.
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

        let count = drawables.len().min(MAX_DRAWABLES);

        // Build and upload all quad vertices before opening the render pass.
        if count > 0 {
            let mut verts: Vec<Vertex> = Vec::with_capacity(count * 4);
            for d in &drawables[..count] {
                let (x0, y0) = (d.x(), d.y());
                let (x1, y1) = (d.x() + d.width(), d.y() + d.height());
                // CCW winding in NDC space (shader flips screen-Y to NDC-Y):
                // TL → BL → BR → TR  with indices [0,1,2, 0,2,3]
                verts.extend_from_slice(&[
                    Vertex {
                        position: [x0, y0],
                        uv: [0.0, 0.0],
                    }, // 0 top-left
                    Vertex {
                        position: [x0, y1],
                        uv: [0.0, 1.0],
                    }, // 1 bottom-left
                    Vertex {
                        position: [x1, y1],
                        uv: [1.0, 1.0],
                    }, // 2 bottom-right
                    Vertex {
                        position: [x1, y0],
                        uv: [1.0, 0.0],
                    }, // 3 top-right
                ]);
            }
            self.queue
                .write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&verts));
        }

        // Encode GPU commands.
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

            // One draw call per drawable.  The static index buffer [0,1,2,0,2,3]
            // is reused for every quad; base_vertex shifts it to the right quad
            // in the vertex buffer.
            for (i, drawable) in drawables[..count].iter().enumerate() {
                pass.set_bind_group(1, &drawable.texture().bind_group, &[]);
                pass.draw_indexed(0..6, (i * 4) as i32, 0..1);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
    }
}

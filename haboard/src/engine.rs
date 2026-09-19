use wgpu::util::DeviceExt;

use crate::{drawables::TextureUploader, texture::Texture};

/// Maximum number of quads that can be drawn in a single `draw_quads` call.
const MAX_QUADS: usize = 10_000;

// ---------------------------------------------------------------------------
// WGSL shader
// ---------------------------------------------------------------------------

const SHADER_SRC: &str = r#"
struct ScreenUniform {
    size: vec2<f32>,
    // Pad to 16 bytes. Backends without
    // `DownlevelFlags::BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED` — WebGL2 among
    // them — reject a uniform binding whose type is not a multiple of 16.
    _pad: vec2<f32>,
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
// Errors
// ---------------------------------------------------------------------------

/// Why [`Engine::new`] or [`Engine::recreate_surface`] failed.
///
/// The three cases are kept apart because they call for different messages: no
/// adapter at all means the platform offers no usable GPU backend and the user
/// should be told so, whereas a refused device request means an adapter existed
/// but would not grant the limits haboard asked for — which is a haboard bug
/// worth reporting rather than anything the user can act on.
#[derive(Debug)]
#[non_exhaustive]
pub enum EngineError {
    /// The surface could not be created for the given target.
    CreateSurface(wgpu::CreateSurfaceError),
    /// No GPU adapter was available for the surface.
    NoAdapter(wgpu::RequestAdapterError),
    /// An adapter was found but refused to produce a device.
    RequestDevice(wgpu::RequestDeviceError),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateSurface(_) => f.write_str("could not create a GPU surface for the target"),
            Self::NoAdapter(_) => f.write_str("no GPU adapter is available"),
            Self::RequestDevice(_) => f.write_str("the GPU adapter refused to provide a device"),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateSurface(e) => Some(e),
            Self::NoAdapter(e) => Some(e),
            Self::RequestDevice(e) => Some(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The low-level GPU rendering engine.
///
/// `Engine` manages all wgpu resources for one surface. It is a pure renderer:
/// given a list of [`Quad`]s it rasterises them each frame and holds no scene
/// state. Use [`Scene`](crate::Scene) to pair the engine with a managed
/// drawable collection.
///
/// It knows nothing about windowing. The surface target is whatever wgpu
/// accepts — an `Arc<winit::window::Window>` on desktop, an
/// [`HtmlCanvasElement`](web_sys::HtmlCanvasElement) on the web via
/// [`from_canvas`](Engine::from_canvas) — so a host that already owns its event
/// loop can drive the engine directly.
///
/// # Coordinates
/// All sizes and positions are **physical pixels**. The engine applies no scale
/// factor of its own; a host working in logical units converts before calling
/// in.
pub struct Engine {
    /// Kept so the surface can be recreated after the platform takes it away
    /// (an Android suspend/resume cycle, or a host remounting its canvas).
    instance: wgpu::Instance,
    /// `None` while the platform has taken the surface away (Android suspend).
    surface: Option<wgpu::Surface<'static>>,
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

/// The screen-size uniform payload, padded to the 16-byte multiple that
/// backends without `DownlevelFlags::BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED`
/// (notably WebGL2) require of a uniform binding. The shader's
/// `ScreenUniform` carries matching padding; keep the two in step.
fn screen_uniform_data(width: f32, height: f32) -> [f32; 4] {
    [width, height, 0.0, 0.0]
}

impl Engine {
    /// Initialise wgpu for `target` and build all fixed GPU resources.
    ///
    /// `size` is the surface size in **physical pixels**; it is not read back
    /// from the target, because the only thing a target can report is the value
    /// the host already set. On the web in particular, a `<canvas>` that has
    /// never had its backing store sized reports the HTML default of 300×150,
    /// so inferring a size would quietly produce a mis-sized surface instead of
    /// an error.
    ///
    /// This is `async` because requesting an adapter and a device are futures
    /// on the web. That is inherent to WebGPU rather than to haboard, and it
    /// means an embedder always has a brief "not ready yet" state to hold —
    /// typically an `Option<Scene>` filled in by an async block.
    ///
    /// Any target wgpu accepts works. On desktop that is an
    /// `Arc<winit::window::Window>`; on the web use
    /// [`from_canvas`](Engine::from_canvas).
    ///
    /// ```text
    /// let size = window.inner_size();
    /// let engine = Engine::new(window, (size.width, size.height)).await?;
    /// ```
    pub async fn new(
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        size: (u32, u32),
    ) -> Result<Self, EngineError> {
        let (width, height) = (size.0.max(1), size.1.max(1));

        let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_desc.backends = wgpu::Backends::all();
        let instance = wgpu::Instance::new(instance_desc);

        let surface = instance
            .create_surface(target)
            .map_err(EngineError::CreateSurface)?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(EngineError::NoAdapter)?;

        // WebGL2 has no compute stage at all, so `Limits::default()` — which
        // demands a non-zero `max_compute_workgroups_per_dimension` — is
        // refused outright on the GL backend, taking down the WebGL fallback
        // on any browser without WebGPU. Ask for the downlevel baseline
        // there, but raise the texture/buffer dimensions back to whatever the
        // adapter actually reports, so a capable GPU isn't pinned to the
        // 2048px texture floor that baseline would otherwise impose.
        //
        // The pipeline below is a plain vertex/fragment sprite shader with one
        // uniform and one sampled texture, so it fits inside the downlevel
        // limits without any change to the renderer.
        let required_limits = if adapter.get_info().backend == wgpu::Backend::Gl {
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits())
        } else {
            wgpu::Limits::default()
        };

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Engine device"),
                required_features: wgpu::Features::empty(),
                required_limits,
                ..Default::default()
            })
            .await
            .map_err(EngineError::RequestDevice)?;

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
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
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
            contents: bytemuck::cast_slice(&screen_uniform_data(width as f32, height as f32)),
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
                buffers: &[Some(Vertex::layout())],
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

        // ── Index buffer ──────────────────────────────────────────────────────
        // Static, and pre-baked with each quad's absolute vertex base, so a
        // draw can select quad `i` by index range alone and never needs a
        // non-zero `base_vertex` (which WebGL2 cannot do).
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

        Ok(Self {
            instance,
            surface: Some(surface),
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
        })
    }

    /// Initialise wgpu against an HTML canvas.
    ///
    /// Equivalent to passing [`wgpu::SurfaceTarget::Canvas`] to
    /// [`new`](Engine::new), but without requiring the caller to depend on wgpu
    /// just to name that type — there is no implicit conversion from
    /// `HtmlCanvasElement`, because wgpu's blanket conversion covers only
    /// window-handle types.
    ///
    /// `size` is the canvas's **backing store** size in physical pixels (its
    /// `width`/`height` attributes), which for a crisp result on a
    /// high-DPI display is its CSS size multiplied by `devicePixelRatio`.
    #[cfg(target_arch = "wasm32")]
    pub async fn from_canvas(
        canvas: web_sys::HtmlCanvasElement,
        size: (u32, u32),
    ) -> Result<Self, EngineError> {
        Self::new(wgpu::SurfaceTarget::Canvas(canvas), size).await
    }

    /// Current surface size in physical pixels (`width`, `height`).
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Resize the surface. `size` is in physical pixels; a zero extent is
    /// ignored, since a surface cannot be configured with one.
    pub fn resize(&mut self, size: (u32, u32)) {
        let (width, height) = size;
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.config);
        }
        self.queue.write_buffer(
            &self.screen_uniform,
            0,
            bytemuck::cast_slice(&screen_uniform_data(width as f32, height as f32)),
        );
    }

    /// Release the current surface (e.g. on Android suspend, when the native
    /// window is destroyed). The device, queue, and pipeline survive; only the
    /// surface is dropped. Rendering becomes a no-op until
    /// [`recreate_surface`].
    ///
    /// [`recreate_surface`]: Engine::recreate_surface
    pub fn drop_surface(&mut self) {
        self.surface = None;
    }

    /// Recreate the surface against a (possibly different) target,
    /// reconfiguring it to `size` in physical pixels.
    ///
    /// The device, queue, pipeline and every uploaded texture survive, so this
    /// is far cheaper than rebuilding the engine — and it is what lets a host
    /// unmount and remount the element it renders into. On Android it pairs
    /// with [`drop_surface`](Engine::drop_surface) across a suspend/resume
    /// cycle.
    ///
    /// The engine is left without a surface if this fails, so rendering stays a
    /// no-op rather than drawing to a stale target; call again to retry.
    pub fn recreate_surface(
        &mut self,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        size: (u32, u32),
    ) -> Result<(), EngineError> {
        self.surface = None;
        let surface = self
            .instance
            .create_surface(target)
            .map_err(EngineError::CreateSurface)?;
        self.config.width = size.0.max(1);
        self.config.height = size.1.max(1);
        surface.configure(&self.device, &self.config);
        self.queue.write_buffer(
            &self.screen_uniform,
            0,
            bytemuck::cast_slice(&screen_uniform_data(
                self.config.width as f32,
                self.config.height as f32,
            )),
        );
        self.surface = Some(surface);
        Ok(())
    }

    /// Recreate the surface against an HTML canvas.
    ///
    /// The canvas counterpart to [`recreate_surface`](Engine::recreate_surface),
    /// for the same reason [`from_canvas`](Engine::from_canvas) exists: wgpu has
    /// no implicit conversion from `HtmlCanvasElement`, so without this a caller
    /// would need its own wgpu dependency to name
    /// [`wgpu::SurfaceTarget::Canvas`].
    ///
    /// This is what lets an embedded board survive its element being unmounted
    /// and remounted — the device, the pipeline and every uploaded texture are
    /// kept, so only the surface is rebuilt.
    #[cfg(target_arch = "wasm32")]
    pub fn recreate_surface_from_canvas(
        &mut self,
        canvas: web_sys::HtmlCanvasElement,
        size: (u32, u32),
    ) -> Result<(), EngineError> {
        self.recreate_surface(wgpu::SurfaceTarget::Canvas(canvas), size)
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
    /// submitted as a single flat `quads` slice by the caller
    /// ([`Scene`](crate::Scene)).
    pub(crate) fn draw_quads(&mut self, quads: &[Quad<'_>]) {
        // No surface (e.g. between an Android suspend and the next resume).
        let Some(surface) = &self.surface else {
            return;
        };
        let output = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex)
            | wgpu::CurrentSurfaceTexture::Suboptimal(tex) => tex,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                surface.configure(&self.device, &self.config);
                return;
            }
            other => {
                log::warn!("Surface unavailable: {other:?}");
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
                // Index into this quad's own six indices, which already carry
                // the absolute vertex base. Selecting the range here rather
                // than passing a non-zero `base_vertex` keeps the draw off
                // `DownlevelFlags::BASE_VERTEX`, which WebGL2 does not have —
                // there is no base-vertex draw in that API at all.
                let base = (i * 6) as u32;
                pass.draw_indexed(base..base + 6, 0, 0..1);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(output);
    }
}

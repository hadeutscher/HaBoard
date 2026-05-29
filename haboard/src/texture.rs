use std::path::Path;

/// A GPU-resident texture with its associated sampler, ready to be bound in a draw call.
#[allow(dead_code)]
pub struct Texture {
    /// The bind group that exposes this texture and its sampler to the shader.
    pub(crate) bind_group: wgpu::BindGroup,
    /// Original image width in pixels.
    pub width: u32,
    /// Original image height in pixels.
    pub height: u32,
    /// CPU-side copy of the RGBA pixel data, used for alpha hit-testing.
    pub(crate) rgba: Vec<u8>,
}

impl Texture {
    /// Upload raw RGBA bytes to a new GPU texture and create a bind group for it.
    pub fn from_rgba_bytes(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        rgba: &[u8],
        width: u32,
        height: u32,
        label: Option<&str>,
    ) -> Self {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let gpu_texture = device.create_texture(&wgpu::TextureDescriptor {
            label,
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            size,
        );

        let view = gpu_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label,
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        Self {
            bind_group,
            width,
            height,
            rgba: rgba.to_vec(),
        }
    }

    /// Decode and upload an in-memory image file (PNG, JPEG, …).
    #[allow(dead_code)]
    pub fn from_image_bytes(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        bytes: &[u8],
        label: Option<&str>,
    ) -> Result<Self, image::ImageError> {
        let img = image::load_from_memory(bytes)?.into_rgba8();
        let (width, height) = img.dimensions();
        Ok(Self::from_rgba_bytes(
            device, queue, layout, &img, width, height, label,
        ))
    }

    /// Load an image from disk and upload it to the GPU.
    #[allow(dead_code)]
    pub fn from_path(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        path: impl AsRef<Path>,
    ) -> Result<Self, image::ImageError> {
        let img = image::open(path)?.into_rgba8();
        let (width, height) = img.dimensions();
        Ok(Self::from_rgba_bytes(
            device, queue, layout, &img, width, height, None,
        ))
    }

    /// Returns the alpha byte of the texel at pixel coordinates `(x, y)`.
    /// Returns 0 for out-of-bounds coordinates.
    pub(crate) fn alpha_at(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.rgba[((y * self.width + x) * 4 + 3) as usize]
    }

    /// Returns `true` if any texel inside the rectangle `(rx, ry, rw, rh)` in
    /// texel space has alpha >= `threshold`.
    pub(crate) fn has_opaque_in_region(
        &self,
        rx: u32,
        ry: u32,
        rw: u32,
        rh: u32,
        threshold: u8,
    ) -> bool {
        let x1 = (rx + rw).min(self.width);
        let y1 = (ry + rh).min(self.height);
        for y in ry.min(self.height)..y1 {
            for x in rx.min(self.width)..x1 {
                if self.alpha_at(x, y) >= threshold {
                    return true;
                }
            }
        }
        false
    }
}

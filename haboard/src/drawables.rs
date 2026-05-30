use std::cmp::Ordering;
use std::sync::Arc;

use crate::drawable::Drawable;
use crate::image_data::ImageData;
use crate::texture::Texture;

/// Alpha values below this threshold are treated as transparent for hit-testing.
const ALPHA_THRESHOLD: u8 = 10;

// ---------------------------------------------------------------------------
// Texture uploader
// ---------------------------------------------------------------------------

/// Minimal GPU context needed to upload [`ImageData`] to the GPU.
///
/// Held by [`Drawables`] so that [`Drawables::push`] can upload textures
/// independently of the [`Engine`](crate::Engine).
pub(crate) struct TextureUploader {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) layout: wgpu::BindGroupLayout,
}

impl TextureUploader {
    /// Upload an [`ImageData`] to the GPU and return the resulting texture.
    pub(crate) fn upload(&self, image: &ImageData) -> Arc<Texture> {
        match image {
            ImageData::Rgba {
                width,
                height,
                bytes,
            } => Arc::new(Texture::from_rgba_bytes(
                &self.device,
                &self.queue,
                &self.layout,
                bytes,
                *width,
                *height,
                None,
            )),
            ImageData::Encoded(bytes) => Arc::new(
                Texture::from_image_bytes(&self.device, &self.queue, &self.layout, bytes, None)
                    .expect("Drawables::push: failed to decode ImageData::Encoded bytes"),
            ),
        }
    }

    /// Convenience: upload raw RGBA bytes directly.
    pub(crate) fn upload_rgba_bytes(&self, rgba: &[u8], width: u32, height: u32) -> Arc<Texture> {
        Arc::new(Texture::from_rgba_bytes(
            &self.device,
            &self.queue,
            &self.layout,
            rgba,
            width,
            height,
            None,
        ))
    }
}

// ---------------------------------------------------------------------------
// Internal entry
// ---------------------------------------------------------------------------

/// Internal wrapper pairing a user drawable with its cached GPU texture and
/// selection state.
pub(crate) struct DrawableEntry<T> {
    pub(crate) drawable: T,
    pub(crate) texture: Arc<Texture>,
    pub(crate) selected: bool,
}

impl<T: Drawable> DrawableEntry<T> {
    /// Alpha-aware point hit test.
    ///
    /// First delegates to `drawable.hit_test_point` (bounding-box or custom),
    /// then checks that the texel at the cursor position is opaque.
    pub(crate) fn hit_test_point(&self, px: f32, py: f32) -> bool {
        let d = &self.drawable;
        if !d.hit_test_point(px, py) {
            return false;
        }
        let tx = ((px - d.x()) / d.width() * self.texture.width as f32)
            .min(self.texture.width as f32 - 1.0) as u32;
        let ty = ((py - d.y()) / d.height() * self.texture.height as f32)
            .min(self.texture.height as f32 - 1.0) as u32;
        self.texture.alpha_at(tx, ty) >= ALPHA_THRESHOLD
    }

    /// Alpha-aware rect hit test.
    ///
    /// First delegates to `drawable.hit_test_rect`, then confirms at least one
    /// opaque texel falls inside the intersection rectangle.
    pub(crate) fn hit_test_rect(&self, rx: f32, ry: f32, rw: f32, rh: f32) -> bool {
        let d = &self.drawable;
        if !d.hit_test_rect(rx, ry, rw, rh) {
            return false;
        }
        let ox = rx.max(d.x());
        let oy = ry.max(d.y());
        let ox2 = (rx + rw).min(d.x() + d.width());
        let oy2 = (ry + rh).min(d.y() + d.height());
        let tw = self.texture.width as f32;
        let th = self.texture.height as f32;
        let tx = ((ox - d.x()) / d.width() * tw) as u32;
        let ty = ((oy - d.y()) / d.height() * th) as u32;
        let tx2 = (((ox2 - d.x()) / d.width() * tw) as u32 + 1).min(self.texture.width);
        let ty2 = (((oy2 - d.y()) / d.height() * th) as u32 + 1).min(self.texture.height);
        self.texture.has_opaque_in_region(
            tx,
            ty,
            tx2.saturating_sub(tx).max(1),
            ty2.saturating_sub(ty).max(1),
            ALPHA_THRESHOLD,
        )
    }
}

// ---------------------------------------------------------------------------
// Public collection
// ---------------------------------------------------------------------------

/// A managed collection of [`Drawable`] objects with GPU-cached textures.
///
/// Each drawable's [`image`](Drawable::image) is uploaded to the GPU exactly
/// once — when it is added via [`push`](Drawables::push) — and the resulting
/// texture is stored alongside it for the lifetime of the entry.
///
/// The [`Scene`](crate::Scene) owns a `Drawables<T>` and uses it for rendering
/// and hit-testing. Obtain an instance through [`Scene::new`](crate::Scene::new).
pub struct Drawables<T: Drawable> {
    pub(crate) entries: Vec<DrawableEntry<T>>,
    pub(crate) uploader: TextureUploader,
}

impl<T: Drawable> Drawables<T> {
    pub(crate) fn new(uploader: TextureUploader, initial: Vec<T>) -> Self {
        let mut d = Self {
            entries: Vec::with_capacity(initial.len()),
            uploader,
        };
        for drawable in initial {
            d.push(drawable);
        }
        d
    }

    /// Add a drawable to the collection.
    ///
    /// The drawable's [`image`](Drawable::image) is uploaded to the GPU
    /// immediately. The drawable starts unselected.
    pub fn push(&mut self, drawable: T) {
        let texture = self.uploader.upload(&drawable.image());
        self.entries.push(DrawableEntry {
            drawable,
            texture,
            selected: false,
        });
    }

    /// Iterate over the drawables in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|e| &e.drawable)
    }

    /// Iterate mutably over the drawables in insertion order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.entries.iter_mut().map(|e| &mut e.drawable)
    }

    /// Number of drawables in the collection.
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// The maximum Z value across all entries, or `0.0` if the collection is empty.
    pub fn max_z(&self) -> f32 {
        self.entries
            .iter()
            .map(|e| e.drawable.z())
            .fold(0.0_f32, f32::max)
    }

    /// Entry indices sorted by Z, lowest first (back-to-front render order).
    pub(crate) fn z_sorted_indices(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..self.entries.len()).collect();
        indices.sort_by(|&a, &b| {
            self.entries[a]
                .drawable
                .z()
                .partial_cmp(&self.entries[b].drawable.z())
                .unwrap_or(Ordering::Equal)
        });
        indices
    }
}

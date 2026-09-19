use std::{cmp::Ordering, sync::Arc};

use crate::{
    drawable::Drawable,
    image_data::{ImageData, ImageError},
    texture::Texture,
};

/// Alpha values below this threshold are treated as transparent for
/// hit-testing.
const ALPHA_THRESHOLD: u8 = 10;

/// Opaque magenta, used when a drawable's image cannot be decoded.
///
/// Opaque rather than transparent on purpose: hit-testing is alpha-aware, so a
/// transparent placeholder would be un-clickable and therefore un-deletable,
/// leaving a permanent ghost in the scene that the user can neither see nor
/// remove. A loud magenta square is both obvious and actionable.
const PLACEHOLDER_RGBA: [u8; 4] = [255, 0, 255, 255];

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// A stable handle to one entry in a [`Drawables`] collection.
///
/// Ids are allocated by the collection, never reused, and survive insertion and
/// removal of other entries — unlike positions, which shift. Every API that
/// refers to a particular drawable takes one of these, and so does all of the
/// scene's internal bookkeeping (drag state, undo history), which is what makes
/// it safe for a host to mutate the collection while an interaction or an undo
/// stack is outstanding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DrawableId(u64);

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
    pub(crate) fn upload(&self, image: &ImageData) -> Result<Arc<Texture>, ImageError> {
        match image {
            ImageData::Rgba {
                width,
                height,
                bytes,
            } => Ok(Arc::new(Texture::from_rgba_bytes(
                &self.device,
                &self.queue,
                &self.layout,
                bytes,
                *width,
                *height,
                None,
            ))),
            ImageData::Encoded(bytes) => {
                Texture::from_image_bytes(&self.device, &self.queue, &self.layout, bytes, None)
                    .map(Arc::new)
                    .map_err(ImageError::new)
            }
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

/// Internal wrapper pairing a user drawable with its cached GPU texture,
/// identity and selection state.
pub(crate) struct DrawableEntry<T> {
    pub(crate) id: DrawableId,
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
/// Each drawable's [`image`](Drawable::image) is uploaded to the GPU when it is
/// added, and the resulting texture is stored alongside it. Call
/// [`refresh_image`](Drawables::refresh_image) when a drawable's image has
/// changed and the GPU copy needs to catch up.
///
/// The [`Scene`](crate::Scene) owns a `Drawables<T>` and uses it for rendering
/// and hit-testing. Obtain an instance through
/// [`Scene::new`](crate::Scene::new).
///
/// # Mutation and undo history
/// The methods here are *not* recorded in the scene's undo history — they are
/// the host's own edits, not the user's. Use
/// [`Scene::add_drawable`](crate::Scene::add_drawable) and
/// [`Scene::remove_drawable`](crate::Scene::remove_drawable) for undoable
/// changes.
///
/// Removing entries here is safe while history is outstanding: history refers
/// to entries by [`DrawableId`], and steps that name an id which no longer
/// exists are skipped. The one sharp edge is [`clear`](Drawables::clear) — an
/// undo afterwards can resurrect entries the host thought it had discarded, so
/// pair it with [`Scene::clear_history`](crate::Scene::clear_history) when that
/// is not what you want.
pub struct Drawables<T: Drawable> {
    pub(crate) entries: Vec<DrawableEntry<T>>,
    pub(crate) uploader: TextureUploader,
    /// Stand-in texture for drawables whose image failed to decode. Uploaded
    /// once and shared by every such entry.
    placeholder: Arc<Texture>,
    /// Next id to hand out. Monotonic, so ids are never reused and a stale
    /// reference can never silently resolve to a different drawable.
    next_id: u64,
}

impl<T: Drawable> Drawables<T> {
    pub(crate) fn new(uploader: TextureUploader, initial: Vec<T>) -> Self {
        let placeholder = uploader.upload_rgba_bytes(&PLACEHOLDER_RGBA, 1, 1);
        let mut d = Self {
            entries: Vec::with_capacity(initial.len()),
            uploader,
            placeholder,
            next_id: 0,
        };
        for drawable in initial {
            d.push(drawable);
        }
        d
    }

    fn alloc_id(&mut self) -> DrawableId {
        let id = DrawableId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Position of `id` in [`entries`](Self::entries), if it is still present.
    pub(crate) fn index_of(&self, id: DrawableId) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    // ── Adding ───────────────────────────────────────────────────────────────

    /// Add a drawable, uploading its [`image`](Drawable::image) to the GPU.
    ///
    /// If the image cannot be decoded the drawable is still added, textured
    /// with an opaque magenta placeholder, and the failure is logged. Use
    /// [`try_push`](Drawables::try_push) to handle that case yourself.
    ///
    /// The drawable starts unselected.
    pub fn push(&mut self, drawable: T) -> DrawableId {
        let texture = match self.uploader.upload(&drawable.image()) {
            Ok(texture) => texture,
            Err(e) => {
                log::error!("{e}; using placeholder texture");
                Arc::clone(&self.placeholder)
            }
        };
        self.insert_entry(drawable, texture)
    }

    /// Add a drawable, failing if its [`image`](Drawable::image) cannot be
    /// decoded.
    ///
    /// Nothing is added when this returns `Err`.
    pub fn try_push(&mut self, drawable: T) -> Result<DrawableId, ImageError> {
        let texture = self.uploader.upload(&drawable.image())?;
        Ok(self.insert_entry(drawable, texture))
    }

    fn insert_entry(&mut self, drawable: T, texture: Arc<Texture>) -> DrawableId {
        let id = self.alloc_id();
        self.entries.push(DrawableEntry {
            id,
            drawable,
            texture,
            selected: false,
        });
        id
    }

    // ── Removing ─────────────────────────────────────────────────────────────

    /// Remove the drawable with this id, returning it.
    pub fn remove(&mut self, id: DrawableId) -> Option<T> {
        let index = self.index_of(id)?;
        Some(self.entries.remove(index).drawable)
    }

    /// Keep only the drawables for which `keep` returns `true`.
    pub fn retain(&mut self, mut keep: impl FnMut(DrawableId, &T) -> bool) {
        self.entries.retain(|e| keep(e.id, &e.drawable));
    }

    /// Remove every drawable.
    ///
    /// See the type-level note on undo history: this does not touch the scene's
    /// history, so a subsequent undo can reinsert what was cleared.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    // ── Access ───────────────────────────────────────────────────────────────

    /// Borrow the drawable with this id.
    pub fn get(&self, id: DrawableId) -> Option<&T> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| &e.drawable)
    }

    /// Mutably borrow the drawable with this id.
    ///
    /// Changing a drawable's geometry takes effect on the next render. Changing
    /// what its [`image`](Drawable::image) would return does *not* — the GPU
    /// still holds the texture uploaded earlier. Follow up with
    /// [`refresh_image`](Drawables::refresh_image).
    pub fn get_mut(&mut self, id: DrawableId) -> Option<&mut T> {
        self.entries
            .iter_mut()
            .find(|e| e.id == id)
            .map(|e| &mut e.drawable)
    }

    /// Whether this id still refers to a drawable in the collection.
    pub fn contains(&self, id: DrawableId) -> bool {
        self.index_of(id).is_some()
    }

    /// The id at a given position in insertion order.
    pub fn id_at(&self, index: usize) -> Option<DrawableId> {
        self.entries.get(index).map(|e| e.id)
    }

    /// Iterate over the drawables in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|e| &e.drawable)
    }

    /// Iterate mutably over the drawables in insertion order.
    ///
    /// The same caveat as [`get_mut`](Drawables::get_mut) applies to images.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.entries.iter_mut().map(|e| &mut e.drawable)
    }

    /// Iterate over `(id, drawable)` pairs in insertion order.
    pub fn iter_with_ids(&self) -> impl Iterator<Item = (DrawableId, &T)> {
        self.entries.iter().map(|e| (e.id, &e.drawable))
    }

    /// Every id currently in the collection, in insertion order.
    pub fn ids(&self) -> impl Iterator<Item = DrawableId> + '_ {
        self.entries.iter().map(|e| e.id)
    }

    /// Number of drawables in the collection.
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// The maximum Z value across all entries, or `0.0` if the collection is
    /// empty.
    pub fn max_z(&self) -> f32 {
        self.entries
            .iter()
            .map(|e| e.drawable.z())
            .fold(0.0_f32, f32::max)
    }

    // ── Images ───────────────────────────────────────────────────────────────

    /// Re-upload a drawable's texture from its current
    /// [`image`](Drawable::image).
    ///
    /// Call this after changing something that its image is derived from — a
    /// label, a colour — since the image is otherwise read only when the
    /// drawable is added. Cheaper and less disruptive than removing and
    /// re-adding, which would allocate a new id and disturb the undo history.
    ///
    /// Returns `false` if no such drawable exists. A decode failure falls back
    /// to the placeholder texture and is logged, as in
    /// [`push`](Drawables::push).
    pub fn refresh_image(&mut self, id: DrawableId) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let texture = match self.uploader.upload(&self.entries[index].drawable.image()) {
            Ok(texture) => texture,
            Err(e) => {
                log::error!("{e}; using placeholder texture");
                Arc::clone(&self.placeholder)
            }
        };
        self.entries[index].texture = texture;
        true
    }

    /// Re-upload a drawable's texture, failing if the image cannot be decoded.
    ///
    /// `Ok(false)` means no such drawable exists. The existing texture is left
    /// untouched when this returns `Err`.
    pub fn try_refresh_image(&mut self, id: DrawableId) -> Result<bool, ImageError> {
        let Some(index) = self.index_of(id) else {
            return Ok(false);
        };
        let texture = self
            .uploader
            .upload(&self.entries[index].drawable.image())?;
        self.entries[index].texture = texture;
        Ok(true)
    }

    // ── Selection ────────────────────────────────────────────────────────────

    /// Whether this drawable is selected.
    pub fn is_selected(&self, id: DrawableId) -> bool {
        self.index_of(id)
            .is_some_and(|index| self.entries[index].selected)
    }

    /// Select or deselect a drawable. Returns `false` if no such drawable
    /// exists.
    pub fn set_selected(&mut self, id: DrawableId, selected: bool) -> bool {
        match self.index_of(id) {
            Some(index) => {
                self.entries[index].selected = selected;
                true
            }
            None => false,
        }
    }

    /// The ids of every selected drawable, in insertion order.
    pub fn selected_ids(&self) -> impl Iterator<Item = DrawableId> + '_ {
        self.entries.iter().filter(|e| e.selected).map(|e| e.id)
    }

    /// Deselect everything.
    pub fn clear_selection(&mut self) {
        for e in &mut self.entries {
            e.selected = false;
        }
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

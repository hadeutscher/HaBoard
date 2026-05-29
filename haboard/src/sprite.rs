use std::sync::Arc;

use crate::drawable::Drawable;
use crate::texture::Texture;

/// Alpha values strictly below this threshold are treated as transparent
/// for the purposes of click and rubber-band hit-testing.
const ALPHA_THRESHOLD: u8 = 10;

/// A 2D renderable object: a rectangular region of the screen filled with a texture.
///
/// `Sprite` implements [`Drawable`] with alpha-aware hit testing: transparent
/// areas of the texture are not counted as hits.
pub struct Sprite {
    /// X position in pixels measured from the left edge of the window.
    pub x: f32,
    /// Y position in pixels measured from the top edge of the window.
    pub y: f32,
    /// Width of the sprite in pixels.
    pub width: f32,
    /// Height of the sprite in pixels.
    pub height: f32,
    /// The image displayed on this sprite.
    pub texture: Arc<Texture>,
    /// Whether this sprite is currently selected.
    pub selected: bool,
    /// When `true`, the sprite cannot be dragged in [`SceneMode::Run`](crate::SceneMode::Run).
    pub locked: bool,
}

impl Sprite {
    pub fn new(x: f32, y: f32, width: f32, height: f32, texture: Arc<Texture>) -> Self {
        Self {
            x,
            y,
            width,
            height,
            texture,
            selected: false,
            locked: false,
        }
    }
}

impl Drawable for Sprite {
    fn x(&self) -> f32 {
        self.x
    }

    fn y(&self) -> f32 {
        self.y
    }

    fn width(&self) -> f32 {
        self.width
    }

    fn height(&self) -> f32 {
        self.height
    }

    fn texture(&self) -> &Arc<Texture> {
        &self.texture
    }

    fn set_position(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
    }

    fn locked(&self) -> bool {
        self.locked
    }

    /// Alpha-aware point hit test: returns `false` over transparent texels.
    fn hit_test_point(&self, px: f32, py: f32) -> bool {
        if px < self.x || px >= self.x + self.width || py < self.y || py >= self.y + self.height {
            return false;
        }
        let tx = ((px - self.x) / self.width * self.texture.width as f32)
            .min(self.texture.width as f32 - 1.0) as u32;
        let ty = ((py - self.y) / self.height * self.texture.height as f32)
            .min(self.texture.height as f32 - 1.0) as u32;
        self.texture.alpha_at(tx, ty) >= ALPHA_THRESHOLD
    }

    /// Alpha-aware rect hit test: returns `true` only if at least one opaque
    /// texel overlaps the selection rectangle.
    fn hit_test_rect(&self, rx: f32, ry: f32, rw: f32, rh: f32) -> bool {
        // Quick bounding-box rejection before scanning any pixels.
        if rx >= self.x + self.width
            || rx + rw <= self.x
            || ry >= self.y + self.height
            || ry + rh <= self.y
        {
            return false;
        }
        // Overlap region in screen space.
        let ox = rx.max(self.x);
        let oy = ry.max(self.y);
        let ox2 = (rx + rw).min(self.x + self.width);
        let oy2 = (ry + rh).min(self.y + self.height);
        // Map overlap to texel coordinates.
        let tw = self.texture.width as f32;
        let th = self.texture.height as f32;
        let tx = ((ox - self.x) / self.width * tw) as u32;
        let ty = ((oy - self.y) / self.height * th) as u32;
        let tx2 = (((ox2 - self.x) / self.width * tw) as u32 + 1).min(self.texture.width);
        let ty2 = (((oy2 - self.y) / self.height * th) as u32 + 1).min(self.texture.height);
        self.texture.has_opaque_in_region(
            tx,
            ty,
            tx2.saturating_sub(tx).max(1),
            ty2.saturating_sub(ty).max(1),
            ALPHA_THRESHOLD,
        )
    }
}

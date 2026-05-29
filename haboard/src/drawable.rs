use std::sync::Arc;

use crate::texture::Texture;

/// A 2D object that can be drawn to the screen.
///
/// Implement this trait to create custom drawable objects that can be rendered
/// by the [`Engine`] and managed inside a [`Scene`].
///
/// Selection state is **not** part of this trait — it is managed by [`Scene`]
/// so that implementors only need to describe how an object looks and where it
/// sits, not how the UI tracks interaction with it.
///
/// [`Engine`]: crate::Engine
/// [`Scene`]: crate::Scene
pub trait Drawable {
    /// X position in pixels from the left edge of the window.
    fn x(&self) -> f32;

    /// Y position in pixels from the top edge of the window.
    fn y(&self) -> f32;

    /// Width of the object in pixels.
    fn width(&self) -> f32;

    /// Height of the object in pixels.
    fn height(&self) -> f32;

    /// The texture used to draw this object.
    fn texture(&self) -> &Arc<Texture>;

    /// Move the object to a new screen position.
    fn set_position(&mut self, x: f32, y: f32);

    /// Whether this drawable is pinned in place.
    ///
    /// In [`SceneMode::Run`](crate::SceneMode::Run), locked drawables cannot
    /// be dragged.  In [`SceneMode::Edit`](crate::SceneMode::Edit) this flag
    /// is ignored and every drawable is freely movable.
    ///
    /// Returns `false` by default (unlocked).
    fn locked(&self) -> bool {
        false
    }

    /// Returns `true` if the screen point `(px, py)` hits this object.
    ///
    /// The default implementation performs a simple bounding-box check.
    /// Override it to add alpha-aware or shape-based hit testing.
    fn hit_test_point(&self, px: f32, py: f32) -> bool {
        px >= self.x()
            && px < self.x() + self.width()
            && py >= self.y()
            && py < self.y() + self.height()
    }

    /// Returns `true` if the axis-aligned rectangle `(rx, ry, rw, rh)` in
    /// screen space overlaps this object.
    ///
    /// The default implementation tests bounding-box intersection.
    fn hit_test_rect(&self, rx: f32, ry: f32, rw: f32, rh: f32) -> bool {
        !(rx >= self.x() + self.width()
            || rx + rw <= self.x()
            || ry >= self.y() + self.height()
            || ry + rh <= self.y())
    }
}

/// Blanket impl so that `Box<dyn Drawable>` (and `Box<Sprite>`, etc.) can
/// itself be used as a `Drawable`.  This lets callers choose
/// `Scene<Box<dyn Drawable>>` for heterogeneous collections while the default
/// `Scene<Sprite>` path stays allocation-free.
impl<D: Drawable + ?Sized> Drawable for Box<D> {
    fn x(&self) -> f32 {
        (**self).x()
    }
    fn y(&self) -> f32 {
        (**self).y()
    }
    fn width(&self) -> f32 {
        (**self).width()
    }
    fn height(&self) -> f32 {
        (**self).height()
    }
    fn texture(&self) -> &Arc<Texture> {
        (**self).texture()
    }
    fn set_position(&mut self, x: f32, y: f32) {
        (**self).set_position(x, y)
    }
    fn locked(&self) -> bool {
        (**self).locked()
    }
    fn hit_test_point(&self, px: f32, py: f32) -> bool {
        (**self).hit_test_point(px, py)
    }
    fn hit_test_rect(&self, rx: f32, ry: f32, rw: f32, rh: f32) -> bool {
        (**self).hit_test_rect(rx, ry, rw, rh)
    }
}

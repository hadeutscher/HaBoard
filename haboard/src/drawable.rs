use crate::image_data::ImageData;

/// A 2D object that can be drawn to the screen.
///
/// Implement this trait to integrate custom objects with [`Scene`] and
/// [`Drawables`].
///
/// # Image upload
/// [`image`](Drawable::image) is called **exactly once**, when the drawable is
/// first added via [`Drawables::push`]. The engine uploads the image to the GPU
/// at that point and retains the resulting texture for the lifetime of the
/// entry. Do not rely on subsequent calls being made.
///
/// # Selection state
/// Selection is **not** part of this trait — it is managed internally by
/// [`Drawables`] so that implementations only need to describe geometry.
///
/// [`Drawables`]: crate::Drawables
/// [`Scene`]: crate::Scene
pub trait Drawable {
    fn x(&self) -> f32;
    fn y(&self) -> f32;
    fn width(&self) -> f32;
    fn height(&self) -> f32;

    /// Z-order value. Higher values render on top. Default: `0.0`.
    fn z(&self) -> f32 {
        0.0
    }

    /// Update the Z-order value.
    ///
    /// Default implementation is a no-op. Implement this to allow the scene to
    /// bring drawables to the front on click.
    fn set_z(&mut self, z: f32) {
        let _ = z;
    }

    /// Image used to texture this drawable.
    ///
    /// Called **once** when the drawable is added to [`Drawables`]. See the
    /// trait-level documentation for details.
    fn image(&self) -> ImageData;

    /// Move the object to a new screen position.
    fn set_position(&mut self, x: f32, y: f32);

    /// Whether this drawable is pinned in
    /// [`SceneMode::Run`](crate::SceneMode::Run). Default: `false`.
    fn locked(&self) -> bool {
        false
    }

    /// Returns `true` if the screen point `(px, py)` hits this object.
    /// Default: axis-aligned bounding-box check.
    fn hit_test_point(&self, px: f32, py: f32) -> bool {
        px >= self.x()
            && px < self.x() + self.width()
            && py >= self.y()
            && py < self.y() + self.height()
    }

    /// Returns `true` if the axis-aligned rectangle `(rx, ry, rw, rh)` overlaps
    /// this object. Default: bounding-box intersection.
    fn hit_test_rect(&self, rx: f32, ry: f32, rw: f32, rh: f32) -> bool {
        !(rx >= self.x() + self.width()
            || rx + rw <= self.x()
            || ry >= self.y() + self.height()
            || ry + rh <= self.y())
    }
}

/// Blanket impl so `Box<dyn Drawable>` can itself be used as a `Drawable`,
/// enabling heterogeneous `Scene<Box<dyn Drawable>>` collections.
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
    fn z(&self) -> f32 {
        (**self).z()
    }
    fn set_z(&mut self, z: f32) {
        (**self).set_z(z)
    }
    fn image(&self) -> ImageData {
        (**self).image()
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

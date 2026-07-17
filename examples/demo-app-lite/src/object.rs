use haboard::{Drawable, ImageData, textures};

/// A draggable circular object — a custom [`Drawable`] that is not [`haboard::Sprite`].
///
/// Unlike `Sprite`, its texture is generated procedurally (an anti-aliased
/// filled circle) rather than loaded from image bytes, and it overrides
/// `hit_test_point` with a true circular hit-test instead of relying on the
/// trait's default axis-aligned bounding-box check.
pub struct Object {
    pub x: f32,
    pub y: f32,
    pub diameter: f32,
    pub z: f32,
    pub color: [u8; 3],
}

impl Object {
    pub fn new(x: f32, y: f32, diameter: f32, color: [u8; 3]) -> Self {
        Self {
            x,
            y,
            diameter,
            z: 0.0,
            color,
        }
    }
}

impl Drawable for Object {
    fn x(&self) -> f32 {
        self.x
    }

    fn y(&self) -> f32 {
        self.y
    }

    fn width(&self) -> f32 {
        self.diameter
    }

    fn height(&self) -> f32 {
        self.diameter
    }

    fn z(&self) -> f32 {
        self.z
    }

    fn set_z(&mut self, z: f32) {
        self.z = z;
    }

    fn image(&self) -> ImageData {
        textures::circle(self.diameter as u32, self.color)
    }

    fn set_position(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
    }

    fn hit_test_point(&self, px: f32, py: f32) -> bool {
        let r = self.diameter / 2.0;
        let cx = self.x + r;
        let cy = self.y + r;
        let dx = px - cx;
        let dy = py - cy;
        dx * dx + dy * dy <= r * r
    }
}

use serde::{Deserialize, Serialize};

use crate::{drawable::Drawable, image_data::ImageData};

/// A 2D renderable object: a rectangular region of the screen filled with an
/// image.
///
/// `Sprite` is the built-in [`Drawable`] implementation provided by haboard.
/// Because it contains only plain data and an [`ImageData`] (which is itself
/// serialisable), `Sprite` derives `Serialize` and `Deserialize` directly —
/// making it straightforward to persist and restore scenes.
///
/// Hit-testing is delegated to [`DrawableEntry`](crate::drawables) which has
/// access to the uploaded texture's CPU-side RGBA copy.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Sprite {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Z-order value; higher renders on top. Default: `0.0`.
    pub z: f32,
    /// Image displayed on this sprite.
    pub image: ImageData,
    /// When `true`, cannot be dragged in
    /// [`SceneMode::Run`](crate::SceneMode::Run).
    pub locked: bool,
}

impl Sprite {
    pub fn new(x: f32, y: f32, width: f32, height: f32, image: ImageData) -> Self {
        Self {
            x,
            y,
            width,
            height,
            z: 0.0,
            image,
            locked: true,
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
    fn z(&self) -> f32 {
        self.z
    }
    fn set_z(&mut self, z: f32) {
        self.z = z;
    }
    fn image(&self) -> ImageData {
        self.image.clone()
    }
    fn set_position(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
    }
    fn locked(&self) -> bool {
        self.locked
    }
}

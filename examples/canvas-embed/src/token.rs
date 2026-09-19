//! A [`Drawable`] whose image is rasterised in the browser.
//!
//! Drawing the image host-side is the common shape for an embedded board: the
//! sprite's appearance is derived from application data (here a name and a
//! colour), so it has to be re-rasterised whenever that data changes. See
//! [`Token::rename`] and its use of
//! [`Drawables::refresh_image`](haboard::Drawables::refresh_image).

use haboard::{Drawable, ImageData, web::device_pixel_ratio};
use wasm_bindgen::JsCast;

/// Side of the rasterised texture in CSS pixels, before the device pixel ratio
/// is applied.
const TEXTURE_CSS_PX: f64 = 96.0;
/// Upper bound on the rasterised texture, so a deep browser zoom cannot ask for
/// an unreasonable one.
const TEXTURE_MAX_PX: f64 = 512.0;

pub struct Token {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub z: f32,
    pub name: String,
    /// Hue in degrees, used to derive the fill colour.
    pub hue: u32,
}

impl Token {
    pub fn new(name: impl Into<String>, x: f32, y: f32, size: f32, hue: u32) -> Self {
        Self {
            x,
            y,
            size,
            z: 0.0,
            name: name.into(),
            hue,
        }
    }

    /// Change the label. The GPU texture is stale until the owner calls
    /// [`Drawables::refresh_image`](haboard::Drawables::refresh_image).
    pub fn rename(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Draw the token onto a throwaway 2D canvas and read the pixels back.
    ///
    /// The texture is sized for the *current* device pixel ratio, which browser
    /// zoom changes. That is why this has to be re-run — via
    /// [`Drawables::refresh_image`](haboard::Drawables::refresh_image) — rather
    /// than only at startup: the geometry stays correct on zoom because it is
    /// in physical pixels, but the pixels themselves would stay at the old
    /// resolution and go blurry.
    ///
    /// Returns a fully transparent image if anything in the canvas API fails,
    /// which keeps `image()` infallible; haboard would otherwise substitute its
    /// own placeholder, and either way the failure is visible rather than fatal.
    fn rasterise(&self) -> ImageData {
        let blank = || ImageData::rgba(1, 1, vec![0, 0, 0, 0]);
        let texture_px =
            (TEXTURE_CSS_PX * device_pixel_ratio()).clamp(TEXTURE_CSS_PX, TEXTURE_MAX_PX);
        let texture_px_u32 = texture_px as u32;
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return blank();
        };
        let Ok(canvas) = document.create_element("canvas") else {
            return blank();
        };
        let Ok(canvas) = canvas.dyn_into::<web_sys::HtmlCanvasElement>() else {
            return blank();
        };
        canvas.set_width(texture_px_u32);
        canvas.set_height(texture_px_u32);

        let Ok(Some(ctx)) = canvas.get_context("2d") else {
            return blank();
        };
        let Ok(ctx) = ctx.dyn_into::<web_sys::CanvasRenderingContext2d>() else {
            return blank();
        };

        // Everything below is proportional to the texture, so the token looks
        // identical at any ratio — only sharper.
        let mid = texture_px / 2.0;
        let stroke = texture_px / 24.0;
        // Inset by the stroke width so the outline is not clipped at the edge.
        let radius = mid - stroke;

        ctx.begin_path();
        let _ = ctx.arc(mid, mid, radius, 0.0, std::f64::consts::TAU);
        ctx.set_fill_style_str(&format!("hsl({} 65% 55%)", self.hue));
        ctx.fill();
        ctx.set_line_width(stroke);
        ctx.set_stroke_style_str(&format!("hsl({} 65% 30%)", self.hue));
        ctx.stroke();

        let initial: String = self
            .name
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .collect();
        ctx.set_fill_style_str("white");
        ctx.set_font(&format!(
            "bold {}px system-ui, sans-serif",
            (texture_px * 0.44).round()
        ));
        ctx.set_text_align("center");
        ctx.set_text_baseline("middle");
        let _ = ctx.fill_text(&initial, mid, mid);

        match ctx.get_image_data(0.0, 0.0, texture_px, texture_px) {
            Ok(data) => ImageData::rgba(texture_px_u32, texture_px_u32, data.data().0),
            Err(_) => blank(),
        }
    }
}

impl Drawable for Token {
    fn x(&self) -> f32 {
        self.x
    }
    fn y(&self) -> f32 {
        self.y
    }
    fn width(&self) -> f32 {
        self.size
    }
    fn height(&self) -> f32 {
        self.size
    }
    fn z(&self) -> f32 {
        self.z
    }
    fn set_z(&mut self, z: f32) {
        self.z = z;
    }
    fn set_position(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
    }
    fn image(&self) -> ImageData {
        self.rasterise()
    }
    /// Unlocked, so the token can be dragged in
    /// [`SceneMode::Run`](haboard::SceneMode::Run) too.
    fn locked(&self) -> bool {
        false
    }
    fn try_clone(&self) -> Option<Self> {
        Some(Self {
            x: self.x,
            y: self.y,
            size: self.size,
            z: self.z,
            name: self.name.clone(),
            hue: self.hue,
        })
    }
}

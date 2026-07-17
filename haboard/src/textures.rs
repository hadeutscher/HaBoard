//! Procedural image generators.
//!
//! Each function returns an [`ImageData`](crate::ImageData) ready to pass
//! directly to [`Sprite::new`](crate::Sprite::new) or
//! [`Drawables::push`](crate::Drawables::push):
//!
//! ```no_run
//! use haboard::{Sprite, textures};
//!
//! let sprite = Sprite::new(0.0, 0.0, 64.0, 64.0, textures::checkerboard(64, 64, 8));
//! ```

use crate::ImageData;

/// Grey/white checkerboard.
///
/// `cell_size` is the side length of each square in pixels.
pub fn checkerboard(width: u32, height: u32, cell_size: u32) -> ImageData {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let light = ((x / cell_size) + (y / cell_size)).is_multiple_of(2);
            let v = if light { 230u8 } else { 40u8 };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    ImageData::rgba(width, height, rgba)
}

/// Flat solid colour.
///
/// `color` is `[R, G, B, A]`.
pub fn solid(width: u32, height: u32, color: [u8; 4]) -> ImageData {
    ImageData::rgba(width, height, color.repeat((width * height) as usize))
}

/// RGB gradient: red increases left→right, green increases top→bottom, blue =
/// 120.
pub fn gradient(width: u32, height: u32) -> ImageData {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let r = (x as f32 / width as f32 * 255.0) as u8;
            let g = (y as f32 / height as f32 * 255.0) as u8;
            rgba.extend_from_slice(&[r, g, 120, 255]);
        }
    }
    ImageData::rgba(width, height, rgba)
}

/// Anti-aliased filled circle.
///
/// The buffer is `diameter × diameter` pixels. Pixels outside the circle are
/// fully transparent; the boundary has a 1-pixel smooth falloff.
/// `color` is `[R, G, B]`.
pub fn circle(diameter: u32, color: [u8; 3]) -> ImageData {
    let mut rgba = Vec::with_capacity((diameter * diameter * 4) as usize);
    let r = diameter as f32 / 2.0;
    for y in 0..diameter {
        for x in 0..diameter {
            let dx = x as f32 + 0.5 - r;
            let dy = y as f32 + 0.5 - r;
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha = if dist <= r - 1.0 {
                255
            } else if dist <= r {
                ((r - dist) * 255.0) as u8
            } else {
                0
            };
            rgba.extend_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }
    ImageData::rgba(diameter, diameter, rgba)
}

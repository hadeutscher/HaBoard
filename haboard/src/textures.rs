//! Procedural texture generators.
//!
//! Each shape is exposed as two functions:
//!
//! - A pure `*_pixels` function that returns a raw RGBA `Vec<u8>` with no GPU
//!   dependency.  These are useful for serialization, testing, or any context
//!   where an [`Engine`] is not yet available.
//! - A plain wrapper (`checkerboard`, `solid`, …) that calls `*_pixels` and
//!   immediately uploads the result to the GPU via the provided [`Engine`].
//!
//! ```no_run
//! # async fn run(engine: &haboard::Engine) {
//! use haboard::textures;
//!
//! // Pure pixel data — no engine needed.
//! let bytes = textures::circle_pixels(128, [255, 160, 20]);
//!
//! // Or build and upload in one step.
//! let badge = textures::circle(engine, 128, [255, 160, 20]);
//! # }
//! ```

use std::sync::Arc;

use crate::{engine::Engine, texture::Texture};

// ---------------------------------------------------------------------------
// Pure pixel generators
// ---------------------------------------------------------------------------

/// Grey/white checkerboard pixel data.
///
/// `cell_size` is the side length of each square in pixels.
/// Returns raw RGBA bytes, `width * height * 4` bytes total.
pub fn checkerboard_pixels(width: u32, height: u32, cell_size: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let light = ((x / cell_size) + (y / cell_size)).is_multiple_of(2);
            let v = if light { 230u8 } else { 40u8 };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    rgba
}

/// Flat solid-colour pixel data.
///
/// `color` is `[R, G, B, A]`.
/// Returns raw RGBA bytes, `width * height * 4` bytes total.
pub fn solid_pixels(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    color.repeat((width * height) as usize)
}

/// RGB gradient pixel data.
///
/// Red increases left→right, green increases top→bottom, blue is fixed at 120.
/// Returns raw RGBA bytes, `width * height * 4` bytes total.
pub fn gradient_pixels(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let r = (x as f32 / width as f32 * 255.0) as u8;
            let g = (y as f32 / height as f32 * 255.0) as u8;
            rgba.extend_from_slice(&[r, g, 120, 255]);
        }
    }
    rgba
}

/// Anti-aliased filled circle pixel data.
///
/// The buffer is `diameter × diameter` pixels.  Pixels outside the circle are
/// fully transparent; interior pixels use `color` at full opacity.  A 1-pixel
/// smooth edge is rendered at the boundary.
///
/// `color` is `[R, G, B]`.
/// Returns raw RGBA bytes, `diameter * diameter * 4` bytes total.
pub fn circle_pixels(diameter: u32, color: [u8; 3]) -> Vec<u8> {
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
    rgba
}

// ---------------------------------------------------------------------------
// Engine-uploading convenience wrappers
// ---------------------------------------------------------------------------

/// Grey/white checkerboard pattern, uploaded to the GPU.
///
/// `cell_size` is the side length of each square in pixels.
pub fn checkerboard(engine: &Engine, width: u32, height: u32, cell_size: u32) -> Arc<Texture> {
    engine.create_texture_from_rgba(
        &checkerboard_pixels(width, height, cell_size),
        width,
        height,
    )
}

/// Flat solid colour, uploaded to the GPU.
///
/// `color` is `[R, G, B, A]`.
pub fn solid(engine: &Engine, width: u32, height: u32, color: [u8; 4]) -> Arc<Texture> {
    engine.create_texture_from_rgba(&solid_pixels(width, height, color), width, height)
}

/// RGB gradient, uploaded to the GPU.
pub fn gradient(engine: &Engine, width: u32, height: u32) -> Arc<Texture> {
    engine.create_texture_from_rgba(&gradient_pixels(width, height), width, height)
}

/// Anti-aliased filled circle, uploaded to the GPU.
///
/// `color` is `[R, G, B]`.
pub fn circle(engine: &Engine, diameter: u32, color: [u8; 3]) -> Arc<Texture> {
    engine.create_texture_from_rgba(&circle_pixels(diameter, color), diameter, diameter)
}

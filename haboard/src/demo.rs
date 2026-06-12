//! Shared demo scene used by the desktop, web, and android examples.
//!
//! Enabled by the `demo-scene` feature so all three example crates render the
//! same set of procedural sprites without duplicating the layout.

use crate::{Sprite, textures};

/// Build the default demo scene: a handful of procedurally generated sprites.
pub fn default_sprites() -> Vec<Sprite> {
    vec![
        Sprite::new(20.0, 20.0, 300.0, 260.0, textures::gradient(128, 128)),
        Sprite::new(360.0, 40.0, 192.0, 192.0, textures::checkerboard(64, 64, 8)),
        Sprite::new(
            50.0,
            420.0,
            96.0,
            96.0,
            textures::solid(32, 32, [210, 50, 50, 255]),
        ),
        Sprite::new(
            180.0,
            420.0,
            96.0,
            96.0,
            textures::solid(32, 32, [50, 100, 220, 255]),
        ),
        Sprite::new(
            120.0,
            100.0,
            144.0,
            144.0,
            textures::solid(48, 48, [40, 200, 80, 160]),
        ),
        Sprite::new(
            500.0,
            310.0,
            128.0,
            128.0,
            textures::circle(128, [255, 160, 20]),
        ),
    ]
}

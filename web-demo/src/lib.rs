//! Web (wasm) example for haboard.
//!
//! Renders the shared demo scene on an HTML canvas via WebGPU (with a WebGL
//! fallback). Built with [Trunk](https://trunkrs.dev): `trunk serve --release`.

use haboard::{SceneMode, SceneRunner, demo};
use wasm_bindgen::prelude::wasm_bindgen;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);

    let sprites = demo::default_sprites();
    // Edit mode: click to select (tints the sprite) and drag to move. Run mode
    // would be inert here because the demo sprites default to `locked`, which
    // blocks dragging in Run mode.
    SceneRunner::new(sprites, SceneMode::Edit).spawn();
}

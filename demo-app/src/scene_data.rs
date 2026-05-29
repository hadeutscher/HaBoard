//! Engine-independent scene state that can be serialized to disk.
//!
//! [`SceneStore`] is the top-level type. It owns a flat list of
//! [`DrawableRecord`]s, each of which captures the position, display size,
//! locking state, and a [`TextureDef`] holding the raw image bytes needed to
//! recreate the texture on any engine.

use serde::{Deserialize, Serialize};

/// Path used for both saving and loading the scene state.
pub const SAVE_PATH: &str = "scene.bin";

// ---------------------------------------------------------------------------
// Texture definition
// ---------------------------------------------------------------------------

/// Engine-independent image data sufficient to (re-)create a GPU texture.
///
/// Neither variant holds any GPU handle; both can be serialized and loaded
/// back on a completely fresh engine.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum TextureDef {
    /// Raw RGBA pixel bytes (R, G, B, A interleaved) with explicit dimensions.
    ///
    /// Pass to [`Engine::create_texture_from_rgba`].
    Rgba {
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    },
    /// Encoded image file bytes (PNG, JPEG, …), decoded at upload time.
    ///
    /// Pass to [`Engine::create_texture_from_image_bytes`].
    Image(Vec<u8>),
}

// ---------------------------------------------------------------------------
// Drawable record
// ---------------------------------------------------------------------------

/// Engine-independent description of a single drawable object.
///
/// Stores everything needed to reconstruct a [`haboard::Sprite`]: display
/// position and size, locking state, and the raw image bytes for the texture.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DrawableRecord {
    /// X position in pixels from the left edge of the window.
    pub x: f32,
    /// Y position in pixels from the top edge of the window.
    pub y: f32,
    /// Display width in pixels (independent of the texture's internal resolution).
    pub width: f32,
    /// Display height in pixels (independent of the texture's internal resolution).
    pub height: f32,
    /// Raw image data used to upload the texture to the GPU.
    pub texture_def: TextureDef,
    /// Whether this drawable is pinned (cannot be dragged in Run mode).
    pub locked: bool,
}

impl DrawableRecord {
    pub fn new(x: f32, y: f32, width: f32, height: f32, texture_def: TextureDef) -> Self {
        Self {
            x,
            y,
            width,
            height,
            texture_def,
            locked: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Scene store
// ---------------------------------------------------------------------------

/// Serializable collection of all drawable records.
///
/// This is the root type written to and read from [`SAVE_PATH`].
#[derive(Serialize, Deserialize, Debug)]
pub struct SceneStore {
    pub drawables: Vec<DrawableRecord>,
}

impl SceneStore {
    /// Load from [`SAVE_PATH`].
    ///
    /// Returns `None` if the file does not exist or cannot be decoded, so the
    /// caller can fall back to a built-in default.
    pub fn load() -> Option<Self> {
        let bytes = std::fs::read(SAVE_PATH).ok()?;
        match postcard::from_bytes(&bytes) {
            Ok(store) => Some(store),
            Err(e) => {
                eprintln!("warn: failed to decode {SAVE_PATH}: {e}");
                None
            }
        }
    }

    /// Serialize and write to [`SAVE_PATH`].
    pub fn save(&self) -> std::io::Result<()> {
        let bytes = postcard::to_allocvec(self).map_err(std::io::Error::other)?;
        std::fs::write(SAVE_PATH, bytes)
    }
}

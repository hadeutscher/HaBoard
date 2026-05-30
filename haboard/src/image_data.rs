use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Engine-independent image data sufficient to create a GPU texture.
///
/// Cheap to clone: the pixel bytes live behind an [`Arc`].
///
/// # Serialization
/// Both variants are serialisable via serde (requires the `rc` feature, already
/// enabled in haboard's own dependency on serde).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ImageData {
    /// Raw RGBA pixel data (R, G, B, A interleaved, row-major).
    Rgba {
        width: u32,
        height: u32,
        bytes: Arc<[u8]>,
    },
    /// Encoded image file bytes (PNG, JPEG, …).
    /// Decoded to RGBA by the engine on upload.
    Encoded(Arc<[u8]>),
}

impl ImageData {
    /// Construct from raw RGBA bytes.
    pub fn rgba(width: u32, height: u32, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self::Rgba {
            width,
            height,
            bytes: bytes.into(),
        }
    }

    /// Construct from encoded image file bytes (PNG, JPEG, …).
    pub fn encoded(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self::Encoded(bytes.into())
    }
}

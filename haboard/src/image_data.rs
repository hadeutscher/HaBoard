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

/// An [`ImageData::Encoded`] payload could not be decoded.
///
/// The underlying decoder error is available through
/// [`source`](std::error::Error::source) but is deliberately not named in the
/// signature, so that the image crate haboard decodes with stays an
/// implementation detail rather than part of a caller's dependency graph.
#[derive(Debug)]
pub struct ImageError(Box<dyn std::error::Error + Send + Sync>);

impl ImageError {
    pub(crate) fn new(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(Box::new(source))
    }
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not decode image data: {}", self.0)
    }
}

impl std::error::Error for ImageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
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

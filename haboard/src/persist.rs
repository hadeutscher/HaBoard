//! Cross-platform scene persistence.
//!
//! A [`SceneStore`] loads and saves a `Vec<Sprite>`. [`SceneRunner`] autosaves
//! through one of these after each committing interaction, so the latest scene
//! is always persisted without relying on a clean shutdown (which the web has
//! no notion of).
//!
//! - Desktop & Android: [`FileStore`] — a `scene.bin` file. Desktop locates it
//!   in the per-user app data directory via [`FileStore::app_data`]; Android
//!   passes its private data dir to [`FileStore::in_dir`].
//! - Web: [`LocalStorageStore`] — base64-encoded bytes in `localStorage`.
//!
//! [`SceneRunner`]: crate::SceneRunner

use crate::Sprite;

/// Loads and saves the scene's sprites.
pub trait SceneStore {
    /// Load a previously saved scene, or `None` if absent/unreadable.
    fn load(&self) -> Option<Vec<Sprite>>;
    /// Persist the given sprites. Failures are logged and otherwise ignored.
    fn save(&self, sprites: &[Sprite]);
}

/// Serialise sprites to the on-disk/wire format (postcard).
fn encode(sprites: &[Sprite]) -> Option<Vec<u8>> {
    match postcard::to_allocvec(sprites) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            log::error!("failed to serialise scene: {e}");
            None
        }
    }
}

/// Deserialise sprites from the on-disk/wire format (postcard).
fn decode(bytes: &[u8]) -> Option<Vec<Sprite>> {
    match postcard::from_bytes::<Vec<Sprite>>(bytes) {
        Ok(sprites) => Some(sprites),
        Err(e) => {
            log::warn!("failed to read saved scene: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// File-backed store (desktop + Android)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub use file::FileStore;

#[cfg(not(target_arch = "wasm32"))]
mod file {
    use std::path::PathBuf;

    use super::{SceneStore, decode, encode};
    use crate::Sprite;

    /// Saves the scene to a `scene.bin` file.
    pub struct FileStore {
        path: PathBuf,
    }

    impl FileStore {
        /// Store `scene.bin` inside the given directory (created on save if
        /// missing). Used on Android with `AndroidApp::internal_data_path()`.
        pub fn in_dir(dir: PathBuf) -> Self {
            Self {
                path: dir.join("scene.bin"),
            }
        }

        /// Locate the per-user application data directory and store `scene.bin`
        /// there. Returns `None` if no such directory can be determined.
        ///
        /// On Windows this resolves to
        /// `%APPDATA%\deut\HaBoard\data\scene.bin`.
        #[cfg(not(target_os = "android"))]
        pub fn app_data() -> Option<Self> {
            let dirs = directories::ProjectDirs::from("sh", "deut", "HaBoard")?;
            Some(Self::in_dir(dirs.data_dir().to_path_buf()))
        }
    }

    impl SceneStore for FileStore {
        fn load(&self) -> Option<Vec<Sprite>> {
            let bytes = std::fs::read(&self.path).ok()?;
            decode(&bytes)
        }

        fn save(&self, sprites: &[Sprite]) {
            let Some(bytes) = encode(sprites) else {
                return;
            };
            if let Some(parent) = self.path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                log::error!("failed to create {}: {e}", parent.display());
                return;
            }
            if let Err(e) = std::fs::write(&self.path, bytes) {
                log::error!("failed to save scene to {}: {e}", self.path.display());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// localStorage-backed store (web)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
pub use local::LocalStorageStore;

#[cfg(target_arch = "wasm32")]
mod local {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;

    use super::{SceneStore, decode, encode};
    use crate::Sprite;

    /// Saves the scene as a base64 string in browser `localStorage`.
    pub struct LocalStorageStore {
        key: String,
    }

    impl LocalStorageStore {
        /// Create a store writing under the given `localStorage` key.
        pub fn new(key: impl Into<String>) -> Self {
            Self { key: key.into() }
        }

        fn storage() -> Option<web_sys::Storage> {
            web_sys::window()?.local_storage().ok()?
        }
    }

    impl SceneStore for LocalStorageStore {
        fn load(&self) -> Option<Vec<Sprite>> {
            let b64 = Self::storage()?.get_item(&self.key).ok()??;
            let bytes = B64.decode(b64).ok()?;
            decode(&bytes)
        }

        fn save(&self, sprites: &[Sprite]) {
            let Some(bytes) = encode(sprites) else {
                return;
            };
            if let Some(storage) = Self::storage() {
                let _ = storage.set_item(&self.key, &B64.encode(bytes));
            }
        }
    }
}

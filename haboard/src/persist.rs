//! Cross-platform scene persistence.
//!
//! A [`SceneStore`] loads and saves any list of serde-compatible items.
//! Apps own their store, load up front, and wire persistence through the
//! [`on_change`] callback on [`SceneRunner`]; the runner itself no longer
//! holds a store.
//!
//! - Desktop & Android: [`FileStore`] — a `scene.bin` file. Desktop locates it
//!   in the per-user app data directory via [`FileStore::app_data`]; Android
//!   passes its private data dir to [`FileStore::in_dir`].
//! - Web: [`LocalStorageStore`] — base64-encoded bytes in `localStorage`.
//!
//! [`SceneRunner`]: crate::SceneRunner
//! [`on_change`]: crate::SceneRunner::on_change

use serde::{Serialize, de::DeserializeOwned};

/// Loads and saves a list of items of type `T`.
pub trait SceneStore<T> {
    /// Load a previously saved list, or `None` if absent/unreadable.
    fn load(&self) -> Option<Vec<T>>;
    /// Persist the given items. Failures are logged and otherwise ignored.
    fn save(&self, items: &[T]);
}

/// Serialise items to the on-disk/wire format (postcard).
fn encode<T: Serialize>(items: &[T]) -> Option<Vec<u8>> {
    match postcard::to_allocvec(items) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            log::error!("failed to serialise scene: {e}");
            None
        }
    }
}

/// Deserialise items from the on-disk/wire format (postcard).
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Option<Vec<T>> {
    match postcard::from_bytes::<Vec<T>>(bytes) {
        Ok(items) => Some(items),
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

    use serde::{Serialize, de::DeserializeOwned};

    use super::{SceneStore, decode, encode};

    /// Saves items to a `scene.bin` file.
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

    impl<T: Serialize + DeserializeOwned> SceneStore<T> for FileStore {
        fn load(&self) -> Option<Vec<T>> {
            let bytes = std::fs::read(&self.path).ok()?;
            decode(&bytes)
        }

        fn save(&self, items: &[T]) {
            let Some(bytes) = encode(items) else {
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
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
    use serde::{Serialize, de::DeserializeOwned};

    use super::{SceneStore, decode, encode};

    /// Saves items as a base64 string in browser `localStorage`.
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

    impl<T: Serialize + DeserializeOwned> SceneStore<T> for LocalStorageStore {
        fn load(&self) -> Option<Vec<T>> {
            let b64 = Self::storage()?.get_item(&self.key).ok()??;
            let bytes = B64.decode(b64).ok()?;
            decode(&bytes)
        }

        fn save(&self, items: &[T]) {
            let Some(bytes) = encode(items) else {
                return;
            };
            if let Some(storage) = Self::storage() {
                let _ = storage.set_item(&self.key, &B64.encode(bytes));
            }
        }
    }
}

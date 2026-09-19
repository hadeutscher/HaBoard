//! A cross-platform GPU-accelerated 2D sprite engine.
//!
//! haboard is layered so that it can be used either as a whole application or
//! as a control inside a host that already owns its event loop and window:
//!
//! - **Core** — [`Engine`], [`Scene`], [`Drawables`] and [`input`]. Knows
//!   nothing about windowing. [`Engine::new`] takes any surface target wgpu
//!   accepts (an `Arc<winit::window::Window>`, or a canvas via
//!   [`Engine::from_canvas`]), and [`Scene::handle`] consumes an
//!   [`Input`](input::Input) and returns a [`Response`](input::Response).
//! - **winit adapter** (`winit` feature, on by default) — maps
//!   `winit::event::WindowEvent` onto [`input::Input`].
//! - **Application shell** (`winit` feature) — [`SceneRunner`] owns the window,
//!   the event loop and the lifecycle.
//!
//! An embedder disables default features to keep winit out of the dependency
//! graph entirely:
//!
//! ```toml
//! haboard = { version = "0.2", default-features = false }
//! ```
//!
//! # Coordinates
//! Every size and position in haboard is in **physical pixels**. The engine
//! applies no scale factor; a host working in logical units (CSS pixels, say)
//! multiplies by its own scale factor before calling in, and sizes its surface
//! to match.

#[cfg(feature = "demo-scene")]
pub mod demo;
pub mod drawable;
pub mod drawables;
pub mod engine;
pub mod image_data;
pub mod input;
pub mod persist;
pub mod scene;
#[cfg(feature = "winit")]
pub mod scene_runner;
mod snap;
pub mod sprite;
pub mod texture;
pub mod textures;
#[cfg(target_arch = "wasm32")]
pub mod web;
#[cfg(feature = "winit")]
pub mod winit_adapter;

pub use drawable::Drawable;
pub use drawables::{DrawableId, Drawables};
pub use engine::{Engine, EngineError};
pub use image_data::{ImageData, ImageError};
pub use input::{Commit, Input, Key, Modifiers, PointerId, PointerPhase, Response};
#[cfg(not(target_arch = "wasm32"))]
pub use persist::FileStore;
#[cfg(target_arch = "wasm32")]
pub use persist::LocalStorageStore;
pub use persist::SceneStore;
pub use scene::{Scene, SceneMode};
#[cfg(all(
    feature = "winit",
    not(any(target_arch = "wasm32", target_os = "android"))
))]
pub use scene_runner::DroppedImage;
#[cfg(feature = "winit")]
pub use scene_runner::{SceneRunner, UserEvent};
pub use sprite::Sprite;
#[cfg(feature = "winit")]
pub use winit_adapter::WinitInput;

/// The wgpu version haboard was built against.
///
/// Re-exported so that a host naming a wgpu type — a
/// [`wgpu::SurfaceTarget`] passed to [`Engine::new`], or the source of an
/// [`EngineError`] — does not need its own wgpu dependency pinned in lockstep
/// with haboard's.
pub use wgpu;

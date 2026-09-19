//! Ready-made [`winit`] application runner for a haboard [`Scene`].
//!
//! [`SceneRunner`] implements [`winit::application::ApplicationHandler`] and
//! handles window creation, GPU init, event routing to the scene, drag-and-drop
//! image import (desktop only), and the Android suspend/resume surface
//! lifecycle.  Hook into any remaining window events via
//! [`SceneRunner::on_event`]. Wire persistence through
//! [`SceneRunner::on_change`], which fires after each committing interaction.
//!
//! ## Platforms
//! - **Desktop:** [`SceneRunner::run`] builds the event loop, blocks on GPU
//!   init with `pollster`, and returns when the window closes.
//! - **Web (wasm):** [`SceneRunner::spawn`] starts the loop non-blocking and
//!   initialises the GPU asynchronously, delivering the ready [`Engine`] back
//!   through an [`EventLoopProxy`] as a [`UserEvent`].
//! - **Android:** build the event loop yourself with the `AndroidApp` and call
//!   [`SceneRunner::run_with`].

use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::ModifiersState,
    window::{Window, WindowAttributes, WindowId},
};

use crate::{Drawable, Engine, Scene, SceneMode, input::Commit};

/// Callback type for [`SceneRunner::on_event`].
type EventHandler<T> = dyn FnMut(&WindowEvent, &ModifiersState, Option<&mut Scene<T>>);

/// Custom event delivered to the winit event loop.
///
/// On web the GPU is initialised asynchronously; the ready [`Engine`] is sent
/// back to the loop through an [`EventLoopProxy`] as `EngineReady`.
pub enum UserEvent {
    /// The asynchronous [`Engine`] initialisation has completed.
    EngineReady(Engine),
}

/// Lifecycle state of the runner.
enum AppState<T: Drawable> {
    /// No window/engine yet (before the first `resumed`).
    Uninitialized,
    /// Window created, GPU init in flight (web async path only).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Loading,
    /// Engine ready; owns the live scene. Boxed — a `Scene` is large relative
    /// to the other (empty) variants.
    Ready(Box<Scene<T>>),
}

/// An image file dragged and dropped onto the window (desktop only).
///
/// Passed to the [`on_drop_image`] callback; the callback returns the `T` to
/// add to the scene.
///
/// [`on_drop_image`]: SceneRunner::on_drop_image
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub struct DroppedImage {
    /// Decoded RGBA pixel data.
    pub image: crate::ImageData,
    /// X position (pixels from window top-left) to place the item.
    pub x: f32,
    /// Y position (pixels from window top-left) to place the item.
    pub y: f32,
    /// Display width in pixels (scaled to fit [`max_drop_dim`]).
    ///
    /// [`max_drop_dim`]: SceneRunner::max_drop_dim
    pub width: f32,
    /// Display height in pixels (scaled to fit [`max_drop_dim`]).
    ///
    /// [`max_drop_dim`]: SceneRunner::max_drop_dim
    pub height: f32,
}

pub type OnChangeCallback<T> = Box<dyn FnMut(&Scene<T>)>;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub type OnDropImageCallback<T> = Box<dyn FnMut(DroppedImage) -> T>;

/// A ready-made [`winit`] application that owns a [`Scene<T>`] and handles the
/// full application lifecycle.
///
/// `SceneRunner` takes care of window creation, GPU init, resize, redraw,
/// drag-and-drop image import (desktop), and the Android surface lifecycle.
/// Register an [`on_event`] callback to handle any additional window events
/// (such as keyboard shortcuts) yourself.  Wire persistence through
/// [`on_change`], which fires after each committing interaction.
///
/// # Example
/// ```no_run
/// use haboard::{SceneRunner, SceneMode, Sprite};
///
/// let mut runner = SceneRunner::<Sprite>::new(Vec::new(), SceneMode::Edit);
/// runner.on_event(|event, modifiers, scene| {
///     // handle custom events here
/// });
/// runner.run();
/// ```
///
/// [`on_event`]: SceneRunner::on_event
/// [`on_change`]: SceneRunner::on_change
pub struct SceneRunner<T: Drawable> {
    scene_mode: SceneMode,
    /// Consumed once when the engine becomes ready.
    initial: Option<Vec<T>>,
    state: AppState<T>,
    /// The window the runner created. Owned here rather than by the scene: the
    /// engine needs only a surface, and the redraw request belongs to whoever
    /// drives the loop.
    window: Option<Arc<Window>>,
    /// Proxy used to deliver the async-initialised engine back to the loop.
    proxy: Option<EventLoopProxy<UserEvent>>,
    /// Last known cursor position, used to centre drag-dropped images.
    cursor_pos: (f32, f32),
    /// Current keyboard modifier state, forwarded to [`on_event`] callbacks.
    ///
    /// [`on_event`]: SceneRunner::on_event
    pub modifiers: ModifiersState,
    /// Maximum display size (longest side, in pixels) for drag-dropped images.
    /// Images larger than this are scaled down proportionally. Default:
    /// `400.0`.
    pub max_drop_dim: f32,
    event_handler: Option<Box<EventHandler<T>>>,
    on_change: Option<OnChangeCallback<T>>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    on_drop_image: Option<OnDropImageCallback<T>>,
}

impl<T: Drawable + 'static> SceneRunner<T> {
    /// Create a new runner with the given initial items and interaction mode.
    pub fn new(initial: Vec<T>, mode: SceneMode) -> Self {
        Self {
            scene_mode: mode,
            initial: Some(initial),
            state: AppState::Uninitialized,
            window: None,
            proxy: None,
            cursor_pos: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
            max_drop_dim: 400.0,
            event_handler: None,
            on_change: None,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            on_drop_image: None,
        }
    }

    /// Register a callback for window events not directly handled by
    /// `SceneRunner` (i.e. everything other than `DroppedFile`,
    /// `CloseRequested`, and `RedrawRequested`).
    ///
    /// The callback receives the event, the current modifier state, and a
    /// mutable reference to the scene (if one has been created yet).
    ///
    /// ```no_run
    /// # use haboard::{SceneRunner, SceneMode, Sprite};
    /// # use winit::event::WindowEvent;
    /// let mut runner = SceneRunner::<Sprite>::new(vec![], SceneMode::Edit);
    /// runner.on_event(|event, modifiers, scene| {
    ///     // handle custom events here
    /// });
    /// ```
    pub fn on_event<F>(&mut self, handler: F)
    where
        F: FnMut(&WindowEvent, &ModifiersState, Option<&mut Scene<T>>) + 'static,
    {
        self.event_handler = Some(Box::new(handler));
    }

    /// Register a callback invoked after each committing interaction (drag
    /// release, touch end, edit keypress, image drop).
    ///
    /// Use this to persist the scene — the callback receives a shared reference
    /// to the live scene.
    ///
    /// ```no_run
    /// # use haboard::{SceneRunner, SceneMode, Sprite};
    /// let runner = SceneRunner::<Sprite>::new(vec![], SceneMode::Edit)
    ///     .on_change(|scene| {
    ///         // persist scene here
    ///     });
    /// ```
    pub fn on_change<F>(mut self, handler: F) -> Self
    where
        F: FnMut(&Scene<T>) + 'static,
    {
        self.on_change = Some(Box::new(handler));
        self
    }

    /// Register a callback that turns a dropped image file into a `T`.
    ///
    /// When the user drags an image file onto the window (desktop only), the
    /// runner decodes the image, scales it to fit [`max_drop_dim`], and calls
    /// this callback to construct the item to add to the scene.
    ///
    /// [`max_drop_dim`]: SceneRunner::max_drop_dim
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub fn on_drop_image<F>(mut self, handler: F) -> Self
    where
        F: FnMut(DroppedImage) -> T + 'static,
    {
        self.on_drop_image = Some(Box::new(handler));
        self
    }

    /// Run the event loop on desktop, blocking until the window is closed.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run(self) {
        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .build()
            .expect("Failed to create event loop");
        self.run_with(event_loop);
    }

    /// Run with a caller-supplied event loop, blocking until exit.
    ///
    /// Use this on Android, where the event loop must be built from the
    /// `AndroidApp` (`EventLoopBuilderExtAndroid::with_android_app`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run_with(mut self, event_loop: EventLoop<UserEvent>) {
        self.proxy = Some(event_loop.create_proxy());
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop.run_app(&mut self).expect("Event loop error");
    }

    /// Start the event loop on the web without blocking the calling thread.
    ///
    /// The GPU is initialised asynchronously; control returns to the browser
    /// immediately.
    #[cfg(target_arch = "wasm32")]
    pub fn spawn(mut self) {
        use winit::platform::web::EventLoopExtWebSys;
        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .build()
            .expect("Failed to create event loop");
        self.proxy = Some(event_loop.create_proxy());
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop.spawn_app(self);
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Build the platform-appropriate window attributes.
    fn window_attributes() -> WindowAttributes {
        let attrs = Window::default_attributes().with_title("HaBoard");
        #[cfg(not(target_arch = "wasm32"))]
        let attrs = attrs.with_maximized(true);
        #[cfg(target_arch = "wasm32")]
        let attrs = {
            // Append the winit-created canvas to the document body.
            use winit::platform::web::WindowAttributesExtWebSys;
            attrs.with_append(true)
        };
        attrs
    }

    /// Build the scene from a ready engine and transition to `Ready`.
    fn set_ready(&mut self, engine: Engine) {
        let items = self.initial.take().unwrap_or_default();
        let mut scene = Scene::new(engine, items, self.scene_mode);
        // On the web, the size passed to `Engine::new` was read before the
        // canvas had any CSS layout (GPU init is async, so by the time we get
        // here winit's `ResizeObserver` has already updated its tracked size,
        // even though the `Resized` event itself was dropped — there was no
        // scene yet to receive it while `AppState` was `Loading`). Catch up
        // once.
        #[cfg(target_arch = "wasm32")]
        if let Some(window) = &self.window {
            let size = window.inner_size();
            scene.resize((size.width, size.height));
        }
        scene.render();
        self.state = AppState::Ready(Box::new(scene));
    }

    /// Invoke the `on_change` callback with the current scene.
    ///
    /// Uses take()/replace() to avoid holding a borrow on `self` while the
    /// callback runs.
    fn invoke_on_change(&mut self) {
        if let AppState::Ready(scene) = &self.state
            && let Some(mut cb) = self.on_change.take()
        {
            cb(scene);
            self.on_change = Some(cb);
        }
    }

    /// Handle a file dragged and dropped onto the window (desktop only).
    ///
    /// Only acts in [`SceneMode::Edit`]. Non-image files are reported and
    /// ignored. Requires an [`on_drop_image`] callback to be registered.
    ///
    /// [`on_drop_image`]: SceneRunner::on_drop_image
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    fn handle_dropped_file(&mut self, path: &std::path::Path) {
        {
            let AppState::Ready(scene) = &self.state else {
                return;
            };
            if scene.mode() != SceneMode::Edit {
                return;
            }
        }

        // No callback registered — nothing to do.
        if self.on_drop_image.is_none() {
            return;
        }

        let raw_bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                log::error!("could not read {}: {e}", path.display());
                return;
            }
        };
        let img = match image::load_from_memory(&raw_bytes) {
            Ok(img) => img.into_rgba8(),
            Err(e) => {
                log::error!("{} is not a valid image: {e}", path.display());
                return;
            }
        };

        let (img_w, img_h) = img.dimensions();
        let scale = (self.max_drop_dim / img_w as f32)
            .min(self.max_drop_dim / img_h as f32)
            .min(1.0);
        let width = img_w as f32 * scale;
        let height = img_h as f32 * scale;
        let x = (self.cursor_pos.0 - width / 2.0).max(0.0);
        let y = (self.cursor_pos.1 - height / 2.0).max(0.0);

        let image = crate::ImageData::rgba(img_w, img_h, img.into_raw());
        let dropped = DroppedImage {
            image,
            x,
            y,
            width,
            height,
        };

        // Take the callback, build the item, restore the callback — avoids
        // holding a borrow on self while the callback runs.
        if let Some(mut cb) = self.on_drop_image.take() {
            let mut item = cb(dropped);
            if let AppState::Ready(scene) = &mut self.state {
                item.set_z(scene.drawables.max_z() + 1.0);
                scene.add_drawable(item);
            }
            self.on_drop_image = Some(cb);
        }
    }
}

impl<T: Drawable + 'static> ApplicationHandler<UserEvent> for SceneRunner<T> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(Self::window_attributes())
                .expect("Failed to create window"),
        );
        let size = window.inner_size();
        self.window = Some(Arc::clone(&window));

        // Re-resume after an Android suspend: recreate the surface on the fresh
        // window rather than rebuilding the whole engine.
        if let AppState::Ready(scene) = &mut self.state {
            if let Err(e) = scene.recreate_surface(window, (size.width, size.height)) {
                log::error!("could not recreate the surface on resume: {e}");
            }
            return;
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            match pollster::block_on(Engine::new(window, (size.width, size.height))) {
                Ok(engine) => self.set_ready(engine),
                // Nothing can be drawn without a GPU, so there is no useful
                // degraded mode for an application; an embedder handles the
                // same failure itself by calling `Engine::new` directly.
                Err(e) => {
                    log::error!("GPU initialisation failed: {e}");
                    event_loop.exit();
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.state = AppState::Loading;
            let proxy = self.proxy.clone().expect("proxy must be set before run");
            wasm_bindgen_futures::spawn_local(async move {
                match Engine::new(window, (size.width, size.height)).await {
                    Ok(engine) => {
                        let _ = proxy.send_event(UserEvent::EngineReady(engine));
                    }
                    Err(e) => log::error!("GPU initialisation failed: {e}"),
                }
            });
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        if let AppState::Ready(scene) = &mut self.state {
            scene.drop_surface();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::EngineReady(engine) => self.set_ready(engine),
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            WindowEvent::DroppedFile(ref path) => {
                self.handle_dropped_file(path);
                self.invoke_on_change();
            }
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                if let AppState::Ready(scene) = &mut self.state {
                    scene.render();
                }
            }
            // A window losing focus can interrupt a run of auto-repeats before
            // the key release that would have flushed it, so settle any
            // deferred change here rather than losing it.
            WindowEvent::Focused(false) => {
                let owed = match &mut self.state {
                    AppState::Ready(scene) => scene.take_pending_commit(),
                    _ => false,
                };
                if owed {
                    self.invoke_on_change();
                }
            }
            _ => {
                if let WindowEvent::CursorMoved { position, .. } = &event {
                    self.cursor_pos = (position.x as f32, position.y as f32);
                }
                if let WindowEvent::ModifiersChanged(mods) = &event {
                    self.modifiers = mods.state();
                }
                let mut commit = Commit::No;
                if let AppState::Ready(scene) = &mut self.state {
                    commit = scene.handle_winit_event(&event).commit;
                }
                // Offer unhandled events to the caller's hook.
                // Use take()/replace() to avoid holding a borrow on self while
                // the handler mutably borrows the scene.
                if let Some(mut handler) = self.event_handler.take() {
                    let scene = match &mut self.state {
                        AppState::Ready(s) => Some(s.as_mut()),
                        _ => None,
                    };
                    handler(&event, &self.modifiers, scene);
                    self.event_handler = Some(handler);
                }
                // Persist once the interaction that just landed has finished.
                // `Commit::Defer` is deliberately not a commit point: it marks
                // a change still in progress, such as a held arrow key, and is
                // flushed by the release that ends the run.
                if commit.is_now() {
                    self.invoke_on_change();
                }
            }
        }
    }
}

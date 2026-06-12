//! Ready-made [`winit`] application runner for a haboard [`Scene`].
//!
//! [`SceneRunner`] implements [`winit::application::ApplicationHandler`] and
//! handles window creation, GPU init, event routing to the scene, drag-and-drop
//! image import (desktop only), and the Android suspend/resume surface
//! lifecycle.  Hook into any remaining window events via [`SceneRunner::on_event`].
//!
//! ## Platforms
//! - **Desktop:** [`SceneRunner::run`] builds the event loop, blocks on GPU init
//!   with `pollster`, and returns the final sprites when the window closes.
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

use crate::{Engine, Scene, SceneMode, Sprite};

/// Callback type for [`SceneRunner::on_event`].
type EventHandler = dyn FnMut(&WindowEvent, &ModifiersState, Option<&mut Scene<Sprite>>);

/// Custom event delivered to the winit event loop.
///
/// On web the GPU is initialised asynchronously; the ready [`Engine`] is sent
/// back to the loop through an [`EventLoopProxy`] as `EngineReady`.
pub enum UserEvent {
    /// The asynchronous [`Engine`] initialisation has completed.
    EngineReady(Engine),
}

/// Lifecycle state of the runner.
enum AppState {
    /// No window/engine yet (before the first `resumed`).
    Uninitialized,
    /// Window created, GPU init in flight (web async path only).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Loading,
    /// Engine ready; owns the live scene. Boxed — a `Scene` is large relative
    /// to the other (empty) variants.
    Ready(Box<Scene<Sprite>>),
}

/// A ready-made [`winit`] application that owns a [`Scene<Sprite>`] and
/// handles the full application lifecycle.
///
/// `SceneRunner` takes care of window creation, GPU init, resize, redraw,
/// drag-and-drop image import (desktop), and the Android surface lifecycle.
/// Register an [`on_event`] callback to handle any additional window events
/// (such as keyboard shortcuts) yourself.
///
/// # Example
/// ```no_run
/// use haboard::{SceneRunner, SceneMode};
///
/// let sprites = vec![];
/// let _final = SceneRunner::new(sprites, SceneMode::Edit).run();
/// ```
///
/// [`on_event`]: SceneRunner::on_event
pub struct SceneRunner {
    scene_mode: SceneMode,
    /// Consumed once when the engine becomes ready.
    initial_sprites: Option<Vec<Sprite>>,
    state: AppState,
    /// Proxy used to deliver the async-initialised engine back to the loop.
    proxy: Option<EventLoopProxy<UserEvent>>,
    /// Last known cursor position, used to centre drag-dropped images.
    cursor_pos: (f32, f32),
    /// Current keyboard modifier state, forwarded to [`on_event`] callbacks.
    ///
    /// [`on_event`]: SceneRunner::on_event
    pub modifiers: ModifiersState,
    /// Maximum display size (longest side, in pixels) for drag-dropped images.
    /// Images larger than this are scaled down proportionally. Default: `400.0`.
    pub max_drop_dim: f32,
    event_handler: Option<Box<EventHandler>>,
}

impl SceneRunner {
    /// Create a new runner with the given initial sprites and interaction mode.
    pub fn new(initial_sprites: Vec<Sprite>, mode: SceneMode) -> Self {
        Self {
            scene_mode: mode,
            initial_sprites: Some(initial_sprites),
            state: AppState::Uninitialized,
            proxy: None,
            cursor_pos: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
            max_drop_dim: 400.0,
            event_handler: None,
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
    /// # use haboard::{SceneRunner, SceneMode};
    /// # use winit::event::WindowEvent;
    /// let mut runner = SceneRunner::new(vec![], SceneMode::Edit);
    /// runner.on_event(|event, modifiers, scene| {
    ///     // handle custom events here
    /// });
    /// ```
    pub fn on_event<F>(&mut self, handler: F)
    where
        F: FnMut(&WindowEvent, &ModifiersState, Option<&mut Scene<Sprite>>) + 'static,
    {
        self.event_handler = Some(Box::new(handler));
    }

    /// Run the event loop on desktop, blocking until the window is closed, and
    /// return the final scene sprites for persistence.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run(self) -> Vec<Sprite> {
        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .build()
            .expect("Failed to create event loop");
        self.run_with(event_loop)
    }

    /// Run with a caller-supplied event loop, blocking until exit, and return
    /// the final scene sprites.
    ///
    /// Use this on Android, where the event loop must be built from the
    /// `AndroidApp` (`EventLoopBuilderExtAndroid::with_android_app`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run_with(mut self, event_loop: EventLoop<UserEvent>) -> Vec<Sprite> {
        self.proxy = Some(event_loop.create_proxy());
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop.run_app(&mut self).expect("Event loop error");
        self.sprites()
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

    /// Return the current scene's sprites, collected into a `Vec`.
    ///
    /// Useful after [`run`](Self::run) returns to retrieve the final scene state
    /// for persistence or inspection.
    pub fn sprites(&self) -> Vec<Sprite> {
        match &self.state {
            AppState::Ready(scene) => scene.drawables.iter().cloned().collect(),
            _ => Vec::new(),
        }
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

    /// Keep the web surface matched to the canvas's actual displayed size.
    ///
    /// On the web the canvas has no layout at `resumed` time, so the surface is
    /// first configured at a degenerate size. This re-reads the canvas client
    /// size (scaled by the device pixel ratio) every frame and resizes when it
    /// differs — self-healing after the first browser layout and on any later
    /// window resize. It is a no-op once the sizes agree.
    #[cfg(target_arch = "wasm32")]
    fn sync_canvas_size(&mut self) {
        use winit::dpi::PhysicalSize;
        use winit::platform::web::WindowExtWebSys;

        let AppState::Ready(scene) = &mut self.state else {
            return;
        };
        let Some(canvas) = scene.window().canvas() else {
            return;
        };
        let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
        let w = (canvas.client_width().max(0) as f64 * dpr).round() as u32;
        let h = (canvas.client_height().max(0) as f64 * dpr).round() as u32;
        if w == 0 || h == 0 || (w, h) == scene.size() {
            return;
        }
        // Set the canvas backing resolution (keeps winit's pointer mapping in
        // sync) and reconfigure the surface to match.
        let _ = scene.window().request_inner_size(PhysicalSize::new(w, h));
        scene.resize(PhysicalSize::new(w, h));
    }

    /// Build the scene from a ready engine and transition to `Ready`.
    fn set_ready(&mut self, engine: Engine) {
        let sprites = self.initial_sprites.take().unwrap_or_default();
        let mut scene = Scene::new(engine, sprites, self.scene_mode);
        scene.render();
        self.state = AppState::Ready(Box::new(scene));
    }

    /// Handle a file dragged and dropped onto the window (desktop only).
    ///
    /// Only acts in [`SceneMode::Edit`]. Non-image files are reported and
    /// ignored.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    fn handle_dropped_file(&mut self, path: &std::path::Path) {
        let AppState::Ready(scene) = &mut self.state else {
            return;
        };
        if scene.mode() != SceneMode::Edit {
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
        let w = img_w as f32 * scale;
        let h = img_h as f32 * scale;
        let x = (self.cursor_pos.0 - w / 2.0).max(0.0);
        let y = (self.cursor_pos.1 - h / 2.0).max(0.0);

        let image = crate::ImageData::rgba(img_w, img_h, img.into_raw());
        let z = scene.drawables.max_z() + 1.0;
        let mut sprite = Sprite::new(x, y, w, h, image);
        sprite.z = z;
        scene.drawables.push(sprite);
    }
}

impl ApplicationHandler<UserEvent> for SceneRunner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Re-resume after an Android suspend: recreate the surface on a fresh
        // window rather than rebuilding the whole engine.
        if let AppState::Ready(scene) = &mut self.state {
            let window = Arc::new(
                event_loop
                    .create_window(Self::window_attributes())
                    .expect("Failed to create window"),
            );
            scene.recreate_surface(window);
            return;
        }

        let window = Arc::new(
            event_loop
                .create_window(Self::window_attributes())
                .expect("Failed to create window"),
        );

        #[cfg(not(target_arch = "wasm32"))]
        {
            let engine = pollster::block_on(Engine::new(window));
            self.set_ready(engine);
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.state = AppState::Loading;
            let proxy = self.proxy.clone().expect("proxy must be set before run");
            wasm_bindgen_futures::spawn_local(async move {
                let engine = Engine::new(window).await;
                let _ = proxy.send_event(UserEvent::EngineReady(engine));
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
        #[cfg(target_arch = "wasm32")]
        self.sync_canvas_size();
        if let AppState::Ready(scene) = &self.state {
            scene.window().request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            WindowEvent::DroppedFile(ref path) => {
                self.handle_dropped_file(path);
            }
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                if let AppState::Ready(scene) = &mut self.state {
                    scene.render();
                }
            }
            _ => {
                if let WindowEvent::CursorMoved { position, .. } = &event {
                    self.cursor_pos = (position.x as f32, position.y as f32);
                }
                if let WindowEvent::ModifiersChanged(mods) = &event {
                    self.modifiers = mods.state();
                }
                if let AppState::Ready(scene) = &mut self.state {
                    scene.handle_window_event(&event);
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
            }
        }
    }
}

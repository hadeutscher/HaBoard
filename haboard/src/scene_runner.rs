//! Ready-made [`winit`] application runner for a haboard [`Scene`].
//!
//! [`SceneRunner`] implements [`winit::application::ApplicationHandler`] and
//! handles window creation, event routing to the scene, drag-and-drop image
//! import, and auto-save on close.  Hook into any remaining window events via
//! [`SceneRunner::on_event`].

use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::ModifiersState,
    window::{Window, WindowId},
};

use crate::{Engine, ImageData, Scene, SceneMode, Sprite};

/// A ready-made [`winit`] application that owns a [`Scene<Sprite>`] and
/// handles the full application lifecycle.
///
/// `SceneRunner` takes care of window creation, resize, redraw, drag-and-drop
/// image import, and auto-save on close.  Register an [`on_event`] callback to
/// handle any additional window events (such as keyboard shortcuts) yourself.
///
/// # Example
/// ```no_run
/// use haboard::{SceneRunner, SceneMode};
///
/// let sprites = SceneRunner::load("scene.bin").unwrap_or_default();
/// SceneRunner::new(sprites, SceneMode::Edit).run();
/// ```
///
/// [`on_event`]: SceneRunner::on_event
pub struct SceneRunner {
    scene_mode: SceneMode,
    /// Consumed once on the first [`ApplicationHandler::resumed`] call.
    initial_sprites: Option<Vec<Sprite>>,
    scene: Option<Scene<Sprite>>,
    /// Last known cursor position, used to centre drag-dropped images.
    cursor_pos: (f32, f32),
    /// Current keyboard modifier state, forwarded to [`on_event`] callbacks.
    ///
    /// [`on_event`]: SceneRunner::on_event
    pub modifiers: ModifiersState,
    /// Maximum display size (longest side, in pixels) for drag-dropped images.
    /// Images larger than this are scaled down proportionally. Default: `400.0`.
    pub max_drop_dim: f32,
    event_handler:
        Option<Box<dyn FnMut(&WindowEvent, &ModifiersState, Option<&mut Scene<Sprite>>)>>,
}

impl SceneRunner {
    /// Create a new runner with the given initial sprites and interaction mode.
    pub fn new(initial_sprites: Vec<Sprite>, mode: SceneMode) -> Self {
        Self {
            scene_mode: mode,
            initial_sprites: Some(initial_sprites),
            scene: None,
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

    /// Run the event loop, blocking until the window is closed.
    pub fn run(&mut self) {
        let event_loop = EventLoop::new().expect("Failed to create event loop");
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop.run_app(self).expect("Event loop error");
    }

    /// Return the current scene's sprites, collected into a `Vec`.
    ///
    /// Useful after [`run`](Self::run) returns to retrieve the final scene state
    /// for persistence or inspection.
    pub fn sprites(&self) -> Vec<Sprite> {
        self.scene
            .as_ref()
            .map(|s| s.drawables.iter().cloned().collect())
            .unwrap_or_default()
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Handle a file dragged and dropped onto the window.
    ///
    /// Only acts in [`SceneMode::Edit`]. Non-image files are reported to stderr
    /// and ignored.
    fn handle_dropped_file(&mut self, path: &std::path::Path) {
        let in_edit = self
            .scene
            .as_ref()
            .is_some_and(|s| s.mode() == SceneMode::Edit);
        if !in_edit {
            return;
        }

        let raw_bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: could not read {}: {e}", path.display());
                return;
            }
        };
        let img = match image::load_from_memory(&raw_bytes) {
            Ok(img) => img.into_rgba8(),
            Err(e) => {
                eprintln!("error: {} is not a valid image: {e}", path.display());
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

        let image = ImageData::rgba(img_w, img_h, img.into_raw());
        let scene = self.scene.as_mut().unwrap();
        let z = scene.drawables.max_z() + 1.0;
        let mut sprite = Sprite::new(x, y, w, h, image);
        sprite.z = z;
        scene.drawables.push(sprite);
    }
}

impl ApplicationHandler for SceneRunner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("HaBoard")
                        .with_inner_size(winit::dpi::LogicalSize::new(800u32, 600u32)),
                )
                .expect("Failed to create window"),
        );
        let engine = pollster::block_on(Engine::new(window));
        let sprites = self.initial_sprites.take().unwrap_or_default();
        self.scene = Some(Scene::new(engine, sprites, self.scene_mode));
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(scene) = &self.scene {
            scene.window().request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::DroppedFile(ref path) => {
                self.handle_dropped_file(path);
            }
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                if let Some(scene) = &mut self.scene {
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
                if let Some(scene) = &mut self.scene {
                    scene.handle_window_event(&event);
                }
                // Offer unhandled events to the caller's hook.
                // Use take()/replace() to avoid holding a borrow on self while
                // the handler mutably borrows self.scene.
                if let Some(mut handler) = self.event_handler.take() {
                    handler(&event, &self.modifiers, self.scene.as_mut());
                    self.event_handler = Some(handler);
                }
            }
        }
    }
}

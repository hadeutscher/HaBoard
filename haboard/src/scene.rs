use std::sync::Arc;

use winit::{
    event::{ElementState, MouseButton, TouchPhase, WindowEvent},
    window::Window,
};

use crate::{drawable::Drawable, engine::Engine, sprite::Sprite, texture::Texture};

// ---------------------------------------------------------------------------
// Public scene mode
// ---------------------------------------------------------------------------

/// Controls which interaction features are active in a [`Scene`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneMode {
    /// Full editor experience:
    /// - Any drawable can be dragged regardless of its [`locked`](Drawable::locked) flag.
    /// - Click-to-select, rubber-band multi-select, and selection halos/tints.
    Edit,

    /// Runtime / playback mode:
    /// - No rubber-band selection and no selection visuals.
    /// - Only drawables whose [`locked`](Drawable::locked) returns `false` can
    ///   be dragged, and they are dragged individually without any tint.
    Run,
}

// ---------------------------------------------------------------------------
// Internal interaction state machine
// ---------------------------------------------------------------------------

#[derive(Default)]
enum InputMode {
    #[default]
    Idle,

    /// One or more drawables are being moved together.
    Dragging {
        /// Cursor position when the drag started.
        start_mouse: (f32, f32),
        /// `(drawable_index, initial_x, initial_y)` for every drawable being
        /// dragged, recorded at the moment the drag began.
        start_positions: Vec<(usize, f32, f32)>,
    },

    /// (Edit mode only) A rubber-band selection rectangle is being drawn.
    Selecting {
        /// The corner where the button was first pressed.
        start: (f32, f32),
    },
}

// ---------------------------------------------------------------------------
// Internal per-frame draw item (no heap allocation, no dynamic dispatch)
// ---------------------------------------------------------------------------

/// A single entry in the per-frame draw list built by [`Scene::render`].
///
/// `User` borrows the caller's drawable directly; `Overlay` owns an
/// internally-generated sprite (selection halo, selection tint, rubber-band).
/// The enum implements [`Drawable`] so the whole list can be passed to
/// [`Engine::render_drawables`] as a concrete `&[DrawItem<T>]` without `dyn`.
enum DrawItem<'a, T> {
    /// A user-supplied drawable, borrowed for this frame only.
    User(&'a T),
    /// An internally-generated overlay sprite.
    Overlay(Sprite),
}

impl<T: Drawable> Drawable for DrawItem<'_, T> {
    fn x(&self) -> f32 {
        match self {
            Self::User(d) => d.x(),
            Self::Overlay(s) => s.x(),
        }
    }
    fn y(&self) -> f32 {
        match self {
            Self::User(d) => d.y(),
            Self::Overlay(s) => s.y(),
        }
    }
    fn width(&self) -> f32 {
        match self {
            Self::User(d) => d.width(),
            Self::Overlay(s) => s.width(),
        }
    }
    fn height(&self) -> f32 {
        match self {
            Self::User(d) => d.height(),
            Self::Overlay(s) => s.height(),
        }
    }
    fn texture(&self) -> &Arc<Texture> {
        match self {
            Self::User(d) => d.texture(),
            Self::Overlay(s) => s.texture(),
        }
    }
    // The render loop only reads draw items; this arm is never reached for
    // the User variant, but must be present to satisfy the trait contract.
    fn set_position(&mut self, x: f32, y: f32) {
        match self {
            Self::User(_) => unreachable!("draw items are read-only during render"),
            Self::Overlay(s) => s.set_position(x, y),
        }
    }
    fn hit_test_point(&self, px: f32, py: f32) -> bool {
        match self {
            Self::User(d) => d.hit_test_point(px, py),
            Self::Overlay(s) => s.hit_test_point(px, py),
        }
    }
    fn hit_test_rect(&self, rx: f32, ry: f32, rw: f32, rh: f32) -> bool {
        match self {
            Self::User(d) => d.hit_test_rect(rx, ry, rw, rh),
            Self::Overlay(s) => s.hit_test_rect(rx, ry, rw, rh),
        }
    }
}

// ---------------------------------------------------------------------------
// Scene
// ---------------------------------------------------------------------------

/// A scene pairs an [`Engine`] with a managed collection of [`Drawable`]
/// objects and owns all interaction logic: dragging, single-click selection,
/// live rubber-band multi-selection, and touch support.
///
/// # Scene mode
/// Pass [`SceneMode::Edit`] or [`SceneMode::Run`] to [`Scene::new`].
/// The mode can also be changed at runtime with [`set_mode`](Scene::set_mode).
///
/// # Drawable type
/// `T` is chosen by the caller:
/// - `Scene<Sprite>` — the common case; no heap allocation per drawable.
/// - `Scene<Box<dyn Drawable>>` — for heterogeneous collections.
/// - Any other type that implements [`Drawable`].
///
/// # Selection state
/// Selection is tracked by the scene in a parallel `Vec<bool>`, not stored on
/// the drawables themselves.  Use [`push_drawable`](Scene::push_drawable)
/// rather than pushing to [`drawables`](Scene::drawables) directly so that the
/// selection vec stays in sync.
pub struct Scene<T> {
    engine: Engine,

    /// The ordered collection of drawables managed by this scene.
    /// Objects are rendered back-to-front; the last entry appears on top.
    ///
    /// Prefer [`push_drawable`](Scene::push_drawable) over pushing here
    /// directly to keep the parallel selection state in sync.
    pub drawables: Vec<T>,

    /// Per-drawable selection flags, parallel to [`drawables`](Scene::drawables).
    /// `selected[i]` is `true` when `drawables[i]` is currently selected.
    /// Only meaningful in [`SceneMode::Edit`].
    selected: Vec<bool>,

    // ── Scene mode ───────────────────────────────────────────────────────────
    scene_mode: SceneMode,

    // ── Internal interaction state ───────────────────────────────────────────
    cursor_pos: (f32, f32),
    input_mode: InputMode,
    /// Touch id of the finger currently driving the interaction, if any.
    primary_touch: Option<u64>,

    // ── Selection visuals (edit mode only) ───────────────────────────────────
    /// Solid-colour texture stretched behind every selected drawable as a halo.
    sel_border_tex: Arc<Texture>,
    /// Semi-transparent texture used for both the selection overlay and the
    /// rubber-band rectangle.
    sel_box_tex: Arc<Texture>,
    /// Thickness in pixels of the selection halo border. Default: `3.0`.
    pub sel_border: f32,
}

impl<T: Drawable> Scene<T> {
    /// Create a new scene in the given [`SceneMode`].
    ///
    /// `engine` must already be initialised (call [`Engine::new`] first so you
    /// can upload textures before constructing the initial drawables).
    pub fn new(engine: Engine, drawables: Vec<T>, mode: SceneMode) -> Self {
        let n = drawables.len();
        // 1×1 pixel textures, stretched at draw time.
        let sel_border_tex = engine.create_texture_from_rgba(&[30, 140, 255, 255], 1, 1);
        let sel_box_tex = engine.create_texture_from_rgba(&[30, 140, 255, 60], 1, 1);

        Self {
            engine,
            selected: vec![false; n],
            drawables,
            scene_mode: mode,
            cursor_pos: (0.0, 0.0),
            input_mode: InputMode::default(),
            primary_touch: None,
            sel_border_tex,
            sel_box_tex,
            sel_border: 3.0,
        }
    }

    /// Add a drawable to the end of the scene, initially unselected.
    ///
    /// Prefer this over `scene.drawables.push(d)` to keep the internal
    /// selection state in sync.
    pub fn push_drawable(&mut self, drawable: T) {
        self.drawables.push(drawable);
        self.selected.push(false);
    }

    /// Return the current [`SceneMode`].
    pub fn mode(&self) -> SceneMode {
        self.scene_mode
    }

    /// Switch the scene between [`SceneMode::Edit`] and [`SceneMode::Run`].
    ///
    /// Switching to `Run` automatically cancels any in-progress interaction
    /// and clears all selection state.
    pub fn set_mode(&mut self, mode: SceneMode) {
        if mode == SceneMode::Run {
            self.selected.fill(false);
            self.input_mode = InputMode::Idle;
        }
        self.scene_mode = mode;
    }

    /// Borrow the underlying [`Engine`] (e.g. to create additional textures).
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Return a reference to the window owned by the engine.
    pub fn window(&self) -> &Window {
        self.engine.window()
    }

    /// Forward a window resize to the engine.
    ///
    /// This is also handled automatically inside
    /// [`handle_window_event`](Scene::handle_window_event).
    pub fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        self.engine.resize(size);
    }

    // ── Event handling ───────────────────────────────────────────────────────

    /// Process a winit [`WindowEvent`].
    ///
    /// Returns `true` when the event was consumed by the interaction layer.
    ///
    /// **Not** handled here (the application keeps responsibility for these):
    /// - `WindowEvent::CloseRequested` — the app decides whether to exit.
    /// - `WindowEvent::RedrawRequested` — call [`render`](Scene::render) yourself.
    pub fn handle_window_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::Resized(size) => {
                self.engine.resize(*size);
                true
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.on_cursor_move(position.x as f32, position.y as f32);
                true
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                match state {
                    ElementState::Pressed => self.on_press(),
                    ElementState::Released => self.on_release(),
                }
                true
            }

            WindowEvent::CursorLeft { .. } => {
                self.input_mode = InputMode::Idle;
                true
            }

            WindowEvent::Touch(touch) => {
                let (tx, ty) = (touch.location.x as f32, touch.location.y as f32);
                match touch.phase {
                    TouchPhase::Started => {
                        if self.primary_touch.is_none() {
                            self.primary_touch = Some(touch.id);
                            self.on_cursor_move(tx, ty);
                            self.on_press();
                        }
                    }
                    TouchPhase::Moved => {
                        if self.primary_touch == Some(touch.id) {
                            self.on_cursor_move(tx, ty);
                        }
                    }
                    TouchPhase::Ended => {
                        if self.primary_touch == Some(touch.id) {
                            self.on_cursor_move(tx, ty);
                            self.on_release();
                            self.primary_touch = None;
                        }
                    }
                    TouchPhase::Cancelled => {
                        if self.primary_touch == Some(touch.id) {
                            self.input_mode = InputMode::Idle;
                            self.primary_touch = None;
                        }
                    }
                }
                true
            }

            _ => false,
        }
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// Render the scene for the current frame.
    ///
    /// **Edit mode** composites (back-to-front):
    /// 1. A blue halo border behind every selected drawable.
    /// 2. The drawable itself.
    /// 3. A semi-transparent blue overlay on top of every selected drawable.
    /// 4. The rubber-band selection rectangle while a drag-select is active.
    ///
    /// **Run mode** renders each drawable as-is with no selection visuals.
    ///
    /// The draw list is a `Vec<DrawItem<T>>` — a concrete type with no heap
    /// allocation per entry and no dynamic dispatch.
    pub fn render(&mut self) {
        let mut draw: Vec<DrawItem<T>> = Vec::with_capacity(self.drawables.len() * 3 + 1);

        match self.scene_mode {
            SceneMode::Edit => {
                let sel_border = self.sel_border;

                for (d, &selected) in self.drawables.iter().zip(self.selected.iter()) {
                    if selected {
                        draw.push(DrawItem::Overlay(Sprite::new(
                            d.x() - sel_border,
                            d.y() - sel_border,
                            d.width() + sel_border * 2.0,
                            d.height() + sel_border * 2.0,
                            Arc::clone(&self.sel_border_tex),
                        )));
                    }
                    draw.push(DrawItem::User(d));
                    if selected {
                        draw.push(DrawItem::Overlay(Sprite::new(
                            d.x(),
                            d.y(),
                            d.width(),
                            d.height(),
                            Arc::clone(&self.sel_box_tex),
                        )));
                    }
                }

                // Rubber-band rectangle on top of everything.
                if let InputMode::Selecting { start } = &self.input_mode {
                    let (sx, sy) = *start;
                    let (cx, cy) = self.cursor_pos;
                    let rw = (cx - sx).abs();
                    let rh = (cy - sy).abs();
                    if rw > 0.0 && rh > 0.0 {
                        draw.push(DrawItem::Overlay(Sprite::new(
                            sx.min(cx),
                            sy.min(cy),
                            rw,
                            rh,
                            Arc::clone(&self.sel_box_tex),
                        )));
                    }
                }
            }

            SceneMode::Run => {
                // Plain rendering — no selection UI of any kind.
                for d in &self.drawables {
                    draw.push(DrawItem::User(d));
                }
            }
        }

        self.engine.render_drawables(&draw);
    }

    // ── Private interaction helpers ──────────────────────────────────────────

    fn on_cursor_move(&mut self, cx: f32, cy: f32) {
        self.cursor_pos = (cx, cy);

        // Drag update — identical in both modes.
        let drag = match &self.input_mode {
            InputMode::Dragging {
                start_mouse,
                start_positions,
            } => Some((*start_mouse, start_positions.clone())),
            _ => None,
        };

        if let Some(((smx, smy), positions)) = drag {
            let (dx, dy) = (cx - smx, cy - smy);
            for (idx, sx, sy) in positions {
                self.drawables[idx].set_position(sx + dx, sy + dy);
            }
        }

        // Rubber-band selection update — edit mode only.
        if self.scene_mode == SceneMode::Edit {
            let sel_rect = match &self.input_mode {
                InputMode::Selecting { start: (sx, sy) } => {
                    let rx = sx.min(cx);
                    let ry = sy.min(cy);
                    Some((rx, ry, (cx - sx).abs(), (cy - sy).abs()))
                }
                _ => None,
            };

            if let Some((rx, ry, rw, rh)) = sel_rect {
                for (d, selected) in self.drawables.iter().zip(self.selected.iter_mut()) {
                    *selected = d.hit_test_rect(rx, ry, rw, rh);
                }
            }
        }
    }

    fn on_press(&mut self) {
        let (mx, my) = self.cursor_pos;

        match self.scene_mode {
            // ── Edit mode ────────────────────────────────────────────────────
            SceneMode::Edit => {
                // Hit-test in reverse draw order so the topmost is picked first.
                let hit = self
                    .drawables
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, d)| d.hit_test_point(mx, my))
                    .map(|(i, _)| (i, self.selected[i]));

                match hit {
                    Some((i, already_selected)) => {
                        if !already_selected {
                            // Deselect everything, select the clicked drawable,
                            // and bring it to the top of the draw stack.
                            self.selected.fill(false);
                            let item = self.drawables.remove(i);
                            self.selected.remove(i);
                            self.drawables.push(item);
                            self.selected.push(true);
                        }
                        // Drag all currently selected drawables together.
                        let start_positions = self
                            .drawables
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| self.selected[*i])
                            .map(|(i, d)| (i, d.x(), d.y()))
                            .collect();
                        self.input_mode = InputMode::Dragging {
                            start_mouse: (mx, my),
                            start_positions,
                        };
                    }
                    None => {
                        self.selected.fill(false);
                        self.input_mode = InputMode::Selecting { start: (mx, my) };
                    }
                }
            }

            // ── Run mode ─────────────────────────────────────────────────────
            SceneMode::Run => {
                // Drag the topmost unlocked drawable; ignore locked ones and
                // empty space (no rubber-band in run mode).
                let hit = self
                    .drawables
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, d)| d.hit_test_point(mx, my) && !d.locked())
                    .map(|(i, _)| i);

                if let Some(i) = hit {
                    let start_positions = vec![(i, self.drawables[i].x(), self.drawables[i].y())];
                    self.input_mode = InputMode::Dragging {
                        start_mouse: (mx, my),
                        start_positions,
                    };
                }
                // No action on locked hits or empty space.
            }
        }
    }

    fn on_release(&mut self) {
        self.input_mode = InputMode::Idle;
    }
}

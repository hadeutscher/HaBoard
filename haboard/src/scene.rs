use std::sync::Arc;

use winit::{
    event::{ElementState, KeyEvent, MouseButton, TouchPhase, WindowEvent},
    keyboard::{Key, NamedKey},
    window::Window,
};

use crate::drawable::Drawable;
use crate::drawables::Drawables;
use crate::engine::{Engine, Quad};
use crate::texture::Texture;

// ---------------------------------------------------------------------------
// Public scene mode
// ---------------------------------------------------------------------------

/// Controls which interaction features are active in a [`Scene`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneMode {
    /// Full editor: click-to-select, rubber-band, group drag.
    Edit,
    /// Playback: no selection UI; only unlocked drawables may be dragged.
    Run,
}

// ---------------------------------------------------------------------------
// Internal interaction state machine
// ---------------------------------------------------------------------------

#[derive(Default)]
enum InputMode {
    #[default]
    Idle,
    Dragging {
        start_mouse: (f32, f32),
        start_positions: Vec<(usize, f32, f32)>,
    },
    Selecting {
        start: (f32, f32),
    },
}

// ---------------------------------------------------------------------------
// Scene
// ---------------------------------------------------------------------------

/// Tint applied to selected sprites in Edit mode.
/// RGBA interpreted as [tint_r, tint_g, tint_b, mix_factor].
const SELECTION_TINT: [f32; 4] = [0.12, 0.55, 1.0, 0.35];
/// No tint — used for unselected sprites and overlay quads.
const NO_TINT: [f32; 4] = [0.0, 0.0, 0.0, 0.0];

/// Pairs an [`Engine`] with a [`Drawables`] collection and owns all interaction
/// logic: dragging, click selection, rubber-band multi-selection, and touch.
pub struct Scene<T: Drawable> {
    engine: Engine,

    /// The drawable collection. Push new drawables here; iterate for save/load.
    pub drawables: Drawables<T>,

    scene_mode: SceneMode,
    cursor_pos: (f32, f32),
    input_mode: InputMode,
    primary_touch: Option<u64>,

    // Overlay texture: semi-transparent blue for the rubber-band rectangle.
    sel_box_tex: Arc<Texture>,
    /// Distance in pixels moved per arrow-key press. Default: `10.0`.
    pub nudge_px: f32,
}

impl<T: Drawable> Scene<T> {
    /// Create a new scene.
    ///
    /// `initial` drawables are uploaded immediately. The scene takes ownership
    /// of the engine.
    pub fn new(engine: Engine, initial: Vec<T>, mode: SceneMode) -> Self {
        let uploader = engine.make_uploader();
        let sel_box_tex = uploader.upload_rgba_bytes(&[30, 140, 255, 60], 1, 1);
        let drawables = Drawables::new(uploader, initial);

        Self {
            engine,
            drawables,
            scene_mode: mode,
            cursor_pos: (0.0, 0.0),
            input_mode: InputMode::default(),
            primary_touch: None,
            sel_box_tex,
            nudge_px: 10.0,
        }
    }

    pub fn mode(&self) -> SceneMode {
        self.scene_mode
    }

    /// Switch mode. Switching to `Run` clears selection and cancels any drag.
    pub fn set_mode(&mut self, mode: SceneMode) {
        if mode == SceneMode::Run {
            for e in &mut self.drawables.entries {
                e.selected = false;
            }
            self.input_mode = InputMode::Idle;
        }
        self.scene_mode = mode;
    }

    pub fn window(&self) -> &Arc<Window> {
        self.engine.window()
    }

    pub fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        self.engine.resize(size);
    }

    // ── Event handling ───────────────────────────────────────────────────────

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
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event),
            _ => false,
        }
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// Render the scene.
    ///
    /// **Pass 1 (back-to-front by Z):** user drawables; selected ones receive a
    /// colour tint that blends with the texture's own RGB while preserving alpha,
    /// so only non-transparent areas appear tinted.
    /// **Pass 2 (always on top):** rubber-band rectangle, if active.
    pub fn render(&mut self) {
        let sorted = self.drawables.z_sorted_indices();
        let edit = self.scene_mode == SceneMode::Edit;

        let mut quads: Vec<Quad<'_>> = Vec::with_capacity(self.drawables.entries.len() + 1);

        for &i in &sorted {
            let e = &self.drawables.entries[i];
            let tint = if edit && e.selected {
                SELECTION_TINT
            } else {
                NO_TINT
            };
            quads.push(Quad {
                x: e.drawable.x(),
                y: e.drawable.y(),
                width: e.drawable.width(),
                height: e.drawable.height(),
                texture: &e.texture,
                tint,
            });
        }

        // Rubber-band rectangle (always on top, edit mode only).
        if edit && let InputMode::Selecting { start: (sx, sy) } = &self.input_mode {
            let (cx, cy) = self.cursor_pos;
            let rw = (cx - sx).abs();
            let rh = (cy - sy).abs();
            if rw > 0.0 && rh > 0.0 {
                quads.push(Quad {
                    x: sx.min(cx),
                    y: sy.min(cy),
                    width: rw,
                    height: rh,
                    texture: &self.sel_box_tex,
                    tint: NO_TINT,
                });
            }
        }

        self.engine.draw_quads(&quads);
    }

    // ── Private interaction helpers ──────────────────────────────────────────

    fn on_cursor_move(&mut self, cx: f32, cy: f32) {
        self.cursor_pos = (cx, cy);

        // Drag update.
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
                self.drawables.entries[idx]
                    .drawable
                    .set_position(sx + dx, sy + dy);
            }
        }

        // Rubber-band selection update (edit mode only).
        if self.scene_mode == SceneMode::Edit
            && let InputMode::Selecting { start: (sx, sy) } = &self.input_mode
        {
            let rx = sx.min(cx);
            let ry = sy.min(cy);
            let rw = (cx - sx).abs();
            let rh = (cy - sy).abs();
            for e in &mut self.drawables.entries {
                e.selected = e.hit_test_rect(rx, ry, rw, rh);
            }
        }
    }

    fn on_press(&mut self) {
        let (mx, my) = self.cursor_pos;

        match self.scene_mode {
            SceneMode::Edit => {
                // Find the topmost hit (highest Z among all entries that hit the cursor).
                let hit = self
                    .drawables
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.hit_test_point(mx, my))
                    .max_by(|(_, a), (_, b)| {
                        a.drawable
                            .z()
                            .partial_cmp(&b.drawable.z())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(i, e)| (i, e.selected));

                match hit {
                    Some((i, already_selected)) => {
                        if !already_selected {
                            for e in &mut self.drawables.entries {
                                e.selected = false;
                            }
                            // Bring to front: assign z = max_z + 1.
                            let new_z = self.drawables.max_z() + 1.0;
                            self.drawables.entries[i].drawable.set_z(new_z);
                            self.drawables.entries[i].selected = true;
                        }
                        // Drag all selected drawables.
                        let start_positions: Vec<(usize, f32, f32)> = self
                            .drawables
                            .entries
                            .iter()
                            .enumerate()
                            .filter(|(_, e)| e.selected)
                            .map(|(i, e)| (i, e.drawable.x(), e.drawable.y()))
                            .collect();
                        self.input_mode = InputMode::Dragging {
                            start_mouse: (mx, my),
                            start_positions,
                        };
                    }
                    None => {
                        for e in &mut self.drawables.entries {
                            e.selected = false;
                        }
                        self.input_mode = InputMode::Selecting { start: (mx, my) };
                    }
                }
            }

            SceneMode::Run => {
                // Drag the topmost unlocked drawable.
                let hit = self
                    .drawables
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.hit_test_point(mx, my) && !e.drawable.locked())
                    .max_by(|(_, a), (_, b)| {
                        a.drawable
                            .z()
                            .partial_cmp(&b.drawable.z())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(i, _)| i);

                if let Some(i) = hit {
                    let (sx, sy) = (
                        self.drawables.entries[i].drawable.x(),
                        self.drawables.entries[i].drawable.y(),
                    );
                    self.input_mode = InputMode::Dragging {
                        start_mouse: (mx, my),
                        start_positions: vec![(i, sx, sy)],
                    };
                }
            }
        }
    }

    fn on_release(&mut self) {
        self.input_mode = InputMode::Idle;
    }

    // ── Keyboard shortcuts (Edit mode only) ──────────────────────────────────────────

    fn on_key(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        if self.scene_mode != SceneMode::Edit {
            return false;
        }
        match &event.logical_key {
            // Escape — deselect all and cancel any in-progress interaction.
            Key::Named(NamedKey::Escape) => {
                for e in &mut self.drawables.entries {
                    e.selected = false;
                }
                self.input_mode = InputMode::Idle;
                true
            }
            // Delete / Backspace — remove all selected drawables (no repeat).
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) if !event.repeat => {
                self.drawables.entries.retain(|e| !e.selected);
                self.input_mode = InputMode::Idle;
                true
            }
            // Arrow keys — nudge selected drawables (repeats while held).
            Key::Named(NamedKey::ArrowLeft) => {
                self.nudge_selected(-self.nudge_px, 0.0);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.nudge_selected(self.nudge_px, 0.0);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.nudge_selected(0.0, -self.nudge_px);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.nudge_selected(0.0, self.nudge_px);
                true
            }
            // +/= — raise Z of selected drawables (repeats while held).
            Key::Character(c) if c == "+" || c == "=" => {
                self.adjust_z_selected(1.0);
                true
            }
            // - — lower Z of selected drawables (repeats while held).
            Key::Character(c) if c == "-" => {
                self.adjust_z_selected(-1.0);
                true
            }
            _ => false,
        }
    }

    fn nudge_selected(&mut self, dx: f32, dy: f32) {
        for e in self.drawables.entries.iter_mut().filter(|e| e.selected) {
            let (x, y) = (e.drawable.x(), e.drawable.y());
            e.drawable.set_position(x + dx, y + dy);
        }
    }

    fn adjust_z_selected(&mut self, delta: f32) {
        for e in self.drawables.entries.iter_mut().filter(|e| e.selected) {
            let z = e.drawable.z();
            e.drawable.set_z(z + delta);
        }
    }
}

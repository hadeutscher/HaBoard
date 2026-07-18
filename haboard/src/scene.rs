use std::{collections::HashMap, sync::Arc};

use winit::{
    event::{ElementState, KeyEvent, MouseButton, TouchPhase, WindowEvent},
    keyboard::{Key, ModifiersState, NamedKey},
    window::Window,
};

use crate::{
    drawable::Drawable,
    drawables::{DrawableEntry, Drawables},
    engine::{Engine, Quad},
    snap::{Rect, snap_delta},
    texture::Texture,
};

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
// Per-finger drag state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TouchDrag {
    /// Screen position where this finger first landed.
    start_touch: (f32, f32),
    /// Start positions of every drawable being moved by this touch.
    /// Holds one entry for a solo drag, or the full selection group when the
    /// touched drawable was already selected.
    /// `(entry_index, start_x, start_y)`
    start_positions: Vec<(usize, f32, f32)>,
}

// ---------------------------------------------------------------------------
// Undo/redo
// ---------------------------------------------------------------------------

/// A single reversible edit to the drawable collection, used to implement
/// undo/redo (Ctrl+Z / Ctrl+Y).
///
/// `Added` and `Removed` are exact duals of each other — inverting one always
/// produces the other — which lets a single [`Scene::invert`] implementation
/// handle both undo and redo. Both carry explicit indices rather than
/// assuming entries stay at the tail, because reinserting a `Removed` batch
/// can land entries at arbitrary (non-contiguous) positions, and a later
/// undo of *that* redo must remove from those same positions.
enum Op<T> {
    /// Drawables moved. `(entry_index, old_x, old_y, new_x, new_y)`.
    Move(Vec<(usize, f32, f32, f32, f32)>),
    /// Entries newly present at these (not necessarily contiguous) indices.
    Added(Vec<usize>),
    /// Entries removed from these indices, paired with the removed data so
    /// they can be reinserted exactly where they were.
    Removed(Vec<(usize, DrawableEntry<T>)>),
}

// ---------------------------------------------------------------------------
// Scene
// ---------------------------------------------------------------------------

/// Tint applied to selected sprites in Edit mode.
/// RGBA interpreted as [tint_r, tint_g, tint_b, mix_factor].
const SELECTION_TINT: [f32; 4] = [0.12, 0.55, 1.0, 0.35];
/// No tint — used for unselected sprites and overlay quads.
const NO_TINT: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
/// Pixel offset applied to each successive Ctrl+V paste, so repeated pastes
/// cascade diagonally instead of stacking exactly on top of each other.
const PASTE_OFFSET: f32 = 20.0;

/// Pairs an [`Engine`] with a [`Drawables`] collection and owns all interaction
/// logic: dragging, click selection, rubber-band multi-selection, and touch.
pub struct Scene<T: Drawable> {
    engine: Engine,

    /// The drawable collection. Push new drawables here; iterate for save/load.
    pub drawables: Drawables<T>,

    scene_mode: SceneMode,
    cursor_pos: (f32, f32),
    input_mode: InputMode,
    /// Per-finger drag state. Each touch point independently drags one
    /// drawable.
    touch_drags: HashMap<u64, TouchDrag>,
    /// Touch ID currently driving rubber-band selection, if any.
    rubber_band_touch: Option<u64>,
    /// Current keyboard modifier state, kept in sync via
    /// [`WindowEvent::ModifiersChanged`].
    modifiers: ModifiersState,
    /// Clipboard for copy/paste (Ctrl+C / Ctrl+V): clones of the drawables
    /// selected at the time of the last copy.
    clipboard: Vec<T>,
    /// Number of times the current clipboard contents have been pasted,
    /// so repeated pastes cascade diagonally instead of stacking exactly on
    /// top of each other.
    paste_count: u32,
    /// Undo history (Ctrl+Z): completed edits, most recent last.
    undo_stack: Vec<Op<T>>,
    /// Redo history (Ctrl+Y): edits undone since the last new edit, most
    /// recently undone last. Cleared whenever a new edit is recorded.
    redo_stack: Vec<Op<T>>,

    // Overlay texture: semi-transparent blue for the rubber-band rectangle.
    sel_box_tex: Arc<Texture>,
    /// Distance in pixels moved per arrow-key press. Default: `10.0`.
    pub nudge_px: f32,
    /// Edge-snap threshold in pixels while dragging: when a dragged object's
    /// edge comes within this distance of another object's edge, it snaps to
    /// align. Set to `0.0` to disable snapping. Default: `10.0`.
    pub snap_px: f32,
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
            touch_drags: HashMap::new(),
            rubber_band_touch: None,
            modifiers: ModifiersState::empty(),
            clipboard: Vec::new(),
            paste_count: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            sel_box_tex,
            nudge_px: 10.0,
            snap_px: 20.0,
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
            self.touch_drags.clear();
            self.rubber_band_touch = None;
        }
        self.scene_mode = mode;
    }

    /// Add a drawable to the scene, recording an undoable "add" operation
    /// (Ctrl+Z removes it again).
    ///
    /// Prefer this over pushing directly via [`drawables`](Self::drawables)
    /// whenever the addition should be undoable.
    pub fn add_drawable(&mut self, drawable: T) {
        let idx = self.drawables.entries.len();
        self.drawables.push(drawable);
        self.record(Op::Added(vec![idx]));
    }

    /// Whether [`undo`](Self::undo) would currently do anything.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether [`redo`](Self::redo) would currently do anything.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Undo the most recently recorded edit (Ctrl+Z), if any.
    pub fn undo(&mut self) {
        let Some(op) = self.undo_stack.pop() else {
            return;
        };
        let redo_op = self.invert(op);
        self.redo_stack.push(redo_op);
        self.input_mode = InputMode::Idle;
        self.touch_drags.clear();
    }

    /// Redo the most recently undone edit (Ctrl+Y), if any.
    pub fn redo(&mut self) {
        let Some(op) = self.redo_stack.pop() else {
            return;
        };
        let undo_op = self.invert(op);
        self.undo_stack.push(undo_op);
        self.input_mode = InputMode::Idle;
        self.touch_drags.clear();
    }

    /// Push `op` onto the undo stack. Any previously undone history is
    /// discarded, since it no longer describes a reachable future state.
    fn record(&mut self, op: Op<T>) {
        self.undo_stack.push(op);
        self.redo_stack.clear();
    }

    /// Apply the inverse of `op` to the drawable collection, returning the
    /// op that would reverse *this* application.
    ///
    /// This single method drives both undo and redo: undoing pops from
    /// `undo_stack`, inverts, and pushes the result onto `redo_stack`;
    /// redoing does the same in the other direction. `Move` is
    /// self-inverting (old/new swapped); `Added`/`Removed` are exact duals
    /// of each other.
    fn invert(&mut self, op: Op<T>) -> Op<T> {
        match op {
            Op::Move(moves) => {
                for &(idx, old_x, old_y, ..) in &moves {
                    self.drawables.entries[idx]
                        .drawable
                        .set_position(old_x, old_y);
                }
                Op::Move(
                    moves
                        .into_iter()
                        .map(|(i, ox, oy, nx, ny)| (i, nx, ny, ox, oy))
                        .collect(),
                )
            }
            Op::Added(mut indices) => {
                // Remove highest index first so removing one never shifts an
                // index still waiting to be removed.
                indices.sort_unstable();
                let removed: Vec<(usize, DrawableEntry<T>)> = indices
                    .iter()
                    .rev()
                    .map(|&idx| (idx, self.drawables.entries.remove(idx)))
                    .collect();
                Op::Removed(removed)
            }
            Op::Removed(mut removed) => {
                // Reinsert lowest index first so inserting one never shifts
                // a target index still waiting to be inserted at.
                removed.sort_by_key(|(idx, _)| *idx);
                let indices = removed.iter().map(|(idx, _)| *idx).collect();
                for (idx, entry) in removed {
                    self.drawables.entries.insert(idx, entry);
                }
                Op::Added(indices)
            }
        }
    }

    pub fn window(&self) -> &Arc<Window> {
        self.engine.window()
    }

    pub fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        self.engine.resize(size);
    }

    /// Current surface size in physical pixels (`width`, `height`).
    pub fn size(&self) -> (u32, u32) {
        self.engine.size()
    }

    /// Release the GPU surface (Android suspend). Rendering is a no-op until
    /// [`recreate_surface`](Self::recreate_surface) is called.
    pub fn drop_surface(&mut self) {
        self.engine.drop_surface();
    }

    /// Recreate the GPU surface for a (possibly new) window after resume.
    pub fn recreate_surface(&mut self, window: Arc<Window>) {
        self.engine.recreate_surface(window);
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
                        match self.find_hit_for_touch(tx, ty) {
                            Some(idx)
                                if self.scene_mode == SceneMode::Edit
                                    && self.modifiers.control_key() =>
                            {
                                // Ctrl+touch: toggle selection, no drag.
                                let was_selected = self.drawables.entries[idx].selected;
                                self.drawables.entries[idx].selected = !was_selected;
                                if !was_selected {
                                    let new_z = self.drawables.max_z() + 1.0;
                                    self.drawables.entries[idx].drawable.set_z(new_z);
                                }
                            }
                            Some(idx) => {
                                // Skip if another finger is already dragging this drawable.
                                let already_claimed = self
                                    .touch_drags
                                    .values()
                                    .any(|d| d.start_positions.iter().any(|&(i, _, _)| i == idx));
                                if !already_claimed {
                                    let start_positions = if self.scene_mode == SceneMode::Edit
                                        && self.drawables.entries[idx].selected
                                    {
                                        // Drag the whole selection group.
                                        self.drawables
                                            .entries
                                            .iter()
                                            .enumerate()
                                            .filter(|(_, e)| e.selected)
                                            .map(|(i, e)| (i, e.drawable.x(), e.drawable.y()))
                                            .collect()
                                    } else {
                                        // Solo drag — bring to front in Edit mode.
                                        if self.scene_mode == SceneMode::Edit {
                                            let new_z = self.drawables.max_z() + 1.0;
                                            self.drawables.entries[idx].drawable.set_z(new_z);
                                        }
                                        vec![(
                                            idx,
                                            self.drawables.entries[idx].drawable.x(),
                                            self.drawables.entries[idx].drawable.y(),
                                        )]
                                    };
                                    self.touch_drags.insert(
                                        touch.id,
                                        TouchDrag {
                                            start_touch: (tx, ty),
                                            start_positions,
                                        },
                                    );
                                }
                            }
                            None if self.scene_mode == SceneMode::Edit
                                && self.rubber_band_touch.is_none()
                                && !self.modifiers.control_key() =>
                            {
                                // Empty space in Edit mode — start rubber-band selection.
                                self.rubber_band_touch = Some(touch.id);
                                self.cursor_pos = (tx, ty);
                                self.on_press();
                            }
                            None => {}
                        }
                    }
                    TouchPhase::Moved => {
                        // Clone positions out before mutably borrowing drawables.
                        let updates = self
                            .touch_drags
                            .get(&touch.id)
                            .map(|d| (d.start_touch, d.start_positions.clone()));
                        if let Some(((stx, sty), positions)) = updates {
                            let (dx, dy) = (tx - stx, ty - sty);
                            let (adjx, adjy) = self.snap_adjustment(&positions, dx, dy);
                            for (idx, sx, sy) in positions {
                                self.drawables.entries[idx]
                                    .drawable
                                    .set_position(sx + dx + adjx, sy + dy + adjy);
                            }
                        } else if self.rubber_band_touch == Some(touch.id) {
                            self.on_cursor_move(tx, ty);
                        }
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        if let Some(drag) = self.touch_drags.remove(&touch.id) {
                            self.record_move(drag.start_positions);
                        }
                        if self.rubber_band_touch == Some(touch.id) {
                            self.on_release();
                            self.rubber_band_touch = None;
                        }
                    }
                }
                true
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event),
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
                true
            }
            _ => false,
        }
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// Render the scene.
    ///
    /// **Pass 1 (back-to-front by Z):** user drawables; selected ones receive a
    /// colour tint that blends with the texture's own RGB while preserving
    /// alpha, so only non-transparent areas appear tinted.
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
            let (adjx, adjy) = self.snap_adjustment(&positions, dx, dy);
            for (idx, sx, sy) in positions {
                self.drawables.entries[idx]
                    .drawable
                    .set_position(sx + dx + adjx, sy + dy + adjy);
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

                if self.modifiers.control_key() {
                    // Ctrl+click: toggle this drawable's selection; no drag.
                    if let Some((i, was_selected)) = hit {
                        self.drawables.entries[i].selected = !was_selected;
                        if !was_selected {
                            // Bring newly selected drawable to front.
                            let new_z = self.drawables.max_z() + 1.0;
                            self.drawables.entries[i].drawable.set_z(new_z);
                        }
                    }
                    // Ctrl+click on empty space: leave selection unchanged.
                } else {
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
        if let InputMode::Dragging {
            start_positions, ..
        } = &self.input_mode
        {
            self.record_move(start_positions.clone());
        }
        self.input_mode = InputMode::Idle;
    }

    /// Compare `start_positions` (captured at drag start) to the drawables'
    /// current positions and record a `Move` op for any that actually moved.
    fn record_move(&mut self, start_positions: Vec<(usize, f32, f32)>) {
        let moves: Vec<(usize, f32, f32, f32, f32)> = start_positions
            .into_iter()
            .filter_map(|(idx, ox, oy)| {
                let d = &self.drawables.entries[idx].drawable;
                let (nx, ny) = (d.x(), d.y());
                (nx != ox || ny != oy).then_some((idx, ox, oy, nx, ny))
            })
            .collect();
        if !moves.is_empty() {
            self.record(Op::Move(moves));
        }
    }

    /// Compute the edge-snap correction for a group drag.
    ///
    /// `moving` holds the dragged entries' `(index, start_x, start_y)` and
    /// `(dx, dy)` is the raw drag delta. Returns `(adjx, adjy)` to add to the
    /// delta so the group snaps as a rigid body to nearby static objects;
    /// `(0, 0)` when snapping is disabled (`snap_px <= 0.0`), Ctrl is not
    /// held, or nothing is within range.
    ///
    /// The correction is computed per member against the other, non-moving
    /// drawables, and the best-fitting member's `(adjx, adjy)` is applied to
    /// the whole group — not the group's outer bounding box, which would
    /// snap based on possibly-empty space at the group's edge rather than
    /// any drawable actually in the group.
    fn snap_adjustment(&self, moving: &[(usize, f32, f32)], dx: f32, dy: f32) -> (f32, f32) {
        if self.snap_px <= 0.0 || moving.is_empty() || !self.modifiers.control_key() {
            return (0.0, 0.0);
        }

        let moving_idx: Vec<usize> = moving.iter().map(|&(idx, _, _)| idx).collect();
        let others: Vec<Rect> = self
            .drawables
            .entries
            .iter()
            .enumerate()
            .filter(|(i, _)| !moving_idx.contains(i))
            .map(|(_, e)| Rect {
                x: e.drawable.x(),
                y: e.drawable.y(),
                w: e.drawable.width(),
                h: e.drawable.height(),
            })
            .collect();

        moving
            .iter()
            .map(|&(idx, sx, sy)| {
                let d = &self.drawables.entries[idx].drawable;
                let item = Rect {
                    x: sx + dx,
                    y: sy + dy,
                    w: d.width(),
                    h: d.height(),
                };
                snap_delta(item, &others, self.snap_px)
            })
            .min_by(|a, b| {
                let mag = |p: &(f32, f32)| p.0 * p.0 + p.1 * p.1;
                mag(a).partial_cmp(&mag(b)).unwrap()
            })
            .unwrap_or((0.0, 0.0))
    }

    /// Find the topmost drawable under a touch point, respecting mode drag
    /// rules.
    ///
    /// In [`SceneMode::Edit`] all drawables are candidates; in
    /// [`SceneMode::Run`] only unlocked ones are.
    fn find_hit_for_touch(&self, tx: f32, ty: f32) -> Option<usize> {
        self.drawables
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.hit_test_point(tx, ty)
                    && match self.scene_mode {
                        SceneMode::Edit => true,
                        SceneMode::Run => !e.drawable.locked(),
                    }
            })
            .max_by(|(_, a), (_, b)| {
                a.drawable
                    .z()
                    .partial_cmp(&b.drawable.z())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
    }

    // ── Keyboard shortcuts (Edit mode only)
    // ──────────────────────────────────────────

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
                let indices: Vec<usize> = self
                    .drawables
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.selected)
                    .map(|(i, _)| i)
                    .collect();
                // Remove highest index first so removing one never shifts an
                // index still waiting to be removed.
                let removed: Vec<(usize, DrawableEntry<T>)> = indices
                    .into_iter()
                    .rev()
                    .map(|i| (i, self.drawables.entries.remove(i)))
                    .collect();
                if !removed.is_empty() {
                    self.record(Op::Removed(removed));
                }
                self.input_mode = InputMode::Idle;
                self.touch_drags.clear(); // entry indices may have shifted after removal
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
            // Ctrl+C — copy selected drawables to the clipboard.
            Key::Character(c) if c == "c" && self.modifiers.control_key() => {
                self.clipboard = self
                    .drawables
                    .entries
                    .iter()
                    .filter(|e| e.selected)
                    .filter_map(|e| e.drawable.try_clone())
                    .collect();
                self.paste_count = 0;
                true
            }
            // Ctrl+V — paste the clipboard, replacing the selection with the
            // newly pasted drawables (no repeat, so holding the key doesn't
            // spawn a pile of copies).
            Key::Character(c) if c == "v" && self.modifiers.control_key() && !event.repeat => {
                if self.clipboard.is_empty() {
                    return false;
                }
                self.paste_count += 1;
                let offset = PASTE_OFFSET * self.paste_count as f32;
                let pasted: Vec<T> = self
                    .clipboard
                    .iter()
                    .filter_map(|d| d.try_clone())
                    .collect();
                for e in &mut self.drawables.entries {
                    e.selected = false;
                }
                let start_idx = self.drawables.entries.len();
                for mut d in pasted {
                    let (x, y) = (d.x(), d.y());
                    d.set_position(x + offset, y + offset);
                    self.drawables.push(d);
                    if let Some(last) = self.drawables.entries.last_mut() {
                        last.selected = true;
                    }
                }
                let added: Vec<usize> = (start_idx..self.drawables.entries.len()).collect();
                if !added.is_empty() {
                    self.record(Op::Added(added));
                }
                true
            }
            // Ctrl+Z — undo the last recorded edit (repeats while held).
            Key::Character(c) if c == "z" && self.modifiers.control_key() => {
                self.undo();
                true
            }
            // Ctrl+Y — redo the last undone edit (repeats while held).
            Key::Character(c) if c == "y" && self.modifiers.control_key() => {
                self.redo();
                true
            }
            _ => false,
        }
    }

    fn nudge_selected(&mut self, dx: f32, dy: f32) {
        let moves: Vec<(usize, f32, f32, f32, f32)> = self
            .drawables
            .entries
            .iter_mut()
            .enumerate()
            .filter(|(_, e)| e.selected)
            .map(|(i, e)| {
                let (ox, oy) = (e.drawable.x(), e.drawable.y());
                let (nx, ny) = (ox + dx, oy + dy);
                e.drawable.set_position(nx, ny);
                (i, ox, oy, nx, ny)
            })
            .collect();
        if !moves.is_empty() {
            self.record(Op::Move(moves));
        }
    }

    fn adjust_z_selected(&mut self, delta: f32) {
        for e in self.drawables.entries.iter_mut().filter(|e| e.selected) {
            let z = e.drawable.z();
            e.drawable.set_z(z + delta);
        }
    }
}

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{
    drawable::Drawable,
    drawables::{DrawableEntry, DrawableId, Drawables},
    engine::{Engine, EngineError, Quad},
    input::{Commit, Input, Key, Modifiers, PointerId, PointerPhase, Response},
    snap::{Rect, group_snap_delta},
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
        start_positions: Vec<(DrawableId, f32, f32)>,
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
    start_positions: Vec<(DrawableId, f32, f32)>,
}

// ---------------------------------------------------------------------------
// Undo/redo
// ---------------------------------------------------------------------------

/// A single reversible edit to the drawable collection, used to implement
/// undo/redo (Ctrl+Z / Ctrl+Y).
///
/// `Added` and `Removed` are exact duals of each other — inverting one always
/// produces the other — which lets a single [`Scene::invert`] implementation
/// handle both undo and redo.
///
/// Entries are named by [`DrawableId`] rather than by position, so that a host
/// removing an unrelated drawable cannot make an outstanding step refer to the
/// wrong one. A step naming an id that no longer exists is skipped. `Removed`
/// additionally carries the position each entry was at, purely so a reinsertion
/// lands where it started; that position is best-effort and clamped, which is
/// harmless because render order comes from Z and not from position.
enum Op<T> {
    /// Drawables moved. `(id, old_x, old_y, new_x, new_y)`.
    Move(Vec<(DrawableId, f32, f32, f32, f32)>),
    /// Entries newly present, most recently added last.
    Added(Vec<DrawableId>),
    /// Entries removed from these positions, paired with the removed data so
    /// they can be reinserted.
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
///
/// `Scene` knows nothing about windowing. Feed it [`Input`] values and it
/// returns a [`Response`] describing what happened; call
/// [`render`](Scene::render) whenever you want a frame. A host that owns its
/// own event loop needs nothing else. With the `winit` feature,
/// [`handle_window_event`](Scene::handle_window_event) accepts winit events
/// directly and [`SceneRunner`](crate::SceneRunner) drives the whole lifecycle.
///
/// # Coordinates
/// Everything is in **physical pixels**, matching [`Engine`].
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
    /// Current keyboard modifier state, kept in sync via [`Input::Modifiers`].
    modifiers: Modifiers,
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
    /// Set when the scene's appearance has changed since the last render.
    dirty: bool,
    /// Set when selection changed while handling the current input.
    selection_dirty: bool,
    /// Set when a change was reported as [`Commit::Defer`] and no
    /// [`Commit::Now`] has flushed it yet.
    pending_commit: bool,

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
            modifiers: Modifiers::NONE,
            clipboard: Vec::new(),
            paste_count: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            dirty: true,
            selection_dirty: false,
            pending_commit: false,
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
            self.deselect_all();
            self.input_mode = InputMode::Idle;
            self.touch_drags.clear();
            self.rubber_band_touch = None;
        }
        self.scene_mode = mode;
        self.dirty = true;
    }

    /// Current keyboard modifier state, as last reported by
    /// [`Input::Modifiers`].
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Last known pointer position in physical pixels.
    ///
    /// Useful for placing something where the user last pointed — a dropped
    /// image, a context menu.
    pub fn cursor(&self) -> (f32, f32) {
        self.cursor_pos
    }

    // ── Contents ─────────────────────────────────────────────────────────────

    /// Add a drawable to the scene, recording an undoable "add" operation
    /// (Ctrl+Z removes it again).
    ///
    /// Prefer this over pushing directly via [`drawables`](Self::drawables)
    /// whenever the addition should be undoable.
    pub fn add_drawable(&mut self, drawable: T) -> DrawableId {
        let id = self.drawables.push(drawable);
        self.record(Op::Added(vec![id]));
        self.dirty = true;
        id
    }

    /// Remove a drawable, recording an undoable "remove" operation.
    ///
    /// Returns `false` if no such drawable exists. The exact dual of
    /// [`add_drawable`](Self::add_drawable); to remove without touching the
    /// history, use [`Drawables::remove`] instead.
    pub fn remove_drawable(&mut self, id: DrawableId) -> bool {
        let Some(index) = self.drawables.index_of(id) else {
            return false;
        };
        let entry = self.drawables.entries.remove(index);
        self.record(Op::Removed(vec![(index, entry)]));
        self.forget_interactions_with(id);
        self.dirty = true;
        true
    }

    /// Replace the scene's entire contents.
    ///
    /// Undo and redo history are cleared: this is a wholesale content
    /// replacement, so steps describing the previous contents no longer
    /// describe a state the user could reach. Any in-progress drag or selection
    /// is cancelled for the same reason, and so is an outstanding
    /// [`Commit::Defer`] — the change it owed was to contents that no longer
    /// exist, so flushing it later would persist the wrong thing.
    pub fn set_items(&mut self, items: Vec<T>) {
        self.drawables.clear();
        for item in items {
            self.drawables.push(item);
        }
        self.clear_history();
        self.input_mode = InputMode::Idle;
        self.touch_drags.clear();
        self.rubber_band_touch = None;
        self.pending_commit = false;
        self.dirty = true;
    }

    // ── History ──────────────────────────────────────────────────────────────

    /// Whether [`undo`](Self::undo) would currently do anything.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether [`redo`](Self::redo) would currently do anything.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Discard all undo and redo history.
    ///
    /// Worth calling after mutating [`drawables`](Self::drawables) directly in
    /// a way that outstanding history should not survive — most notably
    /// [`Drawables::clear`], after which an undo could otherwise reinsert
    /// entries the host had discarded.
    pub fn clear_history(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    /// Undo the most recently recorded edit (Ctrl+Z), if any.
    pub fn undo(&mut self) -> bool {
        let Some(op) = self.undo_stack.pop() else {
            return false;
        };
        let redo_op = self.invert(op);
        self.redo_stack.push(redo_op);
        self.input_mode = InputMode::Idle;
        self.touch_drags.clear();
        self.dirty = true;
        true
    }

    /// Redo the most recently undone edit (Ctrl+Y), if any.
    pub fn redo(&mut self) -> bool {
        let Some(op) = self.redo_stack.pop() else {
            return false;
        };
        let undo_op = self.invert(op);
        self.undo_stack.push(undo_op);
        self.input_mode = InputMode::Idle;
        self.touch_drags.clear();
        self.dirty = true;
        true
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
    ///
    /// Ids that no longer resolve are skipped rather than treated as an error:
    /// the host is free to remove drawables while history is outstanding, and
    /// the worst that step can then do is nothing.
    fn invert(&mut self, op: Op<T>) -> Op<T> {
        match op {
            Op::Move(moves) => {
                for &(id, old_x, old_y, ..) in &moves {
                    if let Some(index) = self.drawables.index_of(id) {
                        self.drawables.entries[index]
                            .drawable
                            .set_position(old_x, old_y);
                    }
                }
                Op::Move(
                    moves
                        .into_iter()
                        .map(|(id, ox, oy, nx, ny)| (id, nx, ny, ox, oy))
                        .collect(),
                )
            }
            Op::Added(ids) => {
                // Resolve every id to its current position first, then remove
                // highest index first so removing one never shifts an index
                // still waiting to be removed.
                let mut located: Vec<(usize, DrawableId)> = ids
                    .iter()
                    .filter_map(|&id| self.drawables.index_of(id).map(|index| (index, id)))
                    .collect();
                located.sort_unstable_by_key(|&(index, _)| index);
                let removed: Vec<(usize, DrawableEntry<T>)> = located
                    .iter()
                    .rev()
                    .map(|&(index, _)| (index, self.drawables.entries.remove(index)))
                    .collect();
                for &(_, id) in &located {
                    self.forget_interactions_with(id);
                }
                Op::Removed(removed)
            }
            Op::Removed(mut removed) => {
                // Reinsert lowest index first so inserting one never shifts
                // a target index still waiting to be inserted at.
                removed.sort_by_key(|(index, _)| *index);
                let ids = removed.iter().map(|(_, entry)| entry.id).collect();
                for (index, entry) in removed {
                    // Clamped: an unrelated host-side removal may have left
                    // this position past the end.
                    let index = index.min(self.drawables.entries.len());
                    self.drawables.entries.insert(index, entry);
                }
                Op::Added(ids)
            }
        }
    }

    // ── Surface ──────────────────────────────────────────────────────────────

    /// Resize the surface, in physical pixels.
    pub fn resize(&mut self, size: (u32, u32)) {
        self.engine.resize(size);
        self.dirty = true;
    }

    /// Current surface size in physical pixels (`width`, `height`).
    pub fn size(&self) -> (u32, u32) {
        self.engine.size()
    }

    /// Borrow the underlying engine, e.g. to set
    /// [`clear_color`](Engine::clear_color).
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Mutably borrow the underlying engine.
    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// Release the GPU surface (Android suspend, or a host unmounting the
    /// element it renders into). Rendering is a no-op until
    /// [`recreate_surface`](Self::recreate_surface) is called.
    pub fn drop_surface(&mut self) {
        self.engine.drop_surface();
    }

    /// Recreate the GPU surface against a (possibly different) target.
    ///
    /// Every uploaded texture and the whole scene survive, so this is how a
    /// host remounts its canvas or window without rebuilding anything.
    pub fn recreate_surface(
        &mut self,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        size: (u32, u32),
    ) -> Result<(), EngineError> {
        self.engine.recreate_surface(target, size)?;
        self.dirty = true;
        Ok(())
    }

    /// Recreate the GPU surface against an HTML canvas.
    ///
    /// See [`Engine::recreate_surface_from_canvas`]. This is the call a web host
    /// makes when the element it renders into has been unmounted and remounted.
    #[cfg(target_arch = "wasm32")]
    pub fn recreate_surface_from_canvas(
        &mut self,
        canvas: web_sys::HtmlCanvasElement,
        size: (u32, u32),
    ) -> Result<(), EngineError> {
        self.engine.recreate_surface_from_canvas(canvas, size)?;
        self.dirty = true;
        Ok(())
    }

    // ── Frame scheduling ─────────────────────────────────────────────────────

    /// Whether the scene's appearance has changed since the last
    /// [`render`](Self::render).
    ///
    /// Advisory — see [`Response::redraw_needed`]. Rendering every frame
    /// regardless is always correct.
    pub fn needs_redraw(&self) -> bool {
        self.dirty
    }

    /// Mark the scene as needing a redraw.
    ///
    /// Call this after mutating [`drawables`](Self::drawables) directly, which
    /// the scene cannot observe.
    pub fn request_redraw(&mut self) {
        self.dirty = true;
    }

    /// Whether a [`Commit::Defer`] is still outstanding, clearing the flag.
    ///
    /// A deferred run is normally flushed by the key release that ends it. Ask
    /// this when tearing the scene down — or on losing focus — so that a run
    /// interrupted before its release is still persisted.
    pub fn take_pending_commit(&mut self) -> bool {
        std::mem::take(&mut self.pending_commit)
    }

    // ── Event handling ───────────────────────────────────────────────────────

    /// Handle one input event.
    ///
    /// See [`Response`] for what the return value reports.
    pub fn handle(&mut self, input: Input) -> Response {
        self.selection_dirty = false;
        let mut response = match input {
            Input::Resize { width, height } => {
                self.resize((width, height));
                Response::HANDLED
            }
            Input::Modifiers(modifiers) => {
                self.modifiers = modifiers;
                Response {
                    handled: true,
                    ..Response::IGNORED
                }
            }
            Input::Pointer {
                id: PointerId::Mouse,
                phase,
                x,
                y,
            } => self.on_mouse(phase, x, y),
            Input::Pointer {
                id: PointerId::Touch(touch),
                phase,
                x,
                y,
            } => self.on_touch(touch, phase, x, y),
            Input::Key {
                key,
                pressed,
                repeat,
            } => self.on_key(key, pressed, repeat),
        };

        response.selection_changed = self.selection_dirty;
        if response.selection_changed {
            response.redraw_needed = true;
        }
        if response.redraw_needed {
            self.dirty = true;
        }
        match response.commit {
            Commit::Defer => self.pending_commit = true,
            Commit::Now => self.pending_commit = false,
            Commit::No => {}
        }
        response
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
        self.dirty = false;
    }

    // ── Selection helpers ────────────────────────────────────────────────────

    /// Set an entry's selection flag, noting whether it actually changed.
    fn set_selected_at(&mut self, index: usize, selected: bool) {
        let entry = &mut self.drawables.entries[index];
        if entry.selected != selected {
            entry.selected = selected;
            self.selection_dirty = true;
        }
    }

    fn deselect_all(&mut self) {
        for e in &mut self.drawables.entries {
            if e.selected {
                e.selected = false;
                self.selection_dirty = true;
            }
        }
    }

    /// Drop any in-progress interaction referring to `id`, so a removal cannot
    /// leave a drag pointing at something that is gone.
    fn forget_interactions_with(&mut self, id: DrawableId) {
        if let InputMode::Dragging {
            start_positions, ..
        } = &mut self.input_mode
        {
            start_positions.retain(|&(member, _, _)| member != id);
            if start_positions.is_empty() {
                self.input_mode = InputMode::Idle;
            }
        }
        self.touch_drags.retain(|_, drag| {
            drag.start_positions.retain(|&(member, _, _)| member != id);
            !drag.start_positions.is_empty()
        });
    }

    // ── Private interaction helpers ──────────────────────────────────────────

    fn on_mouse(&mut self, phase: PointerPhase, x: f32, y: f32) -> Response {
        match phase {
            PointerPhase::Move => {
                self.on_cursor_move(x, y);
                Response::HANDLED
            }
            PointerPhase::Down => {
                self.cursor_pos = (x, y);
                self.on_press();
                Response::HANDLED
            }
            // A cancel is a release at the current position: by now the
            // drawables have visibly moved, and haboard has nothing to revert
            // to, so dropping the change would leave the host's saved state
            // disagreeing with the screen.
            PointerPhase::Up | PointerPhase::Cancel => {
                self.cursor_pos = (x, y);
                let committed = self.on_release();
                Response {
                    handled: true,
                    redraw_needed: true,
                    commit: if committed { Commit::Now } else { Commit::No },
                    selection_changed: false,
                }
            }
        }
    }

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
            self.apply_drag(&positions, dx, dy);
        }

        // Rubber-band selection update (edit mode only).
        if self.scene_mode == SceneMode::Edit
            && let InputMode::Selecting { start: (sx, sy) } = &self.input_mode
        {
            let (sx, sy) = (*sx, *sy);
            let rx = sx.min(cx);
            let ry = sy.min(cy);
            let rw = (cx - sx).abs();
            let rh = (cy - sy).abs();
            for i in 0..self.drawables.entries.len() {
                let hit = self.drawables.entries[i].hit_test_rect(rx, ry, rw, rh);
                self.set_selected_at(i, hit);
            }
        }
    }

    /// Move a captured drag group by `(dx, dy)`, applying edge snapping.
    fn apply_drag(&mut self, positions: &[(DrawableId, f32, f32)], dx: f32, dy: f32) {
        let (adjx, adjy) = self.snap_adjustment(positions, dx, dy);
        for &(id, sx, sy) in positions {
            if let Some(index) = self.drawables.index_of(id) {
                self.drawables.entries[index]
                    .drawable
                    .set_position(sx + dx + adjx, sy + dy + adjy);
            }
        }
    }

    /// The topmost entry index under `(mx, my)`, respecting mode drag rules.
    ///
    /// In [`SceneMode::Edit`] all drawables are candidates; in
    /// [`SceneMode::Run`] only unlocked ones are.
    fn hit_at(&self, mx: f32, my: f32) -> Option<usize> {
        self.drawables
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.hit_test_point(mx, my)
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

    /// Capture the current positions of every selected drawable, for a group
    /// drag.
    fn selected_start_positions(&self) -> Vec<(DrawableId, f32, f32)> {
        self.drawables
            .entries
            .iter()
            .filter(|e| e.selected)
            .map(|e| (e.id, e.drawable.x(), e.drawable.y()))
            .collect()
    }

    /// Raise an entry above everything else.
    fn bring_to_front(&mut self, index: usize) {
        let new_z = self.drawables.max_z() + 1.0;
        self.drawables.entries[index].drawable.set_z(new_z);
    }

    fn on_press(&mut self) {
        let (mx, my) = self.cursor_pos;
        let hit = self.hit_at(mx, my);

        match self.scene_mode {
            SceneMode::Edit => {
                if self.modifiers.ctrl {
                    // Ctrl+click: toggle this drawable's selection; no drag.
                    // Ctrl+click on empty space: leave selection unchanged.
                    if let Some(index) = hit {
                        let was_selected = self.drawables.entries[index].selected;
                        self.set_selected_at(index, !was_selected);
                        if !was_selected {
                            self.bring_to_front(index);
                        }
                    }
                    return;
                }

                match hit {
                    Some(index) => {
                        if !self.drawables.entries[index].selected {
                            self.deselect_all();
                            self.bring_to_front(index);
                            self.set_selected_at(index, true);
                        }
                        // Drag all selected drawables.
                        self.input_mode = InputMode::Dragging {
                            start_mouse: (mx, my),
                            start_positions: self.selected_start_positions(),
                        };
                    }
                    None => {
                        self.deselect_all();
                        self.input_mode = InputMode::Selecting { start: (mx, my) };
                    }
                }
            }

            SceneMode::Run => {
                // Drag the topmost unlocked drawable.
                if let Some(index) = hit {
                    let e = &self.drawables.entries[index];
                    let start = (e.id, e.drawable.x(), e.drawable.y());
                    self.input_mode = InputMode::Dragging {
                        start_mouse: (mx, my),
                        start_positions: vec![start],
                    };
                }
            }
        }
    }

    /// Finish a mouse interaction. Returns whether anything was recorded.
    fn on_release(&mut self) -> bool {
        let committed = if let InputMode::Dragging {
            start_positions, ..
        } = &self.input_mode
        {
            let positions = start_positions.clone();
            self.record_move(positions)
        } else {
            false
        };
        self.input_mode = InputMode::Idle;
        committed
    }

    // ── Touch ────────────────────────────────────────────────────────────────

    fn on_touch(&mut self, touch: u64, phase: PointerPhase, tx: f32, ty: f32) -> Response {
        match phase {
            PointerPhase::Down => {
                self.on_touch_down(touch, tx, ty);
                Response::HANDLED
            }
            PointerPhase::Move => {
                self.on_touch_move(touch, tx, ty);
                Response::HANDLED
            }
            PointerPhase::Up | PointerPhase::Cancel => {
                let mut committed = false;
                if let Some(drag) = self.touch_drags.remove(&touch) {
                    committed = self.record_move(drag.start_positions);
                }
                if self.rubber_band_touch == Some(touch) {
                    self.cursor_pos = (tx, ty);
                    committed |= self.on_release();
                    self.rubber_band_touch = None;
                }
                Response {
                    handled: true,
                    redraw_needed: true,
                    commit: if committed { Commit::Now } else { Commit::No },
                    selection_changed: false,
                }
            }
        }
    }

    fn on_touch_down(&mut self, touch: u64, tx: f32, ty: f32) {
        match self.hit_at(tx, ty) {
            Some(index) if self.scene_mode == SceneMode::Edit && self.modifiers.ctrl => {
                // Ctrl+touch: toggle selection, no drag.
                let was_selected = self.drawables.entries[index].selected;
                self.set_selected_at(index, !was_selected);
                if !was_selected {
                    self.bring_to_front(index);
                }
            }
            Some(index) => {
                // Skip if another finger is already dragging this drawable.
                let id = self.drawables.entries[index].id;
                let already_claimed = self
                    .touch_drags
                    .values()
                    .any(|d| d.start_positions.iter().any(|&(member, _, _)| member == id));
                if already_claimed {
                    return;
                }
                let start_positions = if self.scene_mode == SceneMode::Edit
                    && self.drawables.entries[index].selected
                {
                    // Drag the whole selection group.
                    self.selected_start_positions()
                } else {
                    // Solo drag — bring to front in Edit mode.
                    if self.scene_mode == SceneMode::Edit {
                        self.bring_to_front(index);
                    }
                    let e = &self.drawables.entries[index];
                    vec![(e.id, e.drawable.x(), e.drawable.y())]
                };
                self.touch_drags.insert(
                    touch,
                    TouchDrag {
                        start_touch: (tx, ty),
                        start_positions,
                    },
                );
            }
            None if self.scene_mode == SceneMode::Edit
                && self.rubber_band_touch.is_none()
                && !self.modifiers.ctrl =>
            {
                // Empty space in Edit mode — start rubber-band selection.
                self.rubber_band_touch = Some(touch);
                self.cursor_pos = (tx, ty);
                self.on_press();
            }
            None => {}
        }
    }

    fn on_touch_move(&mut self, touch: u64, tx: f32, ty: f32) {
        // Clone positions out before mutably borrowing drawables.
        let updates = self
            .touch_drags
            .get(&touch)
            .map(|d| (d.start_touch, d.start_positions.clone()));
        if let Some(((stx, sty), positions)) = updates {
            self.apply_drag(&positions, tx - stx, ty - sty);
        } else if self.rubber_band_touch == Some(touch) {
            self.on_cursor_move(tx, ty);
        }
    }

    /// Compare `start_positions` (captured at drag start) to the drawables'
    /// current positions and record a `Move` op for any that actually moved.
    ///
    /// Returns whether anything was recorded, which is what distinguishes a
    /// drag worth persisting from a bare click.
    fn record_move(&mut self, start_positions: Vec<(DrawableId, f32, f32)>) -> bool {
        let moves: Vec<(DrawableId, f32, f32, f32, f32)> = start_positions
            .into_iter()
            .filter_map(|(id, ox, oy)| {
                let index = self.drawables.index_of(id)?;
                let d = &self.drawables.entries[index].drawable;
                let (nx, ny) = (d.x(), d.y());
                (nx != ox || ny != oy).then_some((id, ox, oy, nx, ny))
            })
            .collect();
        if moves.is_empty() {
            return false;
        }
        self.record(Op::Move(moves));
        true
    }

    /// Compute the edge-snap correction for a group drag.
    ///
    /// `moving` holds the dragged entries' `(id, start_x, start_y)` and
    /// `(dx, dy)` is the raw drag delta. Returns `(adjx, adjy)` to add to the
    /// delta so the group snaps as a rigid body to nearby static objects;
    /// `(0, 0)` when snapping is disabled (`snap_px <= 0.0`), Ctrl is not
    /// held, or nothing is within range.
    ///
    /// The correction is computed per member against the other, non-moving
    /// drawables — never the group's outer bounding box, which would snap
    /// based on possibly-empty space at the group's edge rather than any
    /// drawable actually in the group. See [`group_snap_delta`] for how the
    /// members' individual corrections combine into one rigid-body shift.
    fn snap_adjustment(&self, moving: &[(DrawableId, f32, f32)], dx: f32, dy: f32) -> (f32, f32) {
        if self.snap_px <= 0.0 || moving.is_empty() || !self.modifiers.ctrl {
            return (0.0, 0.0);
        }

        let moving_ids: HashSet<DrawableId> = moving.iter().map(|&(id, _, _)| id).collect();
        let others: Vec<Rect> = self
            .drawables
            .entries
            .iter()
            .filter(|e| !moving_ids.contains(&e.id))
            .map(|e| Rect {
                x: e.drawable.x(),
                y: e.drawable.y(),
                w: e.drawable.width(),
                h: e.drawable.height(),
            })
            .collect();

        // Group members at their dragged-but-un-snapped positions, in the
        // order they were captured at drag start.
        let group: Vec<Rect> = moving
            .iter()
            .filter_map(|&(id, sx, sy)| {
                let index = self.drawables.index_of(id)?;
                let d = &self.drawables.entries[index].drawable;
                Some(Rect {
                    x: sx + dx,
                    y: sy + dy,
                    w: d.width(),
                    h: d.height(),
                })
            })
            .collect();

        group_snap_delta(&group, &others, self.snap_px)
    }

    // ── Keyboard shortcuts (Edit mode only) ──────────────────────────────────

    fn on_key(&mut self, key: Key, pressed: bool, repeat: bool) -> Response {
        if !pressed {
            // A release ends any run of auto-repeats, flushing whatever they
            // deferred. Nothing is lost by having waited: the deferred changes
            // are already applied to the scene, only the host's notification
            // was held back.
            if self.pending_commit {
                return Response {
                    handled: false,
                    redraw_needed: false,
                    commit: Commit::Now,
                    selection_changed: false,
                };
            }
            return Response::IGNORED;
        }
        if self.scene_mode != SceneMode::Edit {
            return Response::IGNORED;
        }

        // An auto-repeat is a change that is still in progress, so it is
        // reported as deferred and flushed by the release above. Without this,
        // a held arrow key asks the host to persist tens of times a second.
        let commit_now = if repeat { Commit::Defer } else { Commit::Now };

        match key {
            // Escape — deselect all and cancel any in-progress interaction.
            Key::Escape => {
                self.deselect_all();
                self.input_mode = InputMode::Idle;
                Response::HANDLED
            }
            // Delete / Backspace — remove all selected drawables (no repeat).
            Key::Delete | Key::Backspace if !repeat => {
                let indices: Vec<usize> = self
                    .drawables
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.selected)
                    .map(|(i, _)| i)
                    .collect();
                if indices.is_empty() {
                    return Response::IGNORED;
                }
                // Remove highest index first so removing one never shifts an
                // index still waiting to be removed.
                let removed: Vec<(usize, DrawableEntry<T>)> = indices
                    .into_iter()
                    .rev()
                    .map(|i| (i, self.drawables.entries.remove(i)))
                    .collect();
                for (_, entry) in &removed {
                    let id = entry.id;
                    self.forget_interactions_with(id);
                }
                self.record(Op::Removed(removed));
                self.input_mode = InputMode::Idle;
                self.selection_dirty = true;
                Response {
                    handled: true,
                    redraw_needed: true,
                    commit: Commit::Now,
                    selection_changed: false,
                }
            }
            // Arrow keys — nudge selected drawables (repeats while held).
            Key::ArrowLeft => self.nudge_selected(-self.nudge_px, 0.0, commit_now),
            Key::ArrowRight => self.nudge_selected(self.nudge_px, 0.0, commit_now),
            Key::ArrowUp => self.nudge_selected(0.0, -self.nudge_px, commit_now),
            Key::ArrowDown => self.nudge_selected(0.0, self.nudge_px, commit_now),
            // +/= — raise Z of selected drawables (repeats while held).
            Key::Character('+') | Key::Character('=') => self.adjust_z_selected(1.0, commit_now),
            // - — lower Z of selected drawables (repeats while held).
            Key::Character('-') => self.adjust_z_selected(-1.0, commit_now),
            // Ctrl+C — copy selected drawables to the clipboard.
            Key::Character('c') if self.modifiers.ctrl => {
                self.clipboard = self
                    .drawables
                    .entries
                    .iter()
                    .filter(|e| e.selected)
                    .filter_map(|e| e.drawable.try_clone())
                    .collect();
                self.paste_count = 0;
                Response {
                    handled: true,
                    ..Response::IGNORED
                }
            }
            // Ctrl+V — paste the clipboard, replacing the selection with the
            // newly pasted drawables (no repeat, so holding the key doesn't
            // spawn a pile of copies).
            Key::Character('v') if self.modifiers.ctrl && !repeat => {
                if self.clipboard.is_empty() {
                    return Response::IGNORED;
                }
                self.paste_count += 1;
                let offset = PASTE_OFFSET * self.paste_count as f32;
                let pasted: Vec<T> = self
                    .clipboard
                    .iter()
                    .filter_map(|d| d.try_clone())
                    .collect();
                self.deselect_all();
                let mut added: Vec<DrawableId> = Vec::with_capacity(pasted.len());
                for mut d in pasted {
                    let (x, y) = (d.x(), d.y());
                    d.set_position(x + offset, y + offset);
                    let id = self.drawables.push(d);
                    added.push(id);
                    if let Some(last) = self.drawables.entries.last_mut() {
                        last.selected = true;
                    }
                }
                if added.is_empty() {
                    return Response::IGNORED;
                }
                self.record(Op::Added(added));
                self.selection_dirty = true;
                Response {
                    handled: true,
                    redraw_needed: true,
                    commit: Commit::Now,
                    selection_changed: false,
                }
            }
            // Ctrl+Z — undo the last recorded edit (repeats while held).
            Key::Character('z') if self.modifiers.ctrl => {
                if !self.undo() {
                    return Response::IGNORED;
                }
                self.selection_dirty = true;
                Response {
                    handled: true,
                    redraw_needed: true,
                    commit: commit_now,
                    selection_changed: false,
                }
            }
            // Ctrl+Y — redo the last undone edit (repeats while held).
            Key::Character('y') if self.modifiers.ctrl => {
                if !self.redo() {
                    return Response::IGNORED;
                }
                self.selection_dirty = true;
                Response {
                    handled: true,
                    redraw_needed: true,
                    commit: commit_now,
                    selection_changed: false,
                }
            }
            _ => Response::IGNORED,
        }
    }

    fn nudge_selected(&mut self, dx: f32, dy: f32, commit: Commit) -> Response {
        let moves: Vec<(DrawableId, f32, f32, f32, f32)> = self
            .drawables
            .entries
            .iter_mut()
            .filter(|e| e.selected)
            .map(|e| {
                let (ox, oy) = (e.drawable.x(), e.drawable.y());
                let (nx, ny) = (ox + dx, oy + dy);
                e.drawable.set_position(nx, ny);
                (e.id, ox, oy, nx, ny)
            })
            .collect();
        if moves.is_empty() {
            return Response::IGNORED;
        }
        self.record(Op::Move(moves));
        Response {
            handled: true,
            redraw_needed: true,
            commit,
            selection_changed: false,
        }
    }

    fn adjust_z_selected(&mut self, delta: f32, commit: Commit) -> Response {
        let mut changed = false;
        for e in self.drawables.entries.iter_mut().filter(|e| e.selected) {
            let z = e.drawable.z();
            e.drawable.set_z(z + delta);
            changed = true;
        }
        if !changed {
            return Response::IGNORED;
        }
        Response {
            handled: true,
            redraw_needed: true,
            commit,
            selection_changed: false,
        }
    }
}

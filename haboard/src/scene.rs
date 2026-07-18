use std::{collections::HashMap, sync::Arc};

use winit::{
    event::{ElementState, KeyEvent, MouseButton, TouchPhase, WindowEvent},
    keyboard::{Key, ModifiersState, NamedKey},
    window::Window,
};

use crate::{
    drawable::Drawable,
    drawables::Drawables,
    engine::{Engine, Quad},
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

// ---------------------------------------------------------------------------
// Edge snapping
// ---------------------------------------------------------------------------

/// Axis-aligned rectangle used for edge-snap calculations.
#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn left(&self) -> f32 {
        self.x
    }
    fn right(&self) -> f32 {
        self.x + self.w
    }
    fn top(&self) -> f32 {
        self.y
    }
    fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// Compute the `(dx, dy)` correction that snaps `moving`'s edges to the nearest
/// edge of any rect in `others`, considering only corrections no larger than
/// `threshold` in absolute value.
///
/// Candidates are only ever combined when doing so leaves each contributing
/// object genuinely touched at the *final*, fully-corrected position — never
/// because each looked touched in isolation:
///
/// - A pure single-axis snap needs `moving` to genuinely share a span with that
///   object on the *other* axis (real overlap, or an exact boundary touch) —
///   otherwise the two would not actually meet along the corrected edge.
/// - A same-object two-axis snap is allowed when closing the gap on one axis
///   (an adjacency correction) newly brings that same object into a real shared
///   span on the other axis, unlocking a further alignment refinement against
///   it (e.g. closing an X gap then also aligning tops, once the two share real
///   vertical extent). This refinement is always taken when available, so a
///   smaller, less-aligned correction to the same object never outranks it.
/// - An object sharing no span with `moving` on *either* axis may still offer a
///   **corner snap**: both axes close to exactly zero against that single
///   object.
/// - Two *different* objects can also combine — e.g. nestling into the concave
///   corner formed by two partially-overlapping rects, touching one on X and
///   the other on Y — but only when each object's own correction remains valid
///   (a real shared span) after applying the *other* object's correction too.
///   Two objects that each merely happen to be within `threshold` on their own
///   axis, without this final position actually touching either of them, are
///   rejected.
///
/// Among all valid candidates, the one with the smallest magnitude wins.
fn snap_delta(moving: Rect, others: &[Rect], threshold: f32) -> (f32, f32) {
    // Real span overlap: a shared boundary point counts (`<=`), a genuine
    // gap does not.
    fn overlaps(a_lo: f32, a_hi: f32, b_lo: f32, b_hi: f32) -> bool {
        a_lo <= b_hi && b_lo <= a_hi
    }
    // Smallest-magnitude candidate within `threshold`, if any.
    fn best_of(cands: [f32; 4], threshold: f32) -> Option<f32> {
        cands
            .into_iter()
            .filter(|c| c.abs() <= threshold)
            .min_by(|a: &f32, b: &f32| a.abs().partial_cmp(&b.abs()).unwrap())
    }
    // Horizontal correction against `o`, valid only when `o` shares the
    // vertical span `[y_lo, y_hi]`.
    fn dx_for(moving: Rect, o: &Rect, y_lo: f32, y_hi: f32, threshold: f32) -> Option<f32> {
        overlaps(y_lo, y_hi, o.top(), o.bottom()).then(|| {
            best_of(
                [
                    o.left() - moving.left(),
                    o.right() - moving.right(),
                    o.left() - moving.right(),
                    o.right() - moving.left(),
                ],
                threshold,
            )
        })?
    }
    // Vertical correction against `o`, valid only when `o` shares the
    // horizontal span `[x_lo, x_hi]`.
    fn dy_for(moving: Rect, o: &Rect, x_lo: f32, x_hi: f32, threshold: f32) -> Option<f32> {
        overlaps(x_lo, x_hi, o.left(), o.right()).then(|| {
            best_of(
                [
                    o.top() - moving.top(),
                    o.bottom() - moving.bottom(),
                    o.top() - moving.bottom(),
                    o.bottom() - moving.top(),
                ],
                threshold,
            )
        })?
    }
    // Candidates are ranked by how many independent touches they validate
    // first, magnitude second — a fully-grounded two-axis touch (to one
    // object or two) always beats a smaller single-axis correction that
    // doesn't verify anything on the other axis, even though the latter has
    // a smaller raw magnitude. Without this, a lone object's best-effort
    // single-axis snap could outrank a genuine two-object corner touch
    // simply for being numerically closer.
    fn consider(
        p: (f32, f32),
        touches: u8,
        best: &mut Option<(f32, f32)>,
        best_touches: &mut u8,
        best_dist2: &mut f32,
    ) {
        let dist2 = p.0 * p.0 + p.1 * p.1;
        let better = touches > *best_touches || (touches == *best_touches && dist2 < *best_dist2);
        if best.is_none() || better {
            *best_touches = touches;
            *best_dist2 = dist2;
            *best = Some(p);
        }
    }

    let mut best: Option<(f32, f32)> = None;
    let mut best_touches = 0u8;
    let mut best_dist2 = f32::INFINITY;

    // Each object's primary correction on its own axis, using moving's
    // original (unshifted) span on the other axis. These double as this
    // object's contribution when pairing with a *different* object below.
    let dx0: Vec<Option<f32>> = others
        .iter()
        .map(|o| dx_for(moving, o, moving.top(), moving.bottom(), threshold))
        .collect();
    let dy0: Vec<Option<f32>> = others
        .iter()
        .map(|o| dy_for(moving, o, moving.left(), moving.right(), threshold))
        .collect();

    for (i, o) in others.iter().enumerate() {
        // X first, using moving's original vertical span; an optional Y
        // refinement against this same object once X is applied. The
        // refinement (when found) makes this a validated two-touch
        // candidate rather than a one-touch candidate, so it can't be
        // silently downgraded to competing on magnitude alone against an
        // unrelated two-touch corner.
        if let Some(dx) = dx0[i] {
            match dy_for(
                moving,
                o,
                moving.left() + dx,
                moving.right() + dx,
                threshold,
            ) {
                Some(dy) => consider((dx, dy), 2, &mut best, &mut best_touches, &mut best_dist2),
                None => consider((dx, 0.0), 1, &mut best, &mut best_touches, &mut best_dist2),
            }
        }
        // Y first, with an optional X refinement, symmetric to the above.
        if let Some(dy) = dy0[i] {
            match dx_for(
                moving,
                o,
                moving.top() + dy,
                moving.bottom() + dy,
                threshold,
            ) {
                Some(dx) => consider((dx, dy), 2, &mut best, &mut best_touches, &mut best_dist2),
                None => consider((0.0, dy), 1, &mut best, &mut best_touches, &mut best_dist2),
            }
        }
        // Corner: `o` shares no span with `moving` on either axis, so only a
        // simultaneous zero-gap close on both — via gap-closing (adjacency)
        // formulas only, since an edge *alignment* without any shared span
        // wouldn't create contact at all — counts as a snap.
        let y_overlap = overlaps(moving.top(), moving.bottom(), o.top(), o.bottom());
        let x_overlap = overlaps(moving.left(), moving.right(), o.left(), o.right());
        if !y_overlap && !x_overlap {
            let dx_c = [o.left() - moving.right(), o.right() - moving.left()]
                .into_iter()
                .min_by(|a: &f32, b: &f32| a.abs().partial_cmp(&b.abs()).unwrap())
                .unwrap();
            let dy_c = [o.top() - moving.bottom(), o.bottom() - moving.top()]
                .into_iter()
                .min_by(|a: &f32, b: &f32| a.abs().partial_cmp(&b.abs()).unwrap())
                .unwrap();
            if dx_c.abs() <= threshold && dy_c.abs() <= threshold {
                consider(
                    (dx_c, dy_c),
                    2,
                    &mut best,
                    &mut best_touches,
                    &mut best_dist2,
                );
            }
        }
    }

    // Cross-object corners: pair every object's X correction with a
    // *different* object's Y correction, keeping the pair only if applying
    // both still leaves each one genuinely touching its own anchor.
    for (i, a) in others.iter().enumerate() {
        let Some(dx) = dx0[i] else { continue };
        for (j, b) in others.iter().enumerate() {
            if i == j {
                continue;
            }
            let Some(dy) = dy0[j] else { continue };
            let a_still_touches =
                overlaps(moving.top() + dy, moving.bottom() + dy, a.top(), a.bottom());
            let b_still_touches =
                overlaps(moving.left() + dx, moving.right() + dx, b.left(), b.right());
            if a_still_touches && b_still_touches {
                consider((dx, dy), 2, &mut best, &mut best_touches, &mut best_dist2);
            }
        }
    }

    best.unwrap_or((0.0, 0.0))
}

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
                        self.touch_drags.remove(&touch.id);
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
        self.input_mode = InputMode::Idle;
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
                self.drawables.entries.retain(|e| !e.selected);
                self.input_mode = InputMode::Idle;
                self.touch_drags.clear(); // entry indices may have shifted after retain
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
                for mut d in pasted {
                    let (x, y) = (d.x(), d.y());
                    d.set_position(x + offset, y + offset);
                    self.drawables.push(d);
                    if let Some(last) = self.drawables.entries.last_mut() {
                        last.selected = true;
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::{Rect, snap_delta};

    #[test]
    fn snaps_aligning_left_edges() {
        // moving's left edge (105) is 5px from other's left edge (100).
        let other = Rect {
            x: 100.0,
            y: 0.0,
            w: 50.0,
            h: 20.0,
        };
        let moving = Rect {
            x: 105.0,
            y: 0.0,
            w: 20.0,
            h: 20.0,
        };
        let (dx, dy) = snap_delta(moving, &[other], 10.0);
        assert_eq!(dx, -5.0); // align left edges
        assert_eq!(dy, 0.0); // tops already aligned
    }

    #[test]
    fn snaps_edge_to_edge_adjacency() {
        // moving's right edge (95) is 5px shy of other's left edge (100), and the
        // two overlap vertically (same y), so they would touch along that seam.
        let other = Rect {
            x: 100.0,
            y: 0.0,
            w: 50.0,
            h: 20.0,
        };
        let moving = Rect {
            x: 75.0,
            y: 0.0,
            w: 20.0,
            h: 20.0,
        };
        let (dx, _dy) = snap_delta(moving, &[other], 10.0);
        assert_eq!(dx, 5.0); // push right edge flush against other's left edge
    }

    #[test]
    fn no_snap_beyond_threshold() {
        // Far enough that every edge pairing (align and adjacency) exceeds 10px.
        let other = Rect {
            x: 100.0,
            y: 0.0,
            w: 50.0,
            h: 20.0,
        };
        let moving = Rect {
            x: 300.0,
            y: 200.0,
            w: 20.0,
            h: 20.0,
        };
        let (dx, dy) = snap_delta(moving, &[other], 10.0);
        assert_eq!(dx, 0.0);
        assert_eq!(dy, 0.0);
    }

    #[test]
    fn picks_nearest_candidate() {
        // Two static rects offer left-edge alignment of -1 and -3; expect -1.
        // Wide/tall so only the left-alignment candidate falls within threshold,
        // and moving sits inside their vertical span so X snapping is allowed.
        let other_a = Rect {
            x: 102.0,
            y: 0.0,
            w: 1000.0,
            h: 1000.0,
        };
        let other_b = Rect {
            x: 100.0,
            y: 0.0,
            w: 1000.0,
            h: 1000.0,
        };
        let moving = Rect {
            x: 103.0,
            y: 500.0,
            w: 10.0,
            h: 10.0,
        };
        let (dx, _dy) = snap_delta(moving, &[other_a, other_b], 10.0);
        assert_eq!(dx, -1.0);
    }

    #[test]
    fn no_cross_axis_snap_when_not_overlapping() {
        // Nearly aligned on Y but far apart on X: must NOT snap Y, because the
        // objects share no horizontal span and so would never touch.
        let other = Rect {
            x: 0.0,
            y: 100.0,
            w: 50.0,
            h: 50.0,
        };
        let moving = Rect {
            x: 1000.0,
            y: 105.0,
            w: 50.0,
            h: 50.0,
        };
        let (dx, dy) = snap_delta(moving, &[other], 10.0);
        assert_eq!(dx, 0.0);
        assert_eq!(dy, 0.0); // would have been -5 under the old per-axis logic
    }

    #[test]
    fn snaps_y_when_overlapping_on_x() {
        // Overlapping horizontally and 5px off vertically: tops align.
        let other = Rect {
            x: 0.0,
            y: 100.0,
            w: 50.0,
            h: 50.0,
        };
        let moving = Rect {
            x: 10.0,
            y: 105.0,
            w: 50.0,
            h: 50.0,
        };
        let (_dx, dy) = snap_delta(moving, &[other], 10.0);
        assert_eq!(dy, -5.0);
    }

    #[test]
    fn snaps_both_axes_when_closing_a_gap() {
        // `moving` sits left of `other` with an 8px horizontal gap and tops 5px
        // off. Closing the X gap (adjacency) should also enable the Y alignment
        // snap, even though the rects don't yet overlap on X. Threshold 20.
        let other = Rect {
            x: 100.0,
            y: 100.0,
            w: 50.0,
            h: 50.0,
        }; // x[100,150]
        let moving = Rect {
            x: 42.0,
            y: 105.0,
            w: 50.0,
            h: 50.0,
        }; // x[42,92], 8px gap to other's left edge
        let (dx, dy) = snap_delta(moving, &[other], 20.0);
        assert_eq!(dx, 8.0); // right edge 92 → flush against other's left edge 100
        assert_eq!(dy, -5.0); // top 105 → 100
    }

    #[test]
    fn no_mixed_snap_across_unrelated_objects_with_large_threshold() {
        // Regression test for the reported bug: with a large snap_px, an
        // object with no real horizontal relationship to `moving` could
        // still win the Y correction merely for having a close top edge,
        // because the old gate used the full (now large) threshold as slack
        // for the perpendicular-overlap check. `x_neighbor` genuinely
        // overlaps `moving` vertically (same y range) and is a real X snap.
        // `y_decoy` shares no horizontal span with `moving` at all -- even
        // after the X snap is applied -- but has a tempting 3px-off top
        // edge; it must be ignored.
        let x_neighbor = Rect {
            x: 50.0,
            y: 505.0,
            w: 20.0,
            h: 20.0,
        }; // x[50,70] y[505,525], matches moving's y exactly
        let moving = Rect {
            x: 75.0,
            y: 505.0,
            w: 20.0,
            h: 20.0,
        }; // x[75,95] y[505,525], 5px gap to x_neighbor's right edge
        let y_decoy = Rect {
            x: 250.0,
            y: 508.0,
            w: 20.0,
            h: 20.0,
        }; // x[250,270] -- 160px gap even after the X snap; y[508,528], 3px top offset
        let (dx, dy) = snap_delta(moving, &[x_neighbor, y_decoy], 200.0);
        assert_eq!(dx, -5.0); // flush against x_neighbor's right edge
        assert_eq!(dy, 0.0); // NOT -3.0 toward y_decoy -- no real horizontal relationship
    }

    #[test]
    fn no_mixed_snap_across_unrelated_objects_with_large_threshold2() {
        // `x_decoy` shares real X-overlap with `moving` (offers a Y snap);
        // `y_decoy` shares real Y-overlap with `moving` (offers an X snap).
        // Neither shares a span with `moving` on both axes at once, so the
        // result must fully commit to exactly one of them -- not an X
        // correction from one and a Y correction from the other, which
        // would land `moving` touching neither. `y_decoy`'s correction
        // (-79) is smaller than `x_decoy`'s (-80), so it wins outright.
        let x_decoy = Rect {
            x: 200.0,
            y: 100.0,
            w: 20.0,
            h: 20.0,
        };
        let moving = Rect {
            x: 199.0,
            y: 200.0,
            w: 20.0,
            h: 20.0,
        };
        let y_decoy = Rect {
            x: 100.0,
            y: 200.0,
            w: 20.0,
            h: 20.0,
        };
        let (dx, dy) = snap_delta(moving, &[x_decoy, y_decoy], 100.0);
        assert_eq!((dx, dy), (-79.0, 0.0));
    }

    #[test]
    fn snaps_into_concave_corner_of_two_overlapping_objects() {
        // `a` and `b` partially overlap each other (share the region
        // x[40,60] y[40,60]), which carves a concave notch into their
        // combined silhouette at the point (60, 40): `a`'s right edge to the
        // left, `b`'s top edge below. `moving` sits in that notch, close to
        // both edges but touching neither yet. The correct snap touches
        // BOTH: `a` on X (real Y-overlap: moving's y-span sits inside a's
        // full height) and `b` on Y (real X-overlap: moving's x-span sits
        // inside b's full width) -- landing it exactly in the corner.
        let a = Rect {
            x: 0.0,
            y: 0.0,
            w: 60.0,
            h: 60.0,
        }; // x[0,60] y[0,60]
        let b = Rect {
            x: 40.0,
            y: 40.0,
            w: 60.0,
            h: 60.0,
        }; // x[40,100] y[40,100]
        let moving = Rect {
            x: 70.0,
            y: 25.0,
            w: 10.0,
            h: 10.0,
        }; // x[70,80] (10px gap to a's right edge), y[25,35] (5px gap to b's top edge)
        let (dx, dy) = snap_delta(moving, &[a, b], 50.0);
        assert_eq!(dx, -10.0); // right edge of `a` (60) meets moving's left edge
        assert_eq!(dy, 5.0); // top edge of `b` (40) meets moving's bottom edge
    }

    #[test]
    fn concave_corner_wins_over_smaller_partial_touch_at_small_threshold() {
        // Same notch as `snaps_into_concave_corner_of_two_overlapping_objects`,
        // but with a small threshold (20, matching a real snap_px value).
        // `a` alone can offer a same-object refinement (dx=-10, then a Y
        // realignment against `a`'s own top/bottom), but at this threshold
        // that refinement's candidates (top-top = -25, bottom-bottom = 25,
        // etc.) all exceed 20, so it can only offer a *one*-touch fallback
        // (dx=-10, dy=0, magnitude 10) that never actually validates
        // anything on Y. The genuine corner touch (dx=-10, dy=5, magnitude
        // ~11.18, touching BOTH `a` and `b`) has a larger raw magnitude but
        // validates more -- it must still win.
        let a = Rect {
            x: 0.0,
            y: 0.0,
            w: 60.0,
            h: 60.0,
        };
        let b = Rect {
            x: 40.0,
            y: 40.0,
            w: 60.0,
            h: 60.0,
        };
        let moving = Rect {
            x: 70.0,
            y: 25.0,
            w: 10.0,
            h: 10.0,
        };
        let (dx, dy) = snap_delta(moving, &[a, b], 20.0);
        assert_eq!(dx, -10.0);
        assert_eq!(dy, 5.0);
    }

    #[test]
    fn snaps_diagonal_corner_to_single_object() {
        // `moving` sits diagonally offset from `other`: an 8px gap on X and
        // a 6px gap on Y, sharing no span on either axis. This is the one
        // case where a snap is allowed without any real overlap: closing
        // both gaps at once brings the two rects corner-to-corner against
        // the SAME object.
        let other = Rect {
            x: 100.0,
            y: 100.0,
            w: 50.0,
            h: 50.0,
        }; // x[100,150] y[100,150]
        let moving = Rect {
            x: 42.0,
            y: 44.0,
            w: 50.0,
            h: 50.0,
        }; // x[42,92] (8px gap) y[44,94] (6px gap)
        let (dx, dy) = snap_delta(moving, &[other], 20.0);
        assert_eq!(dx, 8.0);
        assert_eq!(dy, 6.0);
    }

    #[test]
    fn no_partial_corner_snap() {
        // Diagonal gap where the X gap is closeable within threshold but the
        // Y gap is far too large. A corner snap requires BOTH gaps to close
        // against the same object, so neither axis should move.
        let other = Rect {
            x: 100.0,
            y: 100.0,
            w: 50.0,
            h: 50.0,
        };
        let moving = Rect {
            x: 42.0,
            y: -500.0,
            w: 50.0,
            h: 50.0,
        }; // x gap 8 (within 20), y gap huge
        let (dx, dy) = snap_delta(moving, &[other], 20.0);
        assert_eq!(dx, 0.0);
        assert_eq!(dy, 0.0);
    }

    #[test]
    fn disabled_with_zero_threshold() {
        let other = Rect {
            x: 100.0,
            y: 0.0,
            w: 50.0,
            h: 20.0,
        };
        let moving = Rect {
            x: 100.0,
            y: 0.0,
            w: 20.0,
            h: 20.0,
        };
        let (dx, dy) = snap_delta(moving, &[other], 0.0);
        assert_eq!(dx, 0.0);
        assert_eq!(dy, 0.0);
    }
}

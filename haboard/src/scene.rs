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
/// A snap on one axis is only considered against an object the moving rect
/// **overlaps (or is within `threshold` of overlapping) on the other axis** —
/// so the two objects share, or are about to share once snapped, a span along
/// which they would actually meet. (Without the overlap requirement, a far-away
/// object on the X axis could still pull the moving object's Y to align with
/// it; without the `threshold` slack, closing a small gap on one axis wouldn't
/// enable the corner-completing snap on the other.) Each axis is resolved
/// independently; an axis with no qualifying candidate yields `0.0`.
fn snap_delta(moving: Rect, others: &[Rect], threshold: f32) -> (f32, f32) {
    // Overlap on one axis, treating a gap of up to `margin` as overlapping (the
    // perpendicular snap may close such a gap, making the objects adjacent).
    fn overlaps(a_lo: f32, a_hi: f32, b_lo: f32, b_hi: f32, margin: f32) -> bool {
        a_lo <= b_hi + margin && b_lo <= a_hi + margin
    }
    // Keep the smaller-magnitude correction if it is within the running best.
    fn consider(cands: [f32; 4], best: &mut f32, best_abs: &mut f32) {
        for cand in cands {
            let a = cand.abs();
            if a <= *best_abs {
                *best_abs = a;
                *best = cand;
            }
        }
    }

    let (mut dx, mut dy) = (0.0_f32, 0.0_f32);
    let (mut dx_abs, mut dy_abs) = (threshold, threshold); // inclusive threshold
    for o in others {
        let x_overlap = overlaps(
            moving.left(),
            moving.right(),
            o.left(),
            o.right(),
            threshold,
        );
        let y_overlap = overlaps(
            moving.top(),
            moving.bottom(),
            o.top(),
            o.bottom(),
            threshold,
        );

        // Horizontal correction (aligns/touches vertical edges) needs the objects
        // to overlap vertically, otherwise they couldn't touch along that seam.
        if y_overlap {
            consider(
                [
                    o.left() - moving.left(),
                    o.right() - moving.right(),
                    o.left() - moving.right(),
                    o.right() - moving.left(),
                ],
                &mut dx,
                &mut dx_abs,
            );
        }
        // Vertical correction needs horizontal overlap.
        if x_overlap {
            consider(
                [
                    o.top() - moving.top(),
                    o.bottom() - moving.bottom(),
                    o.top() - moving.bottom(),
                    o.bottom() - moving.top(),
                ],
                &mut dy,
                &mut dy_abs,
            );
        }
    }
    (dx, dy)
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
    /// delta so the group's bounding box snaps to nearby static objects; `(0,
    /// 0)` when snapping is disabled (`snap_px <= 0.0`), Ctrl is not held,
    /// or nothing is within range.
    fn snap_adjustment(&self, moving: &[(usize, f32, f32)], dx: f32, dy: f32) -> (f32, f32) {
        if self.snap_px <= 0.0 || moving.is_empty() || !self.modifiers.control_key() {
            return (0.0, 0.0);
        }

        // Tentative bounding box of the dragged group (start positions + delta).
        let (mut min_x, mut min_y) = (f32::INFINITY, f32::INFINITY);
        let (mut max_x, mut max_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        let mut moving_idx: Vec<usize> = Vec::with_capacity(moving.len());
        for &(idx, sx, sy) in moving {
            let d = &self.drawables.entries[idx].drawable;
            let (l, t) = (sx + dx, sy + dy);
            min_x = min_x.min(l);
            min_y = min_y.min(t);
            max_x = max_x.max(l + d.width());
            max_y = max_y.max(t + d.height());
            moving_idx.push(idx);
        }
        let group = Rect {
            x: min_x,
            y: min_y,
            w: max_x - min_x,
            h: max_y - min_y,
        };

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

        snap_delta(group, &others, self.snap_px)
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

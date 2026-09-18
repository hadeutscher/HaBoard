//! Edge-snap correction used while dragging in [`Scene`](crate::Scene).

/// Axis-aligned rectangle used for edge-snap calculations.
#[derive(Clone, Copy)]
pub(crate) struct Rect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
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

/// Compute the rigid-body `(dx, dy)` correction for a **group** of moving
/// rects against the stationary `others`.
///
/// `moving` holds the group members at their already-dragged (un-snapped)
/// positions, in a stable order. The returned correction is applied to every
/// member, so the group translates as one body.
///
/// The aggregation mirrors the multi-select snapping in HaCreator
/// (`InputHandler` → `ISnappable::DoSnap` → `BoardItem::SnapMoveAllMouseBoundItems`):
/// each member is visited in turn and looks for its own best snap; a member
/// that finds one translates the *entire* group by its correction, and the
/// next member is then evaluated from that already-corrected position. A
/// member that finds nothing contributes nothing and is simply skipped.
///
/// Two properties follow, and both are deliberate:
///
/// - A member with no snap candidate cannot veto the group's snap. Picking the
///   smallest correction across members instead would let any member that has
///   nothing nearby (correction `(0, 0)`) win outright and cancel a perfectly
///   good snap found by another member — the defect this replaces.
/// - The visit is a sequential refinement rather than an independent vote, so
///   a later member snapping against the position a previous one established
///   can tighten the fit (e.g. one member lands flush on X, and the next then
///   finds a Y alignment that only exists at that corrected position).
pub(crate) fn group_snap_delta(moving: &[Rect], others: &[Rect], threshold: f32) -> (f32, f32) {
    let mut adj = (0.0f32, 0.0f32);
    for m in moving {
        // Evaluate this member from the group's running, corrected position —
        // HaCreator re-reads each item's live `X`/`Y`, which the previous
        // member's `SnapMoveAllMouseBoundItems` has already shifted.
        let item = Rect {
            x: m.x + adj.0,
            y: m.y + adj.1,
            ..*m
        };
        if let Some((dx, dy)) = snap_delta_opt(item, others, threshold) {
            adj = (adj.0 + dx, adj.1 + dy);
        }
    }
    adj
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
/// `None` when nothing is in range at all — distinct from a candidate that
/// happens to need no correction, so a caller can tell "found nothing" from
/// "already flush".
fn snap_delta_opt(moving: Rect, others: &[Rect], threshold: f32) -> Option<(f32, f32)> {
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

    best
}

#[cfg(test)]
mod tests {
    use super::{Rect, group_snap_delta, snap_delta_opt};

    /// Single-rect snap with "no candidate" flattened to a zero correction.
    fn snap_delta(moving: Rect, others: &[Rect], threshold: f32) -> (f32, f32) {
        snap_delta_opt(moving, others, threshold).unwrap_or((0.0, 0.0))
    }

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

    // ── Group (multi-select) snapping ────────────────────────────────────

    #[test]
    fn group_member_without_a_candidate_does_not_veto_the_snap() {
        // Regression test for the multi-select defect: `leader` is 5px shy of
        // `anchor`'s left edge and should pull the whole group flush, while
        // `tagalong` sits far away with nothing in range. Under the old
        // "smallest correction across members" rule, `tagalong`'s empty
        // (0, 0) was the smallest and cancelled the snap entirely.
        let anchor = Rect {
            x: 100.0,
            y: 0.0,
            w: 50.0,
            h: 20.0,
        };
        let leader = Rect {
            x: 75.0,
            y: 0.0,
            w: 20.0,
            h: 20.0,
        }; // right edge 95, 5px gap to anchor's left edge
        let tagalong = Rect {
            x: 5000.0,
            y: 5000.0,
            w: 20.0,
            h: 20.0,
        };
        let (dx, dy) = group_snap_delta(&[leader, tagalong], &[anchor], 10.0);
        assert_eq!(dx, 5.0);
        assert_eq!(dy, 0.0);

        // Order must not matter when only one member has anything to snap to.
        let (dx, dy) = group_snap_delta(&[tagalong, leader], &[anchor], 10.0);
        assert_eq!(dx, 5.0);
        assert_eq!(dy, 0.0);
    }

    #[test]
    fn group_with_no_candidates_at_all_does_not_move() {
        let anchor = Rect {
            x: 100.0,
            y: 100.0,
            w: 50.0,
            h: 50.0,
        };
        let a = Rect {
            x: 5000.0,
            y: 5000.0,
            w: 20.0,
            h: 20.0,
        };
        let b = Rect {
            x: 6000.0,
            y: 6000.0,
            w: 20.0,
            h: 20.0,
        };
        assert_eq!(group_snap_delta(&[a, b], &[anchor], 20.0), (0.0, 0.0));
    }

    #[test]
    fn group_translates_rigidly_by_the_snapping_member() {
        // Only `leader` is near `anchor`; the correction that lands it flush
        // is the correction the whole group receives — `follower` keeps its
        // relative offset rather than snapping independently.
        let anchor = Rect {
            x: 0.0,
            y: 100.0,
            w: 200.0,
            h: 50.0,
        }; // wide, so `leader` sits well inside it on X and no X snap is in range
        let leader = Rect {
            x: 60.0,
            y: 105.0,
            w: 50.0,
            h: 50.0,
        }; // tops 5px off, overlaps on X
        let follower = Rect {
            x: 60.0,
            y: 400.0,
            w: 50.0,
            h: 50.0,
        };
        let (dx, dy) = group_snap_delta(&[leader, follower], &[anchor], 10.0);
        assert_eq!((dx, dy), (0.0, -5.0));
    }

    #[test]
    fn later_member_refines_from_the_corrected_position() {
        // `x_anchor` gives the first member a pure X snap (5px right). Only
        // once that shift is applied does the second member share a real
        // vertical span with `y_anchor`, unlocking a Y alignment of -4. The
        // sequential visit must compose the two into (5, -4); evaluating the
        // members independently would miss the second snap entirely.
        let x_anchor = Rect {
            x: 100.0,
            y: 0.0,
            w: 50.0,
            h: 20.0,
        }; // x[100,150] y[0,20]
        let first = Rect {
            x: 75.0,
            y: 0.0,
            w: 20.0,
            h: 20.0,
        }; // x[75,95] -> snaps +5 to x[100,120]
        let second = Rect {
            x: 75.0,
            y: 204.0,
            w: 20.0,
            h: 20.0,
        }; // x[75,95] -> after +5 becomes x[100,120]
        let y_anchor = Rect {
            x: 100.0,
            y: 200.0,
            w: 20.0,
            h: 20.0,
        }; // x[100,120]: no X overlap with second at x[75,95], full overlap at x[100,120]
        let (dx, dy) = group_snap_delta(&[first, second], &[x_anchor, y_anchor], 10.0);
        assert_eq!((dx, dy), (5.0, -4.0));
    }

    #[test]
    fn single_member_group_matches_the_single_rect_snap() {
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
        assert_eq!(
            group_snap_delta(&[moving], &[other], 10.0),
            snap_delta(moving, &[other], 10.0)
        );
    }

    #[test]
    fn empty_group_does_not_move() {
        let other = Rect {
            x: 100.0,
            y: 0.0,
            w: 50.0,
            h: 20.0,
        };
        assert_eq!(group_snap_delta(&[], &[other], 10.0), (0.0, 0.0));
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

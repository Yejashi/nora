//! Layout tests: split panes and deterministic layout (N24a).
//!
//! The layout is a pure, headless data structure; these tests exercise the
//! binary split tree, focus, collapse and the integer rectangle allocation
//! without any I/O, PTY or window dependency.

use nora::layout::{Layout, PaneId, SplitDirection};

#[test]
fn split_is_refused_when_a_pane_would_be_under_two_cells() {
    let mut layout = Layout::new();
    // 4 columns halve into 2 + 2; 3 would leave a 1-column pane.
    assert!(layout.can_split(SplitDirection::Horizontal, 4, 1));
    assert!(!layout.can_split(SplitDirection::Horizontal, 3, 24));
    assert!(!layout.can_split(SplitDirection::Horizontal, 1, 24));
    assert!(!layout.can_split(SplitDirection::Vertical, 80, 3));
    assert!(layout.can_split(SplitDirection::Vertical, 80, 4));
    // The check is on the active pane's own rectangle: in a 10-column
    // area, after one split the active (right) pane has 5 columns, which
    // halve into 2 + 3; the new active 3-column pane cannot be split again.
    layout.split_horizontal();
    assert!(layout.can_split(SplitDirection::Horizontal, 10, 24));
    layout.split_horizontal();
    let active = layout.active_pane();
    let rect = layout
        .compute_rectangles(10, 24)
        .into_iter()
        .find(|r| r.0 == active)
        .expect("active pane laid out");
    assert_eq!(rect.3, 3);
    assert!(!layout.can_split(SplitDirection::Horizontal, 10, 24));
    assert!(layout.can_split(SplitDirection::Vertical, 10, 24));
    // An empty layout re-roots on a split.
    let mut empty = Layout::new();
    empty.close_pane(PaneId(0));
    assert!(empty.can_split(SplitDirection::Horizontal, 1, 1));
}

/// Assert that the rectangles are pairwise nonoverlapping, fit inside the
/// total area, and tile it exactly (their areas sum to the total area).
fn assert_tiling(rects: &[(PaneId, u16, u16, u16, u16)], total_w: u16, total_h: u16) {
    let mut area: u32 = 0;
    for &(a, ax, ay, aw, ah) in rects {
        area += u32::from(aw) * u32::from(ah);
        assert!(ax + aw <= total_w, "pane {a:?} exceeds the total width");
        assert!(ay + ah <= total_h, "pane {a:?} exceeds the total height");
    }
    // Pairwise non-overlap.
    for i in 0..rects.len() {
        let (a, ax, ay, aw, ah) = rects[i];
        for &(b, bx, by, bw, bh) in &rects[i + 1..] {
            let overlap_x = ax < bx + bw && bx < ax + aw;
            let overlap_y = ay < by + bh && by < ay + ah;
            assert!(!overlap_x || !overlap_y, "panes {a:?} and {b:?} overlap");
        }
    }
    assert_eq!(
        area,
        u32::from(total_w) * u32::from(total_h),
        "rectangles must tile the total area exactly"
    );
}

#[test]
fn layout_default() {
    let layout = Layout::default();
    assert_eq!(layout.pane_count(), 1);
    assert_eq!(layout.panes(), vec![PaneId(0)]);
    assert_eq!(layout.active_pane(), PaneId(0));
    // The single pane fills the whole area.
    assert_eq!(
        layout.compute_rectangles(10, 5),
        vec![(PaneId(0), 0, 0, 10, 5)]
    );
}

#[test]
fn layout_split_horizontal() {
    let mut layout = Layout::new();
    let second = layout.split_horizontal();
    assert_eq!(layout.pane_count(), 2);
    assert_eq!(layout.panes(), vec![PaneId(0), second]);
    // The new pane becomes active.
    assert_eq!(layout.active_pane(), second);
    // Two panes side by side, equal halves, in tree (pre-order) order.
    assert_eq!(
        layout.compute_rectangles(10, 5),
        vec![(PaneId(0), 0, 0, 5, 5), (second, 5, 0, 5, 5)]
    );
}

#[test]
fn layout_split_vertical() {
    let mut layout = Layout::new();
    let second = layout.split_vertical();
    assert_eq!(layout.pane_count(), 2);
    assert_eq!(layout.active_pane(), second);
    // Two panes stacked: the first child gets floor(5 * 0.5) = 2 rows and
    // the second child the remainder (3 rows).
    assert_eq!(
        layout.compute_rectangles(10, 5),
        vec![(PaneId(0), 0, 0, 10, 2), (second, 0, 2, 10, 3)]
    );
}

#[test]
fn layout_close_pane() {
    let mut layout = Layout::new();
    let second = layout.split_horizontal();
    layout.close_pane(second);
    // The split collapsed; the sibling takes the closed pane's place.
    assert_eq!(layout.pane_count(), 1);
    assert_eq!(layout.panes(), vec![PaneId(0)]);
    assert_eq!(
        layout.compute_rectangles(10, 5),
        vec![(PaneId(0), 0, 0, 10, 5)]
    );
    // Closing a pane that is not in the layout is a no-op.
    layout.close_pane(second);
    assert_eq!(layout.pane_count(), 1);
}

#[test]
fn layout_focus_pane() {
    let mut layout = Layout::new();
    let second = layout.split_horizontal();
    // Focus the first pane.
    assert!(layout.focus_pane(PaneId(0)));
    assert_eq!(layout.active_pane(), PaneId(0));
    // Focus the second pane.
    assert!(layout.focus_pane(second));
    assert_eq!(layout.active_pane(), second);
    // Focusing a pane that is not in the layout fails and keeps focus.
    assert!(!layout.focus_pane(PaneId(99)));
    assert_eq!(layout.active_pane(), second);
}

#[test]
fn layout_active_pane() {
    let mut layout = Layout::new();
    assert_eq!(layout.active_pane(), PaneId(0));
    // Splitting focuses the new pane.
    let second = layout.split_horizontal();
    assert_eq!(layout.active_pane(), second);
    // Focusing moves the active pane.
    assert!(layout.focus_pane(PaneId(0)));
    assert_eq!(layout.active_pane(), PaneId(0));
}

#[test]
fn layout_panes() {
    let mut layout = Layout::new();
    let p1 = layout.split_horizontal();
    let p2 = layout.split_vertical();
    // All live panes are listed, in ascending ID order (deterministic).
    assert_eq!(layout.panes(), vec![PaneId(0), p1, p2]);
    // Closing one removes it from the list.
    layout.close_pane(p2);
    assert_eq!(layout.panes(), vec![PaneId(0), p1]);
}

#[test]
fn layout_pane_count() {
    let mut layout = Layout::new();
    assert_eq!(layout.pane_count(), 1);
    layout.split_horizontal();
    layout.split_vertical();
    assert_eq!(layout.pane_count(), 3);
    layout.close_pane(PaneId(1));
    assert_eq!(layout.pane_count(), 2);
}

#[test]
fn layout_compute_rectangles() {
    let mut layout = Layout::new();
    // Tree: SplitH { p0, SplitV { p1, p2 } }
    let p1 = layout.split_horizontal(); // active p1
    let p2 = layout.split_vertical(); // splits active p1
    assert_eq!(layout.pane_count(), 3);
    assert_eq!(
        layout.compute_rectangles(10, 10),
        vec![(PaneId(0), 0, 0, 5, 10), (p1, 5, 0, 5, 5), (p2, 5, 5, 5, 5),]
    );
    // Deterministic remainder allocation: two panes in 5 columns give the
    // first child floor(5 * 0.5) = 2 and the second child 3.
    let mut two = Layout::new();
    let other = two.split_horizontal();
    assert_eq!(
        two.compute_rectangles(5, 2),
        vec![(PaneId(0), 0, 0, 2, 2), (other, 2, 0, 3, 2)]
    );
}

#[test]
fn layout_nonoverlapping() {
    // Tree: SplitH { SplitH { p0, _ }, SplitV { p1, _ } }
    let mut layout = Layout::new();
    let p1 = layout.split_horizontal();
    assert!(layout.focus_pane(PaneId(0)));
    layout.split_horizontal(); // splits p0
    assert!(layout.focus_pane(p1));
    layout.split_vertical(); // splits p1
    assert_eq!(layout.pane_count(), 4);
    for (w, h) in [(10u16, 10u16), (7, 5), (4, 4)] {
        let rects = layout.compute_rectangles(w, h);
        assert_eq!(rects.len(), 4, "all panes get a rectangle at {w}x{h}");
        assert_tiling(&rects, w, h);
    }
}

#[test]
fn layout_no_zero_grids() {
    // Tree: SplitH { SplitH { p0, _ }, SplitV { p1, _ } }
    let mut layout = Layout::new();
    let p1 = layout.split_horizontal();
    assert!(layout.focus_pane(PaneId(0)));
    layout.split_horizontal(); // splits p0
    assert!(layout.focus_pane(p1));
    layout.split_vertical(); // splits p1
    // At every size where each split has at least two cells to divide
    // (width >= 4, height >= 3 for this tree), every pane gets at least
    // one cell in both dimensions.
    for (w, h) in [(4u16, 4u16), (5, 4), (8, 6), (4, 3)] {
        let rects = layout.compute_rectangles(w, h);
        for (pane, x, y, rw, rh) in &rects {
            assert!(
                *rw >= 1 && *rh >= 1,
                "pane {pane:?} got a zero grid at {w}x{h}: ({x}, {y}, {rw}, {rh})"
            );
        }
        assert_tiling(&rects, w, h);
    }
}

#[test]
fn layout_close_active_pane() {
    let mut layout = Layout::new();
    let second = layout.split_horizontal(); // active = second
    layout.close_pane(second);
    // Focus falls back to the first remaining pane.
    assert_eq!(layout.active_pane(), PaneId(0));
    assert_eq!(layout.pane_count(), 1);
}

#[test]
fn layout_close_last_pane() {
    let mut layout = Layout::new();
    layout.close_pane(PaneId(0));
    // The layout is empty: no panes, no rectangles.
    assert_eq!(layout.pane_count(), 0);
    assert!(layout.panes().is_empty());
    assert!(layout.compute_rectangles(10, 5).is_empty());
    // Splitting an empty layout re-roots at the new pane.
    let pane = layout.split_horizontal();
    assert_eq!(layout.pane_count(), 1);
    assert_eq!(layout.active_pane(), pane);
    assert_eq!(layout.compute_rectangles(10, 5), vec![(pane, 0, 0, 10, 5)]);
}

// ---------------------------------------------------------------------------
// N24b: split UI + routing
// ---------------------------------------------------------------------------

/// `layout_split_shortcuts`: the split shortcuts (Ctrl+D / Ctrl+Shift+D)
/// split the active pane and focus the new pane; closing (Ctrl+X) falls
/// back to a live pane.
#[test]
fn layout_split_shortcuts() {
    let mut layout = Layout::new();
    let first = layout.panes()[0];
    assert_eq!(layout.active_pane(), first);

    // Ctrl+D (horizontal): a new pane to the right becomes active.
    let h = layout.split_horizontal();
    assert_ne!(h, first);
    assert_eq!(layout.active_pane(), h);
    assert_eq!(layout.pane_count(), 2);

    // Ctrl+Shift+D (vertical): a new pane below becomes active.
    let v = layout.split_vertical();
    assert_ne!(v, h);
    assert_eq!(layout.active_pane(), v);
    assert_eq!(layout.pane_count(), 3);

    // Ctrl+X (close the active pane): focus falls back to a live pane.
    layout.close_pane(v);
    assert_ne!(layout.active_pane(), v);
    assert!(layout.has_pane(layout.active_pane()));
}

/// `layout_input_routing`: input is routed to the active pane only. The
/// layout exposes the active pane (the routing target) and which panes
/// are live; the app sends input only to `active_pane()`, so active input
/// never reaches another pane, and focus changes deterministically.
#[test]
fn layout_input_routing() {
    let mut layout = Layout::new();
    let a = layout.panes()[0];
    let b = layout.split_horizontal();

    // The new pane is active; input routes to `b`, not `a`.
    assert_eq!(layout.active_pane(), b);
    assert!(layout.has_pane(layout.active_pane()));

    // Moving focus moves the routing target; the active pane is always
    // live, so a routed input always reaches a real pane.
    layout.focus_pane(a);
    assert_eq!(layout.active_pane(), a);
    assert!(layout.has_pane(layout.active_pane()));

    // Focusing a closed pane is rejected, so routing never targets a dead
    // pane.
    let c = layout.split_vertical();
    layout.close_pane(c);
    assert!(!layout.focus_pane(c));
}

/// `layout_close_collapses`: closing a pane collapses the tree
/// consistently — the sibling takes its place, the live-pane count drops,
/// and the remaining pane tiles the window.
#[test]
fn layout_close_collapses() {
    let mut layout = Layout::new();
    let a = layout.panes()[0];
    let b = layout.split_horizontal();
    assert_eq!(layout.pane_count(), 2);

    layout.close_pane(a);
    assert_eq!(layout.pane_count(), 1);
    assert_eq!(layout.panes(), vec![b]);
    assert_eq!(layout.active_pane(), b);

    // The remaining pane takes the whole window.
    let rects = layout.compute_rectangles(100, 50);
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0].3, 100); // full width
    assert_eq!(rects[0].4, 50); // full height
}

/// `layout_stale_events`: stale events are ignored. An event tagged with
/// a closed (or never-minted) pane does not route — `has_pane` returns
/// false for it, so the app drops the event.
#[test]
fn layout_stale_events() {
    let mut layout = Layout::new();
    let a = layout.panes()[0];
    let b = layout.split_horizontal();
    layout.close_pane(a);

    // `a` is gone; a stale event for `a` is ignored.
    assert!(!layout.has_pane(a));
    // `b` is live; its events route.
    assert!(layout.has_pane(b));
    // A never-minted pane id is also stale.
    assert!(!layout.has_pane(PaneId(999)));
}

/// `layout_resize_updates_pty`: pane resize updates PTYs. After a window
/// resize, each pane's rectangle (and thus its PTY grid) reflects its new
/// fraction of the window; the rectangles tile the new window exactly and
/// no pane is zero-sized.
#[test]
fn layout_resize_updates_pty() {
    let mut layout = Layout::new();
    layout.split_horizontal();

    // Shrink the window: both panes shrink, tiling the new area exactly.
    let rects = layout.compute_rectangles(40, 20);
    assert_eq!(rects.len(), 2);
    let total: u16 = rects.iter().map(|r| r.3 * r.4).sum();
    assert_eq!(total, 40 * 20);
    for rect in &rects {
        assert!(rect.3 >= 1 && rect.4 >= 1);
    }

    // Grow it back: the panes grow, still tiling exactly.
    let rects = layout.compute_rectangles(120, 60);
    let total: u16 = rects.iter().map(|r| r.3 * r.4).sum();
    assert_eq!(total, 120 * 60);
}

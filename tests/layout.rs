//! Layout tests: split panes and deterministic layout (N24a).
//!
//! The layout is a pure, headless data structure; these tests exercise the
//! binary split tree, focus, collapse and the integer rectangle allocation
//! without any I/O, PTY or window dependency.

use nora::layout::{Layout, PaneId};

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

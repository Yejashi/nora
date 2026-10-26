//! Workspace tests: tabs, panes, and focus routing (N23a).
//!
//! The workspace is a pure, headless data structure; these tests exercise the
//! tab/pane tree and the active-tab/active-pane focus without any I/O, PTY or
//! window dependency.

use nora::workspace::{CoalescingSlot, SessionRef, TabCommand, TabId, Workspace};

#[test]
fn workspace_default() {
    let ws = Workspace::default();
    assert_eq!(ws.tab_count(), 0);
    assert!(ws.tabs().is_empty());
    assert_eq!(ws.active_tab(), None);
    assert_eq!(ws.active_pane(), None);
}

#[test]
fn workspace_add_tab() {
    let mut ws = Workspace::new();
    let (tab, pane) = ws.add_tab();
    assert_eq!(ws.tab_count(), 1);
    assert_eq!(ws.tabs().len(), 1);
    assert_eq!(ws.tabs()[0], tab);
    assert_eq!(ws.pane_count(tab), 1);
    let panes = ws.panes(tab).expect("the new tab has a pane");
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0], pane);
    // The first tab becomes active.
    assert_eq!(ws.active_tab(), Some(tab));
    assert_eq!(ws.active_pane(), Some(pane));
}

#[test]
fn workspace_close_tab() {
    let mut ws = Workspace::new();
    let (t1, _p1) = ws.add_tab();
    let (t2, _p2) = ws.add_tab();
    ws.close_tab(t1);
    assert_eq!(ws.tab_count(), 1);
    assert_eq!(ws.tabs().len(), 1);
    assert_eq!(ws.tabs()[0], t2);
    // The closed tab's panes are gone.
    assert_eq!(ws.panes(t1), None);
    assert_eq!(ws.pane_count(t1), 0);
}

#[test]
fn workspace_switch_tab() {
    let mut ws = Workspace::new();
    let (t1, _p1) = ws.add_tab();
    let (t2, _p2) = ws.add_tab();
    // t1 is active initially.
    assert_eq!(ws.active_tab(), Some(t1));
    // Switch to t2.
    assert!(ws.switch_tab(t2));
    assert_eq!(ws.active_tab(), Some(t2));
    // Switch back to t1.
    assert!(ws.switch_tab(t1));
    assert_eq!(ws.active_tab(), Some(t1));
}

#[test]
fn workspace_active_tab() {
    let mut ws = Workspace::new();
    assert_eq!(ws.active_tab(), None);
    let (t1, _p1) = ws.add_tab();
    assert_eq!(ws.active_tab(), Some(t1));
    let (t2, _p2) = ws.add_tab();
    // Adding a tab does not change the active tab.
    assert_eq!(ws.active_tab(), Some(t1));
    assert!(ws.switch_tab(t2));
    assert_eq!(ws.active_tab(), Some(t2));
}

#[test]
fn workspace_active_pane() {
    let mut ws = Workspace::new();
    assert_eq!(ws.active_pane(), None);
    let (_t1, p1) = ws.add_tab();
    let (t2, p2) = ws.add_tab();
    // The active pane is the active tab's pane.
    assert_eq!(ws.active_pane(), Some(p1));
    assert!(ws.switch_tab(t2));
    assert_eq!(ws.active_pane(), Some(p2));
}

#[test]
fn workspace_tabs() {
    let mut ws = Workspace::new();
    let (t1, _p1) = ws.add_tab();
    let (t2, _p2) = ws.add_tab();
    let (t3, _p3) = ws.add_tab();
    // Tabs are reported in creation order.
    assert_eq!(ws.tabs().len(), 3);
    assert_eq!(ws.tabs()[0], t1);
    assert_eq!(ws.tabs()[1], t2);
    assert_eq!(ws.tabs()[2], t3);
}

#[test]
fn workspace_panes() {
    let mut ws = Workspace::new();
    let (t1, p1) = ws.add_tab();
    let (t2, p2) = ws.add_tab();
    // Each tab has its own single pane; pane IDs are distinct.
    let a = ws.panes(t1).expect("t1 has panes");
    let b = ws.panes(t2).expect("t2 has panes");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0], p1);
    assert_eq!(b.len(), 1);
    assert_eq!(b[0], p2);
    assert_ne!(p1, p2);
    // A tab that was never created has no panes.
    assert_eq!(ws.panes(TabId(999)), None);
}

#[test]
fn workspace_tab_count() {
    let mut ws = Workspace::new();
    assert_eq!(ws.tab_count(), 0);
    let (t1, _p1) = ws.add_tab();
    let (_t2, _p2) = ws.add_tab();
    assert_eq!(ws.tab_count(), 2);
    ws.close_tab(t1);
    assert_eq!(ws.tab_count(), 1);
}

#[test]
fn workspace_pane_count() {
    let mut ws = Workspace::new();
    let (t1, _p1) = ws.add_tab();
    assert_eq!(ws.pane_count(t1), 1);
    // An unknown tab has no panes.
    assert_eq!(ws.pane_count(TabId(999)), 0);
}

#[test]
fn workspace_close_active_tab() {
    let mut ws = Workspace::new();
    let (t1, _p1) = ws.add_tab();
    let (t2, _p2) = ws.add_tab();
    let (t3, _p3) = ws.add_tab();
    // Closing the active tab falls back to the last remaining tab.
    assert!(ws.switch_tab(t1));
    ws.close_tab(t1);
    assert_eq!(ws.active_tab(), Some(t3));
    // Closing a non-active tab leaves the active tab unchanged.
    ws.close_tab(t2);
    assert_eq!(ws.active_tab(), Some(t3));
    // Closing the last remaining tab leaves no active tab or pane.
    ws.close_tab(t3);
    assert_eq!(ws.active_tab(), None);
    assert_eq!(ws.active_pane(), None);
}

#[test]
fn workspace_switch_invalid_tab() {
    let mut ws = Workspace::new();
    let (t1, _p1) = ws.add_tab();
    // Switching to a tab that was never created fails.
    assert!(!ws.switch_tab(TabId(999)));
    assert_eq!(ws.active_tab(), Some(t1));
    // Switching to a closed tab also fails and leaves focus unchanged.
    let (t2, _p2) = ws.add_tab();
    ws.close_tab(t2);
    assert!(!ws.switch_tab(t2));
    assert_eq!(ws.active_tab(), Some(t1));
}

// ---------------------------------------------------------------------------
// N23b: tab UI + routing
// ---------------------------------------------------------------------------

#[test]
fn workspace_tab_shortcuts() {
    let mut ws = Workspace::new();
    let (t1, _p1) = ws.add_tab();
    assert_eq!(ws.active_tab(), Some(t1));
    // Ctrl+T (New): a new tab is created and becomes active.
    let t2 = ws
        .apply_tab_command(TabCommand::New)
        .expect("New returns the new tab");
    assert_eq!(ws.active_tab(), Some(t2));
    assert_eq!(ws.tab_count(), 2);
    // Ctrl+Tab (Next): step to the next tab, wrapping from t2 back to t1.
    assert_eq!(ws.apply_tab_command(TabCommand::Next), Some(t1));
    assert_eq!(ws.active_tab(), Some(t1));
    // Ctrl+Shift+Tab (Prev): step to the previous tab, wrapping from t1 to t2.
    assert_eq!(ws.apply_tab_command(TabCommand::Prev), Some(t2));
    assert_eq!(ws.active_tab(), Some(t2));
    // A tab bar click (Goto): the tab at that index; out of range is a no-op.
    assert_eq!(ws.apply_tab_command(TabCommand::Goto(0)), Some(t1));
    assert_eq!(ws.apply_tab_command(TabCommand::Goto(5)), Some(t1));
    assert_eq!(ws.apply_tab_command(TabCommand::Goto(1)), Some(t2));
    // Ctrl+W (Close): close the active tab (t2); active falls back to t1.
    assert_eq!(ws.apply_tab_command(TabCommand::Close), Some(t1));
    assert_eq!(ws.active_tab(), Some(t1));
    assert_eq!(ws.tab_count(), 1);
}

#[test]
fn workspace_input_routing() {
    let mut ws = Workspace::new();
    let (t1, _p1) = ws.add_tab();
    let (t2, _p2) = ws.add_tab();
    // The active tab is the one the window routes keyboard input to.
    assert_eq!(ws.active_tab(), Some(t1));
    // Switch to t2; input now routes to t2.
    assert!(ws.switch_tab(t2));
    assert_eq!(ws.active_tab(), Some(t2));
    // Switch back to t1; input routes to t1 again.
    assert!(ws.switch_tab(t1));
    assert_eq!(ws.active_tab(), Some(t1));
    // A closed tab can never become active, so input never routes to it.
    ws.close_tab(t2);
    assert!(!ws.has_tab(t2));
    assert_eq!(ws.active_tab(), Some(t1));
}

#[test]
fn workspace_close_reaps() {
    let mut ws = Workspace::new();
    let (t1, p1) = ws.add_tab();
    let (t2, p2) = ws.add_tab();
    // Each tab's pane carries a session handle.
    let s1 = ws.session(p1).expect("t1's pane has a session");
    let s2 = ws.session(p2).expect("t2's pane has a session");
    assert_ne!(s1, s2);
    // Closing t1 reaps its session: the pane and its session are gone.
    ws.close_tab(t1);
    assert_eq!(ws.panes(t1), None);
    assert_eq!(ws.session(p1), None);
    assert_eq!(ws.tab_sessions(t1), Vec::<SessionRef>::new());
    // t2's session is unaffected.
    assert_eq!(ws.session(p2), Some(s2));
    assert_eq!(ws.tab_sessions(t2), vec![s2]);
}

#[test]
fn workspace_stale_events() {
    let mut ws = Workspace::new();
    let (t1, _p1) = ws.add_tab();
    let (t2, _p2) = ws.add_tab();
    // A live tab is not stale.
    assert!(ws.has_tab(t1));
    assert!(ws.has_tab(t2));
    // Closing a tab makes events tagged with it stale: the window ignores
    // them because `has_tab` returns false.
    ws.close_tab(t2);
    assert!(!ws.has_tab(t2));
    assert!(ws.has_tab(t1));
    // A tab that was never created is stale as well.
    assert!(!ws.has_tab(TabId(999)));
}

#[test]
fn workspace_background_flood() {
    // A background tab's snapshot flood coalesces to a single pending
    // snapshot; the slot never builds a queue.
    let mut slot = CoalescingSlot::new();
    assert_eq!(slot.pending_count(), 0);
    assert!(!slot.is_dirty());
    // A flood of snapshots: each push replaces the pending one.
    for i in 0..1000 {
        slot.push(i);
        assert_eq!(slot.pending_count(), 1);
    }
    assert!(slot.is_dirty());
    // Only the latest snapshot survives.
    assert_eq!(slot.take(), Some(999));
    assert_eq!(slot.pending_count(), 0);
    assert!(!slot.is_dirty());
}

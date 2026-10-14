//! Workspace management: tabs, panes, and session routing.
//!
//! The workspace is a pure, headless data structure: it owns the tab and pane
//! tree and the active-tab/active-pane focus, with no I/O, PTY or window
//! dependency. Each tab is a container for one or more panes; each pane is the
//! routing target for one independent session (its own output, history, cwd and
//! session lifecycle). The workspace only *routes* — it never owns the
//! sessions themselves, which the `session` module owns.

use std::collections::HashMap;

/// A unique identifier for a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u32);

/// A unique identifier for a pane within a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u32);

/// A lightweight session handle owned by a pane (N23b).
///
/// The workspace tracks these so [`Workspace::close_tab`] reaps them when a
/// tab is closed; the real session (its PTY, engine and lifecycle) is owned
/// by the `session` module and mapped by this handle. The workspace only
/// routes — it never owns the sessions themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionRef(pub u32);

/// A tab command produced by a keyboard shortcut (N23b).
///
/// The window maps winit keys to these; the workspace applies them to the
/// tab tree. See [`Workspace::apply_tab_command`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabCommand {
    /// Ctrl+T: open a new tab; it becomes active.
    New,
    /// Ctrl+W: close the active tab (reaping its sessions).
    Close,
    /// Ctrl+Tab: switch to the next tab (wrapping).
    Next,
    /// Ctrl+Shift+Tab: switch to the previous tab (wrapping).
    Prev,
}

/// A coalescing snapshot slot (N23b).
///
/// Holds at most one pending snapshot (the latest) plus a dirty flag. A flood
/// of snapshots from a background tab coalesces to a single pending
/// snapshot; it never builds a queue. This is the headless model of the
/// window's per-tab presentation state: replaceable snapshots are coalesced,
/// never terminal output.
#[derive(Debug, Default)]
pub struct CoalescingSlot<T> {
    pending: Option<T>,
    dirty: bool,
}

impl<T> CoalescingSlot<T> {
    /// Create an empty slot.
    pub fn new() -> Self {
        Self {
            pending: None,
            dirty: false,
        }
    }

    /// Push a snapshot; it replaces any pending one (coalescing).
    pub fn push(&mut self, snap: T) {
        self.pending = Some(snap);
        self.dirty = true;
    }

    /// Whether a render is pending (coalesced).
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Number of pending snapshots: always 0 or 1, never a queue.
    pub fn pending_count(&self) -> usize {
        usize::from(self.pending.is_some())
    }

    /// Take the pending snapshot, clearing the dirty flag.
    pub fn take(&mut self) -> Option<T> {
        self.dirty = false;
        self.pending.take()
    }
}

/// The workspace: owns all tabs and panes.
#[derive(Debug, Default)]
pub struct Workspace {
    /// All tabs, in order.
    tabs: Vec<TabId>,
    /// The active tab.
    active_tab: Option<TabId>,
    /// The next tab ID to assign.
    next_tab_id: u32,
    /// The next pane ID to assign.
    next_pane_id: u32,
    /// Panes per tab.
    panes_by_tab: HashMap<TabId, Vec<PaneId>>,
    /// The active pane per tab.
    active_pane_by_tab: HashMap<TabId, PaneId>,
    /// The next session handle to assign (N23b).
    next_session: u32,
    /// Session handles per pane (N23b); [`Workspace::close_tab`] reaps them.
    session_by_pane: HashMap<PaneId, SessionRef>,
}

impl Workspace {
    /// Create a new workspace.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new tab with one pane.
    /// Returns the new tab ID and pane ID.
    ///
    /// Each pane is assigned a [`SessionRef`] (N23b); the window/pump maps it
    /// to a real session.
    pub fn add_tab(&mut self) -> (TabId, PaneId) {
        let tab_id = TabId(self.next_tab_id);
        self.next_tab_id += 1;

        let pane_id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;

        // N23b: each pane owns one session handle; `close_tab` reaps it.
        let session = SessionRef(self.next_session);
        self.next_session += 1;

        self.tabs.push(tab_id);
        self.panes_by_tab.insert(tab_id, vec![pane_id]);
        self.active_pane_by_tab.insert(tab_id, pane_id);
        self.session_by_pane.insert(pane_id, session);

        if self.active_tab.is_none() {
            self.active_tab = Some(tab_id);
        }

        (tab_id, pane_id)
    }

    /// Close a tab and all its panes.
    ///
    /// Reaps the tab's panes' session handles (N23b): after this,
    /// [`Workspace::session`] returns `None` for each of the tab's panes.
    pub fn close_tab(&mut self, tab_id: TabId) {
        // N23b: reap the tab's panes' session handles.
        if let Some(panes) = self.panes_by_tab.remove(&tab_id) {
            for pane in panes {
                self.session_by_pane.remove(&pane);
            }
        }
        self.active_pane_by_tab.remove(&tab_id);

        if let Some(pos) = self.tabs.iter().position(|t| *t == tab_id) {
            self.tabs.remove(pos);
        }

        // Update active tab
        if self.active_tab == Some(tab_id) {
            self.active_tab = self.tabs.last().copied();
        }
    }

    /// Switch to a different tab.
    pub fn switch_tab(&mut self, tab_id: TabId) -> bool {
        if self.tabs.contains(&tab_id) {
            self.active_tab = Some(tab_id);
            true
        } else {
            false
        }
    }

    /// Get the active tab.
    pub fn active_tab(&self) -> Option<TabId> {
        self.active_tab
    }

    /// Get the active pane for the active tab.
    pub fn active_pane(&self) -> Option<PaneId> {
        self.active_tab
            .and_then(|tab| self.active_pane_by_tab.get(&tab).copied())
    }

    /// Get all tabs in order.
    pub fn tabs(&self) -> &[TabId] {
        &self.tabs
    }

    /// Get the panes for a tab.
    pub fn panes(&self, tab_id: TabId) -> Option<&[PaneId]> {
        self.panes_by_tab.get(&tab_id).map(|v| v.as_slice())
    }

    /// Get the number of tabs.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Get the number of panes in a tab.
    pub fn pane_count(&self, tab_id: TabId) -> usize {
        self.panes_by_tab.get(&tab_id).map(|v| v.len()).unwrap_or(0)
    }

    /// Apply a tab command (N23b) to the tab tree and return the active tab
    /// afterwards (if any).
    ///
    /// This is the workspace half of the tab shortcuts: the window maps
    /// Ctrl+T / Ctrl+W / Ctrl+Tab / Ctrl+Shift+Tab to a [`TabCommand`] and
    /// applies it here.
    pub fn apply_tab_command(&mut self, cmd: TabCommand) -> Option<TabId> {
        match cmd {
            TabCommand::New => {
                let (tab, _pane) = self.add_tab();
                self.active_tab = Some(tab);
                Some(tab)
            }
            TabCommand::Close => {
                let active = self.active_tab?;
                self.close_tab(active);
                self.active_tab
            }
            TabCommand::Next => self.step_tab(1),
            TabCommand::Prev => self.step_tab(-1),
        }
    }

    /// Step to the next (`delta` positive) or previous (`delta` negative)
    /// tab, wrapping around (N23b). Returns the active tab afterwards.
    fn step_tab(&mut self, delta: i32) -> Option<TabId> {
        let current = self.active_tab?;
        if self.tabs.len() <= 1 {
            return self.active_tab;
        }
        let tabs: Vec<TabId> = self.tabs.to_vec();
        let idx = tabs.iter().position(|t| *t == current)?;
        let next = ((idx as i32) + delta).rem_euclid(tabs.len() as i32) as usize;
        self.active_tab = Some(tabs[next]);
        self.active_tab
    }

    /// The [`SessionRef`] for `pane` (N23b), or `None` if the pane (and its
    /// session) is gone.
    pub fn session(&self, pane: PaneId) -> Option<SessionRef> {
        self.session_by_pane.get(&pane).copied()
    }

    /// The session handles owned by `tab`, in pane order (N23b).
    pub fn tab_sessions(&self, tab: TabId) -> Vec<SessionRef> {
        self.panes_by_tab
            .get(&tab)
            .map(|panes| {
                panes
                    .iter()
                    .filter_map(|p| self.session_by_pane.get(p).copied())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether `tab` still exists in the workspace (N23b).
    ///
    /// A pump event tagged with a closed tab is stale and must be ignored.
    pub fn has_tab(&self, tab: TabId) -> bool {
        self.tabs.contains(&tab)
    }
}

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
}

impl Workspace {
    /// Create a new workspace.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new tab with one pane.
    /// Returns the new tab ID and pane ID.
    pub fn add_tab(&mut self) -> (TabId, PaneId) {
        let tab_id = TabId(self.next_tab_id);
        self.next_tab_id += 1;

        let pane_id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;

        self.tabs.push(tab_id);
        self.panes_by_tab.insert(tab_id, vec![pane_id]);
        self.active_pane_by_tab.insert(tab_id, pane_id);

        if self.active_tab.is_none() {
            self.active_tab = Some(tab_id);
        }

        (tab_id, pane_id)
    }

    /// Close a tab and all its panes.
    pub fn close_tab(&mut self, tab_id: TabId) {
        self.panes_by_tab.remove(&tab_id);
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
}

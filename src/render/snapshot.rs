//! Pooled owned screen snapshots.
//!
//! At most two [`Snapshot`]s are alive at once: the complete current
//! snapshot and exactly one reusable replacement (architecture 03:90,
//! "View snapshots"). All pool operations are owned-value moves; no
//! engine borrow or terminal lock crosses the pool boundary.

use crate::engine::Snapshot;

/// Owns at most two complete screen snapshots: the `current` snapshot and
/// exactly one reusable replacement (`spare`). The pool never grows beyond
/// two slots.
pub struct SnapshotPool {
    current: Option<Snapshot>,
    spare: Option<Snapshot>,
}

impl SnapshotPool {
    /// An empty pool: no snapshots alive.
    pub fn new() -> Self {
        Self {
            current: None,
            spare: None,
        }
    }

    /// The complete current snapshot, if the slot is occupied.
    pub fn current(&self) -> Option<&Snapshot> {
        self.current.as_ref()
    }

    /// The reusable replacement slot, if occupied.
    pub fn spare(&self) -> Option<&Snapshot> {
        self.spare.as_ref()
    }

    /// Replace `current` with `snap`, returning the previous current
    /// snapshot (if any) so the caller can place it (e.g. into the spare
    /// slot) or drop it.
    pub fn store_current(&mut self, snap: Snapshot) -> Option<Snapshot> {
        self.current.replace(snap)
    }

    /// Swap the current and spare slots.
    pub fn swap(&mut self) {
        let current = std::mem::take(&mut self.current);
        let spare = std::mem::take(&mut self.spare);
        self.current = spare;
        self.spare = current;
    }

    /// Number of snapshots alive in the pool: 0, 1, or 2 — never more.
    pub fn len(&self) -> usize {
        usize::from(self.current.is_some()) + usize::from(self.spare.is_some())
    }

    /// Whether the pool holds no snapshots.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SnapshotPool {
    fn default() -> Self {
        Self::new()
    }
}

//! Layout management: split panes and deterministic layout (N24a).
//!
//! The layout is a pure, headless data structure: it owns a binary split
//! tree of panes and the active-pane focus, with no I/O, PTY or window
//! dependency, so it is testable without a display. Each leaf is a pane —
//! the routing target for one independent session (its own output, history,
//! cwd and session lifecycle, owned by the `session` module). The layout
//! only *arranges* panes; it never owns the sessions themselves.
//!
//! Rectangles are physical integer cell counts with deterministic remainder
//! allocation: at every split the first child gets `floor(size * ratio)` and
//! the second child gets the remainder. When a split has at least two cells
//! to divide, each child gets at least one (minimum grids; no zero grids).
//! Closing a pane collapses the tree consistently: the closed pane's
//! sibling subtree takes its place.

use std::collections::HashMap;

/// A unique identifier for a pane in the layout tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u32);

/// The direction of a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    /// Horizontal split (panes side by side).
    Horizontal,
    /// Vertical split (panes stacked).
    Vertical,
}

/// A node in the layout tree.
#[derive(Debug, Clone)]
pub enum LayoutNode {
    /// A leaf node (a single pane).
    Leaf(PaneId),
    /// An internal node (a split).
    Split {
        /// The direction of the split.
        direction: SplitDirection,
        /// The fraction of space given to the first child (0.0 to 1.0).
        ratio: f64,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

impl Default for LayoutNode {
    fn default() -> Self {
        LayoutNode::Leaf(PaneId(0))
    }
}

/// The layout tree: owns all panes and their arrangement.
#[derive(Debug)]
pub struct Layout {
    /// The root of the layout tree.
    root: LayoutNode,
    /// The active pane.
    active_pane: PaneId,
    /// The next pane ID to assign.
    next_pane_id: u32,
    /// All panes in the layout; a key is present iff the pane is alive.
    panes: HashMap<PaneId, bool>, // true = alive
}

/// Replace the leaf `target` in `root` with a fresh 50/50 split whose second
/// child is `new_pane` (in place; no pane state is minted here).
fn split_in_place(
    root: &mut LayoutNode,
    target: PaneId,
    direction: SplitDirection,
    new_pane: PaneId,
) {
    match root {
        LayoutNode::Leaf(pane_id) if *pane_id == target => {
            *root = LayoutNode::Split {
                direction,
                ratio: 0.5,
                first: Box::new(LayoutNode::Leaf(*pane_id)),
                second: Box::new(LayoutNode::Leaf(new_pane)),
            };
        }
        LayoutNode::Leaf(_) => {}
        LayoutNode::Split { first, second, .. } => {
            split_in_place(first, target, direction, new_pane);
            split_in_place(second, target, direction, new_pane);
        }
    }
}

/// Collapse the tree in place after a pane has been removed from `panes`.
///
/// A split whose first or second child is now a dead leaf is replaced by
/// its live child, so a dead leaf never survives below the root.
fn close_in_place(root: &mut LayoutNode, panes: &HashMap<PaneId, bool>) {
    let LayoutNode::Split { first, second, .. } = root else {
        return;
    };
    close_in_place(first, panes);
    close_in_place(second, panes);
    let first_dead = matches!(&**first, LayoutNode::Leaf(id) if !panes.contains_key(id));
    let second_dead = matches!(&**second, LayoutNode::Leaf(id) if !panes.contains_key(id));
    if first_dead || second_dead {
        // Replace this split by its live child.
        *root = if first_dead {
            *std::mem::take(second)
        } else {
            *std::mem::take(first)
        };
    }
}

/// Deterministically split `total` cells between two children: the first
/// child gets `floor(total * ratio)` and the second child gets the
/// remainder. When `total >= 2` the first child's share is clamped to
/// `[1, total - 1]`, so neither child is zero (minimum grids; no zero
/// grids). When `total < 2` the first child takes everything (a zero cell
/// for the second child is then unavoidable).
fn split_size(total: u16, ratio: f64) -> u16 {
    if total < 2 {
        return total;
    }
    let first = (total as f64 * ratio).floor() as u16;
    first.clamp(1, total - 1)
}

impl Layout {
    /// Create a new layout with a single pane.
    pub fn new() -> Self {
        let pane_id = PaneId(0);
        Self {
            root: LayoutNode::Leaf(pane_id),
            active_pane: pane_id,
            next_pane_id: 1,
            panes: HashMap::from([(pane_id, true)]),
        }
    }

    /// Split the active pane horizontally.
    /// Returns the new pane ID.
    pub fn split_horizontal(&mut self) -> PaneId {
        self.split(SplitDirection::Horizontal)
    }

    /// Split the active pane vertically.
    /// Returns the new pane ID.
    pub fn split_vertical(&mut self) -> PaneId {
        self.split(SplitDirection::Vertical)
    }

    /// Split the active pane in `direction`, minting a new pane that
    /// becomes active. If the layout is empty (all panes closed), the new
    /// pane re-roots the tree instead.
    fn split(&mut self, direction: SplitDirection) -> PaneId {
        let new_pane_id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;

        if self.panes.contains_key(&self.active_pane) {
            // The active pane is alive: split it in place.
            split_in_place(&mut self.root, self.active_pane, direction, new_pane_id);
        } else {
            // The layout is empty: re-root at the new pane.
            self.root = LayoutNode::Leaf(new_pane_id);
        }
        self.panes.insert(new_pane_id, true);

        // Focus the new pane.
        self.active_pane = new_pane_id;

        new_pane_id
    }

    /// Close a pane.
    ///
    /// If the pane is in a split, the split collapses and the sibling
    /// subtree takes the closed pane's place. If the closed pane was
    /// active, focus falls back to the first remaining pane (the leftmost
    /// live leaf); if the layout becomes empty, the active pane points at
    /// the (now dead) root leaf and [`Layout::pane_count`] is 0.
    pub fn close_pane(&mut self, pane_id: PaneId) {
        if self.panes.remove(&pane_id).is_none() {
            return;
        }

        close_in_place(&mut self.root, &self.panes);

        // If the active pane was closed, focus the first remaining pane.
        if self.active_pane == pane_id {
            self.active_pane = self.first_pane(&self.root);
        }
    }

    /// The leftmost leaf of the tree (deterministic focus fallback).
    fn first_pane(&self, node: &LayoutNode) -> PaneId {
        match node {
            LayoutNode::Leaf(pane_id) => *pane_id,
            LayoutNode::Split { first, .. } => self.first_pane(first),
        }
    }

    /// Focus a pane.
    /// Returns `false` if the pane is not in the layout.
    pub fn focus_pane(&mut self, pane_id: PaneId) -> bool {
        if self.panes.contains_key(&pane_id) {
            self.active_pane = pane_id;
            true
        } else {
            false
        }
    }

    /// Get the active pane.
    pub fn active_pane(&self) -> PaneId {
        self.active_pane
    }

    /// Get all live panes, in ascending ID order (deterministic).
    pub fn panes(&self) -> Vec<PaneId> {
        let mut ids: Vec<PaneId> = self.panes.keys().copied().collect();
        ids.sort();
        ids
    }

    /// Get the number of live panes.
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Compute the rectangles for all live panes, in cells, in tree
    /// (pre-order) order.
    ///
    /// `width` and `height` are the total available cells. The rectangles
    /// are nonoverlapping and tile the given area exactly. Allocation is
    /// deterministic: at every split the first child gets
    /// `floor(size * ratio)` and the second child gets the remainder, and
    /// when a split has at least two cells to divide each child gets at
    /// least one (minimum grids; no zero grids).
    pub fn compute_rectangles(&self, width: u16, height: u16) -> Vec<(PaneId, u16, u16, u16, u16)> {
        let mut rects = Vec::new();
        self.compute_rectangles_node(&self.root, 0, 0, width, height, &mut rects);
        rects
    }

    fn compute_rectangles_node(
        &self,
        node: &LayoutNode,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        rects: &mut Vec<(PaneId, u16, u16, u16, u16)>,
    ) {
        match node {
            LayoutNode::Leaf(pane_id) => {
                if self.panes.contains_key(pane_id) {
                    rects.push((*pane_id, x, y, width, height));
                }
            }
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => match direction {
                SplitDirection::Horizontal => {
                    let first_width = split_size(width, *ratio);
                    let second_width = width - first_width;

                    self.compute_rectangles_node(first, x, y, first_width, height, rects);
                    self.compute_rectangles_node(
                        second,
                        x + first_width,
                        y,
                        second_width,
                        height,
                        rects,
                    );
                }
                SplitDirection::Vertical => {
                    let first_height = split_size(height, *ratio);
                    let second_height = height - first_height;

                    self.compute_rectangles_node(first, x, y, width, first_height, rects);
                    self.compute_rectangles_node(
                        second,
                        x,
                        y + first_height,
                        width,
                        second_height,
                        rects,
                    );
                }
            },
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::new()
    }
}

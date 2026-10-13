//! Pure pump policy for the live loop (N08b2a).
//!
//! The window loop (N08b2b) asks [`render_policy`] what to do after each
//! live-loop tick: it is deterministic and side-effect free — no I/O, no
//! clocks, no terminal types — so every tick decision is headless and
//! unit-testable.

/// One live-loop tick's decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderPolicy {
    /// The session is not yet terminated — keep pumping.
    pub pump: bool,
    /// A snapshot was fetched this tick — request one window redraw.
    pub redraw: bool,
}

/// Pump while the session lives; redraw only when this tick fetched a
/// snapshot.
pub fn render_policy(terminated: bool, snapshot_taken: bool) -> RenderPolicy {
    let pump = !terminated;
    RenderPolicy {
        pump,
        redraw: pump && snapshot_taken,
    }
}

/// N11a staleness tag: a fetched snapshot whose geometry differs from the
/// window's current grid must never be presented (a resize may have landed
/// between the capture and the render attempt). The geometry pair is the
/// staleness tag in this single-window architecture; `Snapshot.epoch`
/// remains the per-capture monotonic id and is not consulted here.
pub fn snapshot_is_stale(snap_rows: u16, snap_cols: u16, grid_rows: u16, grid_cols: u16) -> bool {
    snap_rows != grid_rows || snap_cols != grid_cols
}

/// N11a zero retention: a zero on either axis keeps the previous value for
/// that axis (a tiny/minimized window never requests a zero row or column);
/// a non-zero axis takes the new value.
pub fn clamp_grid(rows: u16, cols: u16, prev_rows: u16, prev_cols: u16) -> (u16, u16) {
    (
        if rows == 0 { prev_rows } else { rows },
        if cols == 0 { prev_cols } else { cols },
    )
}

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

//! Pure render-loop state machine (N08b1).
//!
//! The window loop (N08b2) folds its inputs — dirty pulses, window
//! resizes, zero-size surfaces, GPU surface status, child exit — into
//! [`RenderState`] through [`on_event`], which is deterministic and
//! side-effect free: no clocks, no global state, no wgpu types (the
//! `CurrentSurfaceTexture` mapping happens at the boundary in N08b2).
//! Every window frame therefore has a headless, unit-testable model.

/// Status of acquiring the current surface texture (N08b2 maps the
/// pinned wgpu `CurrentSurfaceTexture` variants 1:1 into these;
/// wgpu's `Validation` is payload-less, so nora carries the message
/// in its own `String` payload at that boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceStatus {
    /// Acquired a usable texture.
    Success,
    /// Acquired a texture whose size differs from the configured
    /// window size: one `Reconfigure` pass is enough, then render.
    Suboptimal,
    /// Acquire timed out: no retry budget; the frame is skipped until
    /// the next event.
    Timeout,
    /// The window is hidden: no retry budget; skip the frame.
    Occluded,
    /// The window was destroyed or its configuration is stale: counts
    /// against the retry budget.
    Outdated,
    /// The surface is gone: counts against the retry budget.
    Lost,
    /// Acquire failed validation: counts against the retry budget.
    Validation(String),
}

/// Folded render-loop state. All fields are either owned or small
/// integers: no engine lifetimes, no terminal locks, no GPU types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderState {
    /// A non-noop damage report is pending from the engine (a dirty
    /// pulse carrying real damage, or a resize that forces
    /// `Damage::Full`).
    pub dirty: bool,
    /// The window's surface is zero-sized: rendering is skipped until
    /// a non-zero resize arrives.
    pub zero_size: bool,
    /// Failed-surface retry budget consumed so far; the failure that
    /// brings this to `3` exits with `ExitFail`.
    pub surface_retry: u8,
}

/// Inputs the window loop folds into [`RenderState`] (N08b2 maps window
/// and GPU events into these).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderEvent {
    /// The engine produced a new report: `non_noop` when the damage is
    /// real (wider than the pinned cursor-cell no-op shape or `Full`);
    /// a dirty pulse with no pending damage leaves the state idle.
    Dirty { non_noop: bool },
    /// The window was resized (or a `Reconfigure` applied a new
    /// configuration): recompute the surface, and the following render
    /// attempt renders (`dirty` stays set).
    Resize,
    /// The window's surface is zero-sized (or became usable again).
    ZeroSize { zero: bool },
    /// The GPU reported a surface-acquire status.
    SurfaceStatus(SurfaceStatus),
    /// The PTY session's child exited.
    ChildExited,
}

/// What the window loop must do after each folded event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Render the pending snapshot.
    Render,
    /// Reconfigure the window/surface and try again (counts toward the
    /// retry budget only on `Outdated`/`Lost`/`Validation`).
    Reconfigure,
    /// The surface is lost: create a new one for the window (counts
    /// toward the retry budget).
    Recreate,
    /// Retry budget exhausted: stop the loop with a failure.
    ExitFail,
    /// Shut down cleanly (child exited).
    Close,
    /// No work: wait for the next event.
    Skip,
}

/// Fold one event into the state. Deterministic and pure: the same
/// event sequence always yields the same state/action pair.
///
/// Semantics (N08b1 block):
/// - `Dirty { non_noop: true }` sets `dirty` and renders unless the
///   surface is zero-sized (skip); `Dirty { non_noop: false }` leaves
///   the state idle.
/// - `Resize` sets `dirty` and reconfigures; the next attempt renders.
/// - `ZeroSize { zero }` only records; it never renders by itself.
/// - Surface status: `Success` renders when dirty (skip otherwise);
///   `Suboptimal` reconfigures (the next attempt renders);
///   `Outdated`/`Lost`/`Validation` consume one retry each
///   (`Reconfigure`; `ExitFail` at 3); sustained `Timeout`/`Occluded`
///   skip the frame with NO retry increment.
/// - `ChildExited` closes.
///
/// `dirty` is only set, never cleared here: the loop feeds
/// `Dirty { non_noop: false }` when its poll finds nothing pending,
/// and a failed attempt must still carry the pending report into the
/// next attempt ("after a `Reconfigure` the next attempt renders").
pub fn on_event(state: RenderState, ev: RenderEvent) -> (RenderState, Action) {
    let mut state = state;
    let action = match ev {
        RenderEvent::Dirty { non_noop } => {
            if non_noop {
                state.dirty = true;
                if state.zero_size {
                    Action::Skip
                } else {
                    Action::Render
                }
            } else {
                Action::Skip
            }
        }
        RenderEvent::Resize => {
            // A resize carries a usable (non-zero) window size.
            state.dirty = true;
            state.zero_size = false;
            Action::Reconfigure
        }
        RenderEvent::ZeroSize { zero } => {
            state.zero_size = zero;
            Action::Skip
        }
        RenderEvent::SurfaceStatus(status) => match status {
            SurfaceStatus::Success => {
                // A good acquire ends any recovery streak: the retry
                // budget bounds consecutive failures, not a session total.
                state.surface_retry = 0;
                if state.dirty && !state.zero_size {
                    Action::Render
                } else {
                    Action::Skip
                }
            }
            // The surface size no longer matches the window: reconfigure,
            // then the next attempt renders.
            // A suboptimal frame is still presentable: render it, and the
            // caller reconfigures after presenting. Reconfiguring first and
            // re-requesting a frame could loop while the compositor keeps
            // reporting Suboptimal.
            SurfaceStatus::Suboptimal => {
                state.surface_retry = 0;
                if state.dirty && !state.zero_size {
                    Action::Render
                } else {
                    Action::Reconfigure
                }
            }
            // No retry budget: the frame is skipped until the next event.
            SurfaceStatus::Timeout | SurfaceStatus::Occluded => Action::Skip,
            SurfaceStatus::Outdated | SurfaceStatus::Lost | SurfaceStatus::Validation(_) => {
                state.surface_retry += 1;
                if state.surface_retry >= 3 {
                    Action::ExitFail
                } else if status == SurfaceStatus::Lost {
                    Action::Recreate
                } else {
                    Action::Reconfigure
                }
            }
        },
        RenderEvent::ChildExited => Action::Close,
    };
    (state, action)
}

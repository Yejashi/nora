//! N08b1 tests for the pure render-loop state machine
//! (`nora::render::state::on_event`): dirty pulse vs idle, the
//! surface-retry budget (only `Outdated`/`Lost`/`Validation` count),
//! zero-size skipping and child-exit close. Pure and headless: no
//! window, no threads, no clocks.

use nora::render::state::{Action, RenderEvent, RenderState, SurfaceStatus, on_event};

fn default_state() -> RenderState {
    RenderState::default()
}

/// A dirty pulse with a pending non-noop snapshot renders.
#[test]
fn dirty_pulse_with_pending_report_renders() {
    let (state, action) = on_event(default_state(), RenderEvent::Dirty { non_noop: true });
    assert_eq!(action, Action::Render);
    assert!(state.dirty, "the pulse must mark state dirty");
}

/// An idle pulse (no pending report) is a no-op: no action, no state
/// change.
#[test]
fn idle_pulse_is_noop() {
    let (state, action) = on_event(default_state(), RenderEvent::Dirty { non_noop: false });
    assert_eq!(action, Action::Skip);
    assert_eq!(state, default_state(), "idle must not touch state");
}

/// Retries 1 and 2 reconfigure and the following `Success` renders;
/// retry 3 exits with `ExitFail`.
#[test]
fn surface_retry_budget_exhausts_at_three() {
    // Retry 1.
    let (state, action) = on_event(
        RenderState {
            dirty: true,
            ..default_state()
        },
        RenderEvent::SurfaceStatus(SurfaceStatus::Lost),
    );
    assert_eq!(action, Action::Reconfigure);
    assert_eq!(state.surface_retry, 1);

    // The attempt after reconfigure renders.
    let (state, action) = on_event(state, RenderEvent::SurfaceStatus(SurfaceStatus::Success));
    assert_eq!(action, Action::Render);
    assert_eq!(state.surface_retry, 1, "success must not change the budget");

    // Retry 2.
    let (state, action) = on_event(state, RenderEvent::SurfaceStatus(SurfaceStatus::Lost));
    assert_eq!(action, Action::Reconfigure);
    assert_eq!(state.surface_retry, 2);

    // Retry 3 exhausts the budget.
    let (state, action) = on_event(state, RenderEvent::SurfaceStatus(SurfaceStatus::Outdated));
    assert_eq!(action, Action::ExitFail);
    assert_eq!(state.surface_retry, 3);
}

/// `Validation` counts like the other retryable statuses.
#[test]
fn validation_counts_against_retry_budget() {
    let (state, action) = on_event(
        RenderState {
            dirty: true,
            ..default_state()
        },
        RenderEvent::SurfaceStatus(SurfaceStatus::Validation("bad".into())),
    );
    assert_eq!(action, Action::Reconfigure);
    assert_eq!(state.surface_retry, 1);
}

/// Sustained `Timeout`/`Occluded` skip the frame WITHOUT incrementing
/// the retry budget.
#[test]
fn timeout_and_occluded_never_count() {
    let start = RenderState {
        dirty: true,
        ..default_state()
    };
    for status in [
        SurfaceStatus::Timeout,
        SurfaceStatus::Occluded,
        SurfaceStatus::Timeout,
        SurfaceStatus::Occluded,
    ] {
        let (state, action) = on_event(start.clone(), RenderEvent::SurfaceStatus(status.clone()));
        assert_eq!(action, Action::Skip);
        assert_eq!(state.surface_retry, 0, "{status:?} must not count");
    }
}

/// A window resize reconfigures and marks dirty: the next attempt
/// renders.
#[test]
fn resize_reconfigures_then_next_attempt_renders() {
    let (state, action) = on_event(default_state(), RenderEvent::Resize);
    assert_eq!(action, Action::Reconfigure);
    assert!(state.dirty, "the resize forces Damage::Full: dirty");

    let (_, action) = on_event(state, RenderEvent::SurfaceStatus(SurfaceStatus::Success));
    assert_eq!(action, Action::Render);
}

/// `Suboptimal` reconfigures (the next attempt renders).
#[test]
fn suboptimal_reconfigures() {
    let (state, action) = on_event(
        RenderState {
            dirty: true,
            ..default_state()
        },
        RenderEvent::SurfaceStatus(SurfaceStatus::Suboptimal),
    );
    assert_eq!(action, Action::Reconfigure);
    assert_eq!(
        state.surface_retry, 0,
        "suboptimal must not consume the budget"
    );
}

/// A zero-sized surface skips the render even when dirty; a later
/// non-zero resize renders.
#[test]
fn zero_size_skips_render_until_resized() {
    let (state, action) = on_event(default_state(), RenderEvent::ZeroSize { zero: true });
    assert_eq!(action, Action::Skip);
    assert!(state.zero_size);

    let (state, action) = on_event(state, RenderEvent::Dirty { non_noop: true });
    assert_eq!(action, Action::Skip, "zero-size: no Render");
    assert!(state.dirty, "the report stays pending");

    let (state, action) = on_event(state, RenderEvent::Resize);
    assert_eq!(action, Action::Reconfigure);

    let (state, action) = on_event(state, RenderEvent::SurfaceStatus(SurfaceStatus::Success));
    assert_eq!(action, Action::Render);
    assert!(!state.zero_size);
}

/// Child exit closes the loop.
#[test]
fn child_exit_closes() {
    let (state, action) = on_event(default_state(), RenderEvent::ChildExited);
    assert_eq!(action, Action::Close);
    assert_eq!(state, default_state(), "exit must not touch state");
}

/// Success while idle renders nothing.
#[test]
fn success_while_idle_skips() {
    let (state, action) = on_event(
        default_state(),
        RenderEvent::SurfaceStatus(SurfaceStatus::Success),
    );
    assert_eq!(action, Action::Skip);
    assert_eq!(state, default_state());
}

//! N16a: pure focus-event encoding.
//!
//! Translates a pane/OS focus change into the terminal's focus report,
//! with no terminal state involved, so it is headless-testable. The
//! window layer (N16b) routes focus changes to the active session and
//! calls [`encode_focus_event`] with the session's focus-reporting flag
//! (the engine's `TermMode::FOCUS_IN_OUT`).

/// Encode a focus event into the terminal's focus report (N16a).
///
/// Returns `None` if focus reporting is not enabled. Focus in is
/// `CSI I`, focus out is `CSI O` (xterm focus reporting, pinned from
/// the xterm ctlseqs [S18]).
pub fn encode_focus_event(enabled: bool, focused: bool) -> Option<String> {
    if !enabled {
        return None;
    }
    Some(if focused {
        "\x1b[I".to_string()
    } else {
        "\x1b[O".to_string()
    })
}

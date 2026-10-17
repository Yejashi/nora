//! ADR-013 step 1: the shell integration's command-line reports.
//!
//! - `OSC 133 ; C ; cmdline=` (shell-quoted) / `cmdline_url=`
//!   (percent-encoded) carry the executed command line; `OSC 133 ; D ;
//!   <status>` its exit status; other marker parameters are ignored.
//! - `OSC 6973 ; buffer ; <cursor> ; <base64>` reports the line editor's
//!   buffer, `OSC 6973 ; end` its end.
//! - The buffer is current only after the input marker `B` and before the
//!   command starts.

use base64::Engine as _;
use nora::engine::Engine;
use nora::prompt_marker::{MarkerKind, PromptMarkerEvent, PromptMarkerFilter, ShellEvent};
use nora::shell_input::{LineBuffer, MAX_EXECUTED, ShellInput, unquote_shell};

fn osc(data: &str) -> Vec<u8> {
    let mut out = b"\x1b]".to_vec();
    out.extend_from_slice(data.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

fn buffer_report(text: &str, cursor: usize) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    osc(&format!("6973;buffer;{cursor};{b64}"))
}

fn marker(kind: MarkerKind) -> ShellEvent {
    ShellEvent::Marker(PromptMarkerEvent { kind })
}

fn events(bytes: &[u8]) -> (Vec<u8>, Vec<ShellEvent>) {
    let (forward, events) = PromptMarkerFilter::new().process(bytes);
    (forward, events.into_iter().map(|(_, e)| e).collect())
}

#[test]
fn command_line_from_cmdline_url_and_cmdline() {
    let (forward, got) = events(&osc("133;C;cmdline_url=ls%20-la%20%2Ftmp%2Fcaf%C3%A9"));
    assert!(forward.is_empty(), "the frame is suppressed");
    assert_eq!(
        got,
        vec![
            marker(MarkerKind::C),
            ShellEvent::CommandLine("ls -la /tmp/café".to_string())
        ]
    );
    let (_, got) = events(&osc(r"133;C;cmdline=git\ commit\ -m\ $'one\ntwo'"));
    assert_eq!(
        got,
        vec![
            marker(MarkerKind::C),
            ShellEvent::CommandLine("git commit -m one\ntwo".to_string())
        ]
    );
}

#[test]
fn exit_status_and_ignored_parameters() {
    let (forward, got) = events(&osc("133;D;127"));
    assert!(forward.is_empty());
    assert_eq!(
        got,
        vec![marker(MarkerKind::D), ShellEvent::CommandExit(127)]
    );
    // Parameters nora does not use (kitty's `k=s`, a bare `C`, a
    // non-numeric status) still yield the marker alone.
    assert_eq!(events(&osc("133;A;k=s")).1, vec![marker(MarkerKind::A)]);
    assert_eq!(events(&osc("133;C")).1, vec![marker(MarkerKind::C)]);
    assert_eq!(events(&osc("133;D;x")).1, vec![marker(MarkerKind::D)]);
    // An unknown kind is still forwarded unchanged.
    let frame = osc("133;Z;cmdline=x");
    assert_eq!(events(&frame), (frame.clone(), vec![]));
}

#[test]
fn buffer_reports_parse_and_malformed_ones_forward() {
    let (forward, got) = events(&buffer_report("git st", 6));
    assert!(forward.is_empty());
    assert_eq!(
        got,
        vec![ShellEvent::Buffer(LineBuffer {
            text: "git st".to_string(),
            cursor: 6
        })]
    );
    assert_eq!(events(&osc("6973;end")).1, vec![ShellEvent::BufferEnd]);
    for bad in [
        "6973;buffer;x;Z2l0",
        "6973;buffer;1;***",
        "6973;buffer;1",
        "6973;other",
    ] {
        let frame = osc(bad);
        assert_eq!(events(&frame), (frame.clone(), vec![]), "{bad}");
    }
    // Invalid UTF-8 inside the base64 is rejected too.
    let b64 = base64::engine::general_purpose::STANDARD.encode([0xff, 0xfe]);
    let frame = osc(&format!("6973;buffer;0;{b64}"));
    assert_eq!(events(&frame).1, vec![]);
}

/// The buffer is current only between `B` and `C`; `end` clears it; the
/// executed command keeps the cwd of the time and gets its exit status.
#[test]
fn engine_gates_the_buffer_on_the_input_phase() {
    let mut engine = Engine::new(5, 40);
    engine.feed(&osc("7;file:///home/u/proj"));
    engine.feed(&osc("133;A"));
    engine.feed(b"$ ");
    engine.feed(&buffer_report("too early", 9));
    assert_eq!(engine.shell_buffer(), None, "no buffer before B");
    engine.feed(&osc("133;B"));
    engine.feed(&buffer_report("cargo te", 8));
    let buffer = engine.shell_buffer().expect("current after B");
    assert_eq!(buffer.text, "cargo te");
    assert!(buffer.cursor_at_end());
    engine.feed(&buffer_report("cargo test", 3));
    assert_eq!(engine.shell_buffer().unwrap().cursor, 3);
    engine.feed(&osc("6973;end"));
    assert_eq!(engine.shell_buffer(), None, "end clears it");
    engine.feed(&buffer_report("cargo test", 10));
    engine.feed(&osc("133;C;cmdline_url=cargo%20test"));
    assert_eq!(engine.shell_buffer(), None, "the command started");
    assert!(
        engine.take_executed().is_empty(),
        "held until its exit status arrives"
    );
    engine.feed(&osc("133;D;101"));
    let executed = engine.take_executed();
    assert_eq!(executed.len(), 1);
    assert_eq!(executed[0].cmdline, "cargo test");
    assert_eq!(executed[0].cwd.as_deref(), Some("/home/u/proj"));
    assert_eq!(executed[0].exit, Some(101));
    assert!(engine.take_executed().is_empty());
    // The visible text is untouched by the reports.
    let text: String = engine.snapshot().cells.iter().map(|c| c.ch).collect();
    assert!(text.starts_with("$ "), "{text:?}");
    assert!(!text.contains("6973"));
}

#[test]
fn cursor_is_clamped_and_executed_is_bounded() {
    let mut state = ShellInput::new();
    state.apply(&marker(MarkerKind::B), None);
    state.apply(
        &ShellEvent::Buffer(LineBuffer {
            text: "日本".to_string(),
            cursor: 99,
        }),
        None,
    );
    assert_eq!(state.current_buffer().unwrap().cursor, 2, "in characters");
    for i in 0..MAX_EXECUTED + 10 {
        state.apply(&ShellEvent::CommandLine(format!("cmd {i}")), None);
        state.apply(&ShellEvent::CommandExit(0), None);
    }
    let taken = state.take_executed();
    assert_eq!(taken.len(), MAX_EXECUTED);
    assert_eq!(taken[0].cmdline, "cmd 10", "the oldest are dropped");
    let generation = state.generation();
    state.apply(&ShellEvent::Cwd("/x".to_string()), None);
    assert_eq!(state.generation(), generation, "cwd is not input state");
}

#[test]
fn unquote_shell_handles_bash_and_zsh_quoting() {
    assert_eq!(unquote_shell(r"echo\ hi\!").as_deref(), Some("echo hi!"));
    assert_eq!(
        unquote_shell("echo 'a b' \"c \\\"d\\\" \\x\"").as_deref(),
        Some("echo a b c \"d\" \\x")
    );
    assert_eq!(
        unquote_shell(r"printf $'\e[1m\x41\101\t'").as_deref(),
        Some("printf \x1b[1mAA\t")
    );
    assert_eq!(unquote_shell("open 'quote"), None);
    assert_eq!(unquote_shell("trailing\\"), None);
}

/// `$'...'` byte escapes are UTF-8 bytes, not code points: bash in a C
/// locale quotes `é` as `$'\303\251'`.
#[test]
fn unquote_shell_byte_escapes_decode_as_utf8() {
    assert_eq!(
        unquote_shell(r"echo caf$'\303\251'").as_deref(),
        Some("echo café")
    );
    assert_eq!(
        unquote_shell(r"echo $'\xC3\xA9\xf0\x9f\x98\x9c'").as_deref(),
        Some("echo é😜")
    );
    // Code-point escapes stay code points; literal non-ASCII is kept.
    assert_eq!(
        unquote_shell(r"$'é\U0001F61C' 'ü'").as_deref(),
        Some("é😜 ü")
    );
    // An invalid byte sequence is replaced, not dropped silently.
    assert_eq!(unquote_shell(r"$'\xff'x").as_deref(), Some("\u{FFFD}x"));
}

/// An integration that sends a bare `D` (no status, like nora-bash.sh):
/// the command is finished once the next prompt starts, without a status.
#[test]
fn bare_d_completes_at_the_next_prompt() {
    let mut state = ShellInput::new();
    state.apply(&ShellEvent::CommandLine("make".to_string()), Some("/p"));
    state.apply(&marker(MarkerKind::C), None);
    assert!(state.take_executed().is_empty(), "still running");
    state.apply(&marker(MarkerKind::D), None);
    state.apply(&marker(MarkerKind::A), None);
    let taken = state.take_executed();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].exit, None);
    // A late status does not attach to the finished command.
    state.apply(&ShellEvent::CommandExit(3), None);
    assert!(state.take_executed().is_empty());
}

/// The executed-command queue counts each command's directory against
/// its byte cap, and drops a directory longer than `PATH_MAX` (an OSC 7
/// report may be 64 KiB).
#[test]
fn executed_bytes_count_the_directory() {
    use nora::shell_input::{MAX_CWD_BYTES, MAX_EXECUTED_BYTES};
    let mut state = ShellInput::new();
    let huge = format!("/{}", "d".repeat(64 * 1024));
    state.apply(&ShellEvent::CommandLine("ls".to_string()), Some(&huge));
    state.apply(&ShellEvent::CommandExit(0), None);
    let taken = state.take_executed();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].cwd, None, "a directory past PATH_MAX is not kept");

    let long = format!("/{}", "d".repeat(MAX_CWD_BYTES - 1));
    for i in 0..MAX_EXECUTED {
        state.apply(&ShellEvent::CommandLine(format!("c{i}")), Some(&long));
        state.apply(&ShellEvent::CommandExit(0), None);
    }
    let taken = state.take_executed();
    let bytes: usize = taken
        .iter()
        .map(|c| c.cmdline.len() + c.cwd.as_ref().map_or(0, String::len))
        .sum();
    assert!(bytes <= MAX_EXECUTED_BYTES, "{bytes} bytes kept");
    assert!(taken.len() < MAX_EXECUTED, "the byte cap bounds the count");
    assert_eq!(taken.last().unwrap().cwd.as_deref(), Some(long.as_str()));
}

//! The window's paste order (`PasteGate`): the clipboard is read on a
//! thread, so input typed after a paste request must follow the pasted
//! text, a second paste queues behind the first, and a failed or stuck
//! read releases the held input.

use std::time::{Duration, Instant};

use nora::layout::PaneId;
use nora::session::pump::{
    INPUT_HEADROOM, PASTE_READ_TIMEOUT, PaneKey, PasteGate, PasteGateAction as A,
};
use nora::workspace::TabId;

fn pane(n: u32) -> PaneKey {
    PaneKey {
        tab: TabId(0),
        pane: PaneId(n),
    }
}

fn input(p: PaneKey, bytes: &[u8]) -> A {
    A::Input {
        pane: p,
        bytes: bytes.to_vec(),
    }
}

fn read_id(actions: &[A]) -> u64 {
    match actions.last() {
        Some(A::Read { read, .. }) => *read,
        other => panic!("expected a read, got {other:?}"),
    }
}

#[test]
fn keys_typed_during_the_read_follow_the_paste() {
    let now = Instant::now();
    let mut gate = PasteGate::new();
    let (p, q) = (pane(0), pane(1));
    let read = read_id(&gate.request(p, now));
    assert!(gate.input(p, b"\r".to_vec(), now).is_empty(), "Enter waits");
    assert_eq!(
        gate.input(q, b"x".to_vec(), now),
        vec![input(q, b"x")],
        "another pane's input is not held"
    );
    assert_eq!(
        gate.read_done(read, Some("echo hi".to_string()), now),
        vec![
            A::Paste {
                pane: p,
                text: "echo hi".to_string()
            },
            input(p, b"\r"),
        ]
    );
    assert!(!gate.reading());
    assert_eq!(gate.input(p, b"y".to_vec(), now), vec![input(p, b"y")]);
}

#[test]
fn a_second_paste_queues_behind_the_first() {
    let now = Instant::now();
    let mut gate = PasteGate::new();
    let p = pane(0);
    let first = read_id(&gate.request(p, now));
    assert!(gate.input(p, b"a".to_vec(), now).is_empty());
    assert!(gate.request(p, now).is_empty(), "queued, not dropped");
    assert!(gate.input(p, b"b".to_vec(), now).is_empty());
    let actions = gate.read_done(first, Some("1".to_string()), now);
    assert_eq!(
        &actions[..2],
        &[
            A::Paste {
                pane: p,
                text: "1".to_string()
            },
            input(p, b"a"),
        ]
    );
    let second = read_id(&actions);
    assert_ne!(second, first);
    assert!(
        gate.input(p, b"c".to_vec(), now).is_empty(),
        "still behind the second"
    );
    assert_eq!(
        gate.read_done(second, Some("2".to_string()), now),
        vec![
            A::Paste {
                pane: p,
                text: "2".to_string()
            },
            input(p, b"b"),
            input(p, b"c"),
        ]
    );
}

#[test]
fn a_failed_read_releases_the_held_input() {
    let now = Instant::now();
    let mut gate = PasteGate::new();
    let p = pane(0);
    let read = read_id(&gate.request(p, now));
    assert!(gate.input(p, b"k".to_vec(), now).is_empty());
    assert_eq!(gate.read_done(read, None, now), vec![input(p, b"k")]);
}

#[test]
fn a_stuck_read_is_abandoned_at_the_next_input() {
    let now = Instant::now();
    let mut gate = PasteGate::new();
    let p = pane(0);
    let read = read_id(&gate.request(p, now));
    assert!(gate.input(p, b"a".to_vec(), now).is_empty());
    let late = now + PASTE_READ_TIMEOUT + Duration::from_millis(1);
    assert_eq!(
        gate.input(p, b"b".to_vec(), late),
        vec![input(p, b"a"), input(p, b"b")]
    );
    assert!(
        gate.read_done(read, Some("late".to_string()), late)
            .is_empty(),
        "an abandoned read's text is dropped"
    );
}

#[test]
fn held_input_is_bounded() {
    let now = Instant::now();
    let mut gate = PasteGate::new();
    let p = pane(0);
    let read = read_id(&gate.request(p, now));
    assert!(gate.input(p, vec![b'k'; INPUT_HEADROOM], now).is_empty());
    assert!(gate.input(p, b"x".to_vec(), now).is_empty(), "refused");
    let actions = gate.read_done(read, None, now);
    assert_eq!(actions, vec![input(p, &vec![b'k'; INPUT_HEADROOM])]);
}

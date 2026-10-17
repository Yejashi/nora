//! A paste that arrives while another is still being fed waits in the
//! pump (`HeldInput`) until the first is fully fed or cancelled: reply
//! segmentation, the paste window and cancellation follow the paste the
//! child is reading, and input typed after the second paste follows it.

use std::time::{Duration, Instant};

use nora::session::pty::{DEFAULT_CAP, LaunchSpec, PasteSegment, PtySession};
use nora::session::pump::{
    HeldInput, MAX_HELD_PASTES, PasteApply, PasteBlock, advance_paste_block, feed_pending_held,
    paste_segment, pump_tick, queue_key_held, queue_paste, release_held, unbracket_paste,
};

const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";

fn bracketed(text: &[u8]) -> Vec<u8> {
    [START, text, END].concat()
}

/// Feed `n` bytes from the front, as the session would take them.
fn feed(pending: &mut Vec<u8>, block: &mut Option<PasteBlock>, n: usize) {
    pending.drain(..n);
    advance_paste_block(block, n);
}

#[test]
fn a_second_paste_waits_for_the_first_with_the_keys_after_it() {
    let mut pending = Vec::new();
    let mut block = None;
    let mut held = HeldInput::default();
    let a = queue_paste(&mut pending, &mut block, &mut held, "first", true);
    assert_eq!(a, PasteApply::Queued(17));
    feed(&mut pending, &mut block, 8); // start marker + "fi"
    assert!(queue_key_held(&mut pending, &mut block, &mut held, b"x"));
    queue_paste(&mut pending, &mut block, &mut held, "second", true);
    assert!(queue_key_held(&mut pending, &mut block, &mut held, b"\r"));
    assert_eq!(held.pastes(), 1);
    // The first paste alone is in the queue, and it decides segmentation.
    assert_eq!(pending, [&b"rst"[..], END, b"x"].concat());
    assert_eq!(paste_segment(&pending, block), PasteSegment::Open);
    // Released only once the first is fully fed.
    release_held(&mut pending, &mut block, &mut held);
    assert_eq!(held.pastes(), 1, "still held while the first is in flight");
    feed(&mut pending, &mut block, 9);
    assert_eq!(block, None);
    release_held(&mut pending, &mut block, &mut held);
    assert!(held.is_empty());
    assert_eq!(pending, [&b"x"[..], &bracketed(b"second"), b"\r"].concat());
    assert_eq!(block.map(|b| (b.start, b.end, b.fed)), Some((1, 19, 0)));
}

#[test]
fn ctrl_c_cancels_the_held_pastes_too_and_keeps_keys_in_order() {
    let mut pending = Vec::new();
    let mut block = None;
    let mut held = HeldInput::default();
    queue_paste(&mut pending, &mut block, &mut held, "first paste", true);
    feed(&mut pending, &mut block, 8);
    queue_paste(&mut pending, &mut block, &mut held, "second", true);
    assert!(queue_key_held(&mut pending, &mut block, &mut held, b"k"));
    queue_paste(&mut pending, &mut block, &mut held, "third", false);
    assert!(queue_key_held(&mut pending, &mut block, &mut held, b"\x03"));
    assert!(held.is_empty(), "every paste before the key is cancelled");
    // The first paste's end marker, the key typed between, then Ctrl+C.
    assert_eq!(pending, [END, b"k", b"\x03"].concat());
    assert_eq!(block.map(|b| (b.start, b.end)), Some((0, 6)));
}

#[test]
fn unbracketing_the_first_paste_keeps_the_second_paste_s_markers() {
    let mut pending = Vec::new();
    let mut block = None;
    let mut held = HeldInput::default();
    queue_paste(&mut pending, &mut block, &mut held, "first", true);
    feed(&mut pending, &mut block, 8);
    queue_paste(&mut pending, &mut block, &mut held, "second", true);
    unbracket_paste(&mut pending, &mut block);
    assert_eq!(pending, b"rst".to_vec(), "the first paste's end marker");
    feed(&mut pending, &mut block, 3);
    release_held(&mut pending, &mut block, &mut held);
    assert_eq!(pending, bracketed(b"second"));
}

#[test]
fn held_pastes_are_bounded() {
    let mut pending = Vec::new();
    let mut block = None;
    let mut held = HeldInput::default();
    queue_paste(&mut pending, &mut block, &mut held, "first", false);
    for _ in 0..MAX_HELD_PASTES {
        let r = queue_paste(&mut pending, &mut block, &mut held, "more", false);
        assert_eq!(r, PasteApply::Queued(4));
    }
    let before = (pending.clone(), held.bytes());
    assert_eq!(
        queue_paste(&mut pending, &mut block, &mut held, "more", false),
        PasteApply::Full
    );
    assert_eq!((pending, held.bytes()), before, "nothing changes");
}

/// Two pastes and a protocol reply while the first is part-fed: the reply
/// goes between a closing and a reopening marker of the first paste, and
/// the second paste follows the first whole.
#[test]
fn a_reply_during_the_first_of_two_pastes_stays_out_of_both() {
    let dir = std::env::temp_dir().join(format!("nora-paste-queue-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let head = dir.join("head");
    let rest = dir.join("rest");
    let script = format!(
        "stty raw -echo; printf '\\033[?2004hREADY\\r\\n'; \
         dd bs=1 count=10 of={} 2>/dev/null; printf '\\033[6n'; cat > {}",
        head.display(),
        rest.display()
    );
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        DEFAULT_CAP,
    )
    .expect("spawn sh over a real PTY");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let outcome = pump_tick(&mut session);
        let text: Option<String> = outcome
            .snapshot
            .map(|snap| snap.cells.iter().map(|c| c.ch).collect());
        if text.is_some_and(|t| t.contains("READY")) {
            break;
        }
        assert!(Instant::now() < deadline, "the child got ready");
        std::thread::sleep(Duration::from_millis(10));
    }

    // More than the paste window, so the first is still being fed when
    // the second arrives and when the reply comes.
    let first: Vec<u8> = (0..200 * 1024).map(|i| b'a' + (i % 26) as u8).collect();
    let second: Vec<u8> = (0..64 * 1024).map(|i| b'A' + (i % 26) as u8).collect();
    let mut pending = Vec::new();
    let mut block = None;
    let mut held = HeldInput::default();
    queue_paste(
        &mut pending,
        &mut block,
        &mut held,
        std::str::from_utf8(&first).unwrap(),
        true,
    );
    feed_pending_held(&mut session, &mut pending, &mut block, &mut held);
    queue_paste(
        &mut pending,
        &mut block,
        &mut held,
        std::str::from_utf8(&second).unwrap(),
        true,
    );
    assert!(queue_key_held(&mut pending, &mut block, &mut held, b"!"));
    let expected = [bracketed(&first), bracketed(&second), b"!".to_vec()].concat();

    let deadline = Instant::now() + Duration::from_secs(10);
    let got = loop {
        feed_pending_held(&mut session, &mut pending, &mut block, &mut held);
        let _ = session.flush(1024 * 1024);
        let _ = session.poll_events();
        let got = [
            std::fs::read(&head).unwrap_or_default(),
            std::fs::read(&rest).unwrap_or_default(),
        ]
        .concat();
        if pending.is_empty() && held.is_empty() && got.ends_with(b"!") {
            break got;
        }
        assert!(Instant::now() < deadline, "the pastes arrived");
        std::thread::sleep(Duration::from_millis(5));
    };
    session.close();
    let _ = std::fs::remove_dir_all(&dir);

    let reply_at = (0..got.len())
        .find(|&i| {
            got[i..].starts_with(END)
                && got[i + END.len()..].starts_with(b"\x1b[")
                && !got[i + END.len()..].starts_with(START)
        })
        .expect("a reply after a closing marker");
    assert!(
        reply_at < START.len() + first.len(),
        "the reply came during the first paste"
    );
    let after = &got[reply_at + END.len()..];
    let reply_len = after.iter().position(|&b| b == b'R').expect("a CPR reply") + 1;
    assert!(
        after[reply_len..].starts_with(START),
        "the first paste reopens after the reply"
    );
    let without: Vec<u8> = [&got[..reply_at], &after[reply_len + START.len()..]].concat();
    assert_eq!(without.len(), expected.len());
    assert!(without == expected, "both pastes whole and in order");
}

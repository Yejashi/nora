//! A protocol reply produced while a bracketed paste is part-fed is not
//! sent inside the paste: the paste is closed (`CSI 201 ~`), the reply
//! follows, and the paste reopens (`CSI 200 ~`) for the rest of its text
//! (docs/06-compatibility.md, bounded bracketed segments).

use std::time::{Duration, Instant};

use nora::session::pty::{DEFAULT_CAP, LaunchSpec, PasteSegment, PtySession};
use nora::session::pump::{
    PasteBlock, advance_paste_block, apply_paste, feed_pending, paste_segment, pump_tick,
    unbracket_paste,
};

const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";

#[test]
fn segment_follows_the_fed_position() {
    let mut pending = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, "a\u{e9}b", true);
    assert_eq!(paste_segment(&pending, block), PasteSegment::Closed);
    let feed = |pending: &mut Vec<u8>, block: &mut Option<PasteBlock>, n: usize| {
        pending.drain(..n);
        advance_paste_block(block, n);
        paste_segment(pending, *block)
    };
    assert_eq!(
        feed(&mut pending, &mut block, 3),
        PasteSegment::Hold,
        "in the start marker"
    );
    assert_eq!(
        feed(&mut pending, &mut block, 3),
        PasteSegment::Open,
        "after it"
    );
    assert_eq!(
        feed(&mut pending, &mut block, 2),
        PasteSegment::Hold,
        "in a character"
    );
    assert_eq!(feed(&mut pending, &mut block, 1), PasteSegment::Open);
    assert_eq!(
        feed(&mut pending, &mut block, 1),
        PasteSegment::Open,
        "before the end"
    );
    assert_eq!(
        feed(&mut pending, &mut block, 2),
        PasteSegment::Hold,
        "in the end marker"
    );
    assert_eq!(
        feed(&mut pending, &mut block, 4),
        PasteSegment::Closed,
        "done"
    );
    assert_eq!(block, None);

    let mut pending = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, "abc", false);
    pending.drain(..1);
    advance_paste_block(&mut block, 1);
    assert_eq!(
        paste_segment(&pending, block),
        PasteSegment::Closed,
        "ordinary paste"
    );
}

#[test]
fn unbracket_drops_only_the_end_marker() {
    let mut pending = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, "abcd", true);
    pending.extend_from_slice(b"K");
    pending.drain(..8);
    advance_paste_block(&mut block, 8);
    unbracket_paste(&mut pending, &mut block);
    assert_eq!(pending, b"cdK".to_vec());
    assert_eq!(
        block.map(|b| (b.start, b.end, b.bracketed)),
        Some((0, 2, false))
    );
}

/// The bytes a paste of `text` takes when wrapped in bracket markers.
fn bracketed(text: &[u8]) -> Vec<u8> {
    [START, text, END].concat()
}

#[test]
fn reply_during_a_paste_closes_and_reopens_it() {
    let dir = std::env::temp_dir().join(format!("nora-paste-reply-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let head = dir.join("head");
    let rest = dir.join("rest");
    // Raw mode, bracketed paste on; read ten bytes, ask for the cursor
    // position, then keep everything else.
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

    // More than the paste window, so the paste is still being fed when
    // the reply comes.
    let text: Vec<u8> = (0..200 * 1024).map(|i| b'a' + (i % 26) as u8).collect();
    let mut pending = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(
        &mut pending,
        &mut block,
        std::str::from_utf8(&text).unwrap(),
        true,
    );
    let expected = bracketed(&text);

    let deadline = Instant::now() + Duration::from_secs(10);
    let got = loop {
        feed_pending(&mut session, &mut pending, &mut block);
        let _ = session.flush(1024 * 1024);
        let _ = session.poll_events();
        let got = [
            std::fs::read(&head).unwrap_or_default(),
            std::fs::read(&rest).unwrap_or_default(),
        ]
        .concat();
        if pending.is_empty() && got.ends_with(END) && got.len() > expected.len() {
            break got;
        }
        assert!(Instant::now() < deadline, "the paste arrived");
        std::thread::sleep(Duration::from_millis(5));
    };
    session.close();
    let _ = std::fs::remove_dir_all(&dir);

    // Exactly one reply, between a closing and a reopening marker.
    let reply_at = (0..got.len())
        .find(|&i| {
            got[i..].starts_with(END)
                && got[i + END.len()..].starts_with(b"\x1b[")
                && !got[i + END.len()..].starts_with(START)
        })
        .expect("a reply after a closing marker");
    let after = &got[reply_at + END.len()..];
    let reply_len = after.iter().position(|&b| b == b'R').expect("a CPR reply") + 1;
    assert!(
        after[reply_len..].starts_with(START),
        "the paste reopens after the reply"
    );
    let without: Vec<u8> = [&got[..reply_at], &after[reply_len + START.len()..]].concat();
    assert_eq!(without.len(), expected.len());
    assert!(
        without == expected,
        "the paste text is unchanged and in order"
    );
}

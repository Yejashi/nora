//! N20a regression suite for the scrollback search core (`nora::search`).
//!
//! Headless: no PTY, no threads, no GPU, no window. Each test builds a
//! small owned snapshot (`Vec<Vec<char>>`, row 0 = oldest) and asserts
//! the card's acceptance lines on literal matching, match navigation,
//! and generation tagging.

use nora::search::{SearchOutcome, SearchState, check_stale, run_search, search_snapshot};

/// Build a snapshot from raw row strings (row 0 = oldest).
fn snapshot(lines: &[&str]) -> Vec<Vec<char>> {
    lines.iter().map(|line| line.chars().collect()).collect()
}

/// N20a (1): a fresh search state has an empty query, no matches and
/// generation 0; `Default` agrees.
#[test]
fn search_state_new() {
    let state = SearchState::new();
    assert_eq!(state.query, "");
    assert_eq!(state.current, 0);
    assert_eq!(state.total, 0);
    assert_eq!(state.generation, 0);

    let default = SearchState::default();
    assert_eq!(default.query, state.query);
    assert_eq!(default.generation, state.generation);
}

/// N20a (2): setting a query stores it, resets the match state and
/// increments the generation tag.
#[test]
fn search_state_set_query() {
    let mut state = SearchState::new();
    state.set_query("abc".into());
    assert_eq!(state.query, "abc");
    assert_eq!(state.current, 0);
    assert_eq!(state.total, 0);
    assert_eq!(state.generation, 1);

    state.set_query("def".into());
    assert_eq!(state.query, "def");
    assert_eq!(state.current, 0);
    assert_eq!(state.total, 0);
    assert_eq!(state.generation, 2);
}

/// N20a (3): `next` advances and wraps around at the end; with no
/// matches it is a no-op.
#[test]
fn search_state_next() {
    let mut state = SearchState::new();
    state.total = 3;
    state.next();
    assert_eq!(state.current, 1);
    state.next();
    assert_eq!(state.current, 2);
    state.next();
    assert_eq!(
        state.current, 0,
        "next wraps from the last match to the first"
    );
    state.next();
    assert_eq!(state.current, 1);

    let mut empty = SearchState::new();
    empty.next();
    assert_eq!(empty.current, 0, "next with no matches is a no-op");
}

/// N20a (4): `prev` steps back and wraps around at the start; with no
/// matches it is a no-op.
#[test]
fn search_state_prev() {
    let mut state = SearchState::new();
    state.total = 3;
    state.prev();
    assert_eq!(
        state.current, 2,
        "prev from the first match wraps to the last"
    );
    state.prev();
    assert_eq!(state.current, 1);
    state.prev();
    assert_eq!(state.current, 0);
    state.prev();
    assert_eq!(state.current, 2);

    let mut empty = SearchState::new();
    empty.prev();
    assert_eq!(empty.current, 0, "prev with no matches is a no-op");
}

/// N20a (5): an empty query matches nothing (no "match every cell").
#[test]
fn search_snapshot_empty_query() {
    let snap = snapshot(&["hello", "world"]);
    assert!(search_snapshot(&snap, "").is_empty());
}

/// N20a (6): a query absent from the snapshot matches nothing.
#[test]
fn search_snapshot_no_match() {
    let snap = snapshot(&["hello", "world"]);
    assert!(search_snapshot(&snap, "xyz").is_empty());
    assert!(search_snapshot(&snap, "hello world").is_empty());
}

/// N20a (7): a query appearing exactly once yields one (row, col) match.
#[test]
fn search_snapshot_single_match() {
    let snap = snapshot(&["the needle is here"]);
    assert_eq!(
        search_snapshot(&snap, "needle"),
        vec![nora::search::SearchResult { row: 0, col: 4 }]
    );
}

/// N20a (8): every occurrence is reported, in row-then-column order,
/// including adjacent overlaps.
#[test]
fn search_snapshot_multiple_matches() {
    let snap = snapshot(&["aaa", "baab", "aa"]);
    assert_eq!(
        search_snapshot(&snap, "aa"),
        vec![
            nora::search::SearchResult { row: 0, col: 0 },
            nora::search::SearchResult { row: 0, col: 1 },
            nora::search::SearchResult { row: 1, col: 1 },
            nora::search::SearchResult { row: 2, col: 0 },
        ]
    );
}

/// N20a (9): matching is per code point, so multi-byte (wide)
/// characters are found exactly like ASCII ones.
#[test]
fn search_snapshot_wide_chars() {
    let snap = snapshot(&["héllo", "日本語"]);
    assert_eq!(
        search_snapshot(&snap, "él"),
        vec![nora::search::SearchResult { row: 0, col: 1 }]
    );
    assert_eq!(
        search_snapshot(&snap, "日本"),
        vec![nora::search::SearchResult { row: 1, col: 0 }]
    );
}

/// N20a (10): each physical row is searched separately — a query
/// spanning a wrap boundary matches nothing, while queries inside a
/// row match as usual.
#[test]
fn search_snapshot_wrapped_text() {
    let snap = snapshot(&["abcdef", "ghijkl"]);
    assert!(search_snapshot(&snap, "defg").is_empty());
    assert_eq!(
        search_snapshot(&snap, "def"),
        vec![nora::search::SearchResult { row: 0, col: 3 }]
    );
    assert_eq!(
        search_snapshot(&snap, "ghi"),
        vec![nora::search::SearchResult { row: 1, col: 0 }]
    );
}

/// N20a (11): every search is tagged with the state's generation, and
/// successive queries carry distinct, monotonically increasing tags.
#[test]
fn search_generation_tag() {
    let snap = snapshot(&["find me"]);
    let mut state = SearchState::new();
    state.set_query("find".into());
    match run_search(&snap, &state) {
        SearchOutcome::Found {
            matches,
            generation,
        } => {
            assert_eq!(matches, vec![nora::search::SearchResult { row: 0, col: 0 }]);
            assert_eq!(generation, 1);
        }
        other => panic!("expected Found, got {other:?}"),
    }

    state.set_query("me".into());
    match run_search(&snap, &state) {
        SearchOutcome::Found {
            matches,
            generation,
        } => {
            assert_eq!(matches, vec![nora::search::SearchResult { row: 0, col: 5 }]);
            assert_eq!(generation, 2);
        }
        other => panic!("expected Found, got {other:?}"),
    }
}

/// N20a (12): a result whose generation no longer matches the state is
/// cancelled (a newer query superseded it); a current result passes
/// through unchanged.
#[test]
fn search_cancelled() {
    let snap = snapshot(&["find me"]);
    let mut state = SearchState::new();
    state.set_query("find".into()); // generation 1
    let stale = run_search(&snap, &state);
    state.set_query("me".into()); // generation 2 supersedes the in-flight search

    match check_stale(stale, &state) {
        SearchOutcome::Cancelled => {}
        other => panic!("stale outcome must be Cancelled, got {other:?}"),
    }

    let current = run_search(&snap, &state);
    match check_stale(current, &state) {
        SearchOutcome::Found {
            matches,
            generation,
        } => {
            assert_eq!(matches, vec![nora::search::SearchResult { row: 0, col: 5 }]);
            assert_eq!(generation, 2);
        }
        other => panic!("current outcome must pass through, got {other:?}"),
    }
}

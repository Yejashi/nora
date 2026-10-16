//! ADR-013 step 3: the suggestion model's ranking, privacy filters, caps
//! and history file.

use nora::suggest::{History, MAX_CMDLINE_BYTES, MAX_ENTRIES};

const NOW: u64 = 1_800_000_000;
const HOUR: u64 = 3_600;
const DAY: u64 = 86_400;

fn run(h: &mut History, cmd: &str, times: u32, cwd: &str, exit: i32, at: u64) {
    for _ in 0..times {
        assert!(h.record(cmd, Some(cwd), Some(exit), at), "{cmd}");
    }
}

#[test]
fn suggests_only_extensions_of_the_buffer() {
    let mut h = History::new();
    run(&mut h, "git status", 1, "/p", 0, NOW);
    assert_eq!(h.suggest("git s", None, NOW), Some("git status"));
    assert_eq!(h.suggest("git status", None, NOW), None, "nothing to add");
    assert_eq!(h.suggest("git x", None, NOW), None);
    assert_eq!(
        h.suggest("", None, NOW),
        None,
        "no suggestion for an empty line"
    );
    assert_eq!(h.suggest("   ", None, NOW), None);
    assert_eq!(h.suggest("Git", None, NOW), None, "case-sensitive");
}

#[test]
fn frequency_and_recency_rank() {
    let mut h = History::new();
    run(&mut h, "cargo build", 5, "/p", 0, NOW - 10 * DAY);
    run(&mut h, "cargo bench", 2, "/p", 0, NOW - HOUR / 2);
    // 5 x 0.5 (older than a week) = 2.5 < 2 x 4 (this hour) = 8.
    assert_eq!(h.suggest("cargo b", None, NOW), Some("cargo bench"));
    run(&mut h, "cargo build", 2, "/p", 0, NOW);
    assert_eq!(h.suggest("cargo b", None, NOW), Some("cargo build"));
}

#[test]
fn current_directory_boosts() {
    let mut h = History::new();
    run(&mut h, "make test", 4, "/a", 0, NOW);
    run(&mut h, "make release", 2, "/b", 0, NOW);
    assert_eq!(h.suggest("make ", Some("/a"), NOW), Some("make test"));
    // 2 x 4 x 3 = 24 in /b beats 4 x 4 = 16.
    assert_eq!(h.suggest("make ", Some("/b"), NOW), Some("make release"));
    assert_eq!(h.suggest("make ", Some("/c"), NOW), Some("make test"));
}

#[test]
fn failing_commands_are_demoted() {
    let mut h = History::new();
    run(&mut h, "npm run buidl", 3, "/p", 1, NOW);
    run(&mut h, "npm run build", 1, "/p", 0, NOW);
    // 3 x 4 x 0.2 = 2.4 < 1 x 4 = 4.
    assert_eq!(h.suggest("npm run bu", None, NOW), Some("npm run build"));
    let entry = h.get("npm run buidl").unwrap();
    assert_eq!((entry.count, entry.failures), (3, 3));
    // A missing exit status (the shell did not report one) is no failure.
    assert!(h.record("ls -la", None, None, NOW));
    assert_eq!(h.get("ls -la").unwrap().failures, 0);
}

#[test]
fn privacy_and_size_filters() {
    let mut h = History::new();
    assert!(
        !h.record(" secret --token abc", None, Some(0), NOW),
        "leading space"
    );
    assert!(!h.record("echo 'a\nb'", None, Some(0), NOW), "multi-line");
    assert!(
        !h.record("printf '\x1b[1m'", None, Some(0), NOW),
        "control characters"
    );
    assert!(!h.record(&"x".repeat(MAX_CMDLINE_BYTES + 1), None, Some(0), NOW));
    assert!(!h.record("   ", None, Some(0), NOW));
    assert!(h.is_empty());
    assert!(
        h.record("ls   ", None, Some(0), NOW),
        "trailing space trimmed"
    );
    assert!(h.get("ls").is_some());
}

#[test]
fn entry_cap_evicts_the_lowest_ranked() {
    let mut h = History::new();
    run(&mut h, "keep me", 50, "/p", 0, NOW);
    for i in 0..MAX_ENTRIES + 20 {
        h.record(
            &format!("cmd {i}"),
            None,
            Some(0),
            NOW - 30 * DAY + i as u64,
        );
    }
    assert_eq!(h.len(), MAX_ENTRIES);
    assert!(h.get("keep me").is_some(), "a frequent command survives");
    assert!(h.get("cmd 0").is_none(), "the oldest single-use one goes");
    assert!(
        h.get(&format!("cmd {}", MAX_ENTRIES + 19)).is_some(),
        "the newest stays"
    );
}

#[test]
fn history_file_round_trips_and_tolerates_damage() {
    let dir = std::env::temp_dir().join(format!("nora-suggest-{}", std::process::id()));
    let path = dir.join("state").join("history");
    let mut h = History::new();
    run(&mut h, "grep -n '%d x' src", 2, "/tmp/a\tb", 1, NOW);
    run(&mut h, "grep -n '%d x' src", 1, "/home/u:x", 0, NOW + 5);
    run(&mut h, "echo café", 1, "/p", 0, NOW);
    assert!(h.is_dirty());
    h.save(&path).expect("save");
    assert!(!h.is_dirty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the history is private");
    }
    let loaded = History::load(&path).expect("load");
    assert_eq!(loaded.len(), 2);
    let grep = loaded.get("grep -n '%d x' src").expect("% survives");
    assert_eq!((grep.count, grep.failures, grep.last_used), (3, 2, NOW + 5));
    let mut dirs = grep.dirs.clone();
    dirs.sort();
    assert_eq!(
        dirs,
        vec![("/home/u:x".to_string(), 1), ("/tmp/a\tb".to_string(), 2)]
    );
    assert!(!loaded.is_dirty());

    // Damaged lines are skipped, the rest loads.
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("not a line\n1\t2\n3\t1\t0\tbad%ZZescape\n4\t1\t0\tok line\n");
    std::fs::write(&path, text).unwrap();
    let loaded = History::load(&path).unwrap();
    assert_eq!(loaded.len(), 3);
    assert!(loaded.get("ok line").is_some());

    // A missing file is an empty history; an unknown header is ignored.
    assert!(History::load(&dir.join("missing")).unwrap().is_empty());
    std::fs::write(&path, "other-format 9\n1\t1\t0\tx\n").unwrap();
    assert!(History::load(&path).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

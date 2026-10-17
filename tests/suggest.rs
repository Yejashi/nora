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

    // A missing or empty file is an empty history.
    assert!(History::load(&dir.join("missing")).unwrap().is_empty());
    std::fs::write(&path, "").unwrap();
    assert!(History::load(&path).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A history file nora cannot read as its format (another header, or not
/// UTF-8) is an error to load and is never saved over, so the first save
/// of a session cannot destroy it.
#[test]
fn unreadable_history_is_never_overwritten() {
    let dir = std::env::temp_dir().join(format!("nora-suggest-keep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("history");
    let other_format: &[u8] = b"nora-history 2\n1\t1\t0\tx\n";
    let not_utf8: &[u8] = b"nora-history 1\n1\t1\t0\t\xff\xfe\n";
    for original in [other_format, not_utf8] {
        std::fs::write(&path, original).unwrap();
        let error = History::load(&path).expect_err("not this format");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let mut h = History::new();
        run(&mut h, "make", 1, "/p", 0, NOW);
        let error = h.save(&path).expect_err("refuses to replace it");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&path).unwrap(), original, "file untouched");
        assert!(h.is_dirty());
    }
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .filter(|name| name != "history")
        .collect();
    assert!(leftovers.is_empty(), "no temporary files: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two nora processes saving in turn keep each other's commands: a save
/// merges the file first (larger count, newest time, directories joined)
/// and writes through its own temporary file.
#[test]
fn saves_from_two_processes_merge() {
    let dir = std::env::temp_dir().join(format!("nora-suggest-merge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("history");
    let mut a = History::new();
    let mut b = History::new();
    run(&mut a, "cargo build", 2, "/a", 0, NOW);
    run(&mut a, "make", 1, "/a", 1, NOW);
    run(&mut b, "git status", 1, "/b", 0, NOW + 10);
    run(&mut b, "make", 3, "/b", 0, NOW + 20);
    a.save(&path).expect("a saves");
    b.save(&path).expect("b saves");
    a.save(&path).expect("a saves again (idempotent)");
    let disk = History::load(&path).unwrap();
    assert_eq!(disk.len(), 3);
    assert_eq!(disk.get("cargo build").unwrap().count, 2);
    assert_eq!(disk.get("git status").unwrap().count, 1);
    let make = disk.get("make").unwrap();
    assert_eq!(
        (make.count, make.failures, make.last_used),
        (3, 0, NOW + 20)
    );
    let mut dirs = make.dirs.clone();
    dirs.sort();
    assert_eq!(dirs, vec![("/a".to_string(), 1), ("/b".to_string(), 3)]);
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names, ["history"], "no temporary files left");
    let _ = std::fs::remove_dir_all(&dir);
}

mod words {
    use nora::suggest::{MAX_DIR_ENTRIES, complete_word, path_executables};

    fn tree(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("nora-words-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["project/src", "project/scripts", "home/docs", "bin"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "project/README.md",
            "project/.hidden",
            "project/src/main.rs",
            "home/docs/notes.txt",
        ] {
            std::fs::write(root.join(file), "").unwrap();
        }
        root
    }

    #[test]
    fn paths_complete_unambiguously() {
        let root = tree("paths");
        let cwd = root.join("project");
        let cwd = cwd.to_str();
        let home = root.join("home");
        let home = home.to_str();
        let none: &[String] = &[];
        assert_eq!(
            complete_word("cat REA", cwd, home, none).as_deref(),
            Some("DME.md")
        );
        assert_eq!(
            complete_word("ls sr", cwd, home, none).as_deref(),
            Some("c/"),
            "a directory"
        );
        assert_eq!(
            complete_word("ls s", cwd, home, none),
            None,
            "src and scripts: ambiguous"
        );
        assert_eq!(
            complete_word("ls sc", cwd, home, none).as_deref(),
            Some("ripts/")
        );
        assert_eq!(
            complete_word("vim src/ma", cwd, home, none).as_deref(),
            Some("in.rs")
        );
        assert_eq!(
            complete_word("cat ~/docs/no", cwd, home, none).as_deref(),
            Some("tes.txt")
        );
        let abs = format!("cat {}/project/RE", root.display());
        assert_eq!(
            complete_word(&abs, cwd, home, none).as_deref(),
            Some("ADME.md")
        );
        assert_eq!(
            complete_word("cat .hi", cwd, home, none).as_deref(),
            Some("dden")
        );
        assert_eq!(complete_word("cat ", cwd, home, none), None, "no word");
        assert_eq!(complete_word("cat 'REA", cwd, home, none), None, "quoted");
        assert_eq!(complete_word("cat REA", None, home, none), None, "no cwd");
        assert_eq!(complete_word("cat zzz", cwd, home, none), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn first_word_completes_from_path() {
        let exes: Vec<String> = ["cargo", "cargo-clippy", "cat", "htop"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            complete_word("ht", None, None, &exes).as_deref(),
            Some("op")
        );
        assert_eq!(
            complete_word("car", None, None, &exes).as_deref(),
            Some("go"),
            "common prefix"
        );
        assert_eq!(
            complete_word("ca", None, None, &exes),
            None,
            "cargo and cat"
        );
        assert_eq!(
            complete_word("sudo ht", None, None, &exes),
            None,
            "only the first word"
        );
        assert_eq!(
            complete_word("  ht", None, None, &exes).as_deref(),
            Some("op")
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_executables_lists_executable_files() {
        use std::os::unix::fs::PermissionsExt;
        let root = tree("exes");
        let bin = root.join("bin");
        for (name, mode) in [("tool-a", 0o755), ("data", 0o644)] {
            let path = bin.join(name);
            std::fs::write(&path, "").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let path_var = std::env::join_paths([bin.clone(), bin, root.join("missing")]).unwrap();
        assert_eq!(path_executables(&path_var), vec!["tool-a".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A directory listing cut off at `MAX_DIR_ENTRIES` may have missed
    /// matches, so it offers no completion; at exactly the cap the
    /// listing is whole and still completes.
    #[test]
    fn truncated_listing_offers_no_completion() {
        let root = tree("huge");
        let big = root.join("big");
        std::fs::create_dir_all(&big).unwrap();
        std::fs::write(big.join("unique-name"), "").unwrap();
        for i in 1..MAX_DIR_ENTRIES {
            std::fs::write(big.join(format!("f{i:05}")), "").unwrap();
        }
        let cwd = big.to_str();
        let none: &[String] = &[];
        assert_eq!(
            complete_word("cat uni", cwd, None, none).as_deref(),
            Some("que-name"),
            "exactly the cap: whole listing"
        );
        std::fs::write(big.join("f-one-more"), "").unwrap();
        assert_eq!(
            complete_word("cat uni", cwd, None, none),
            None,
            "past the cap: matches may be missing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

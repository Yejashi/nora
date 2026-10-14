//! N26a: deterministic corpus generator + headless engine benchmark.
//!
//! Corpus determinism pins exact byte equality across two generators
//! with the same seed; the engine benchmark trials verify measured
//! fields and build/machine metadata. Timings are informational in
//! tests (shared CI is noisy); the state checksum is the correctness
//! gate.

use std::time::Duration;

use nora::dev::bench::{BenchMetadata, BenchResult, run_bench};
use nora::dev::corpus::Corpus;

const ROWS: usize = 24;
const COLS: usize = 80;

fn bench_result(corpus: &[u8]) -> BenchResult {
    run_bench(corpus, ROWS, COLS)
}

fn assert_metadata(metadata: &BenchMetadata) {
    assert_eq!(metadata.package, "nora");
    assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
    assert!(
        matches!(metadata.profile, "debug" | "release"),
        "profile is {:?}",
        metadata.profile
    );
    assert!(!metadata.os.is_empty());
    assert!(!metadata.arch.is_empty());
    assert!(metadata.logical_cpus > 0);
}

#[test]
fn corpus_ascii_deterministic() {
    let a = Corpus::default().ascii_lines(100);
    let b = Corpus::default().ascii_lines(100);
    assert_eq!(a, b, "same seed must produce the same bytes");
    // 100 lines × (80 columns + \r\n).
    assert_eq!(a.len(), 100 * (80 + 2));
    // Printable ASCII only, line-terminated.
    assert!(
        a.iter()
            .all(|b| *b == b'\r' || *b == b'\n' || *b >= 0x20 && *b <= 0x7E)
    );
    assert_eq!(&a[80..82], b"\r\n");
    // A different seed draws different characters.
    let other = Corpus::new(Corpus::DEFAULT_SEED + 1).ascii_lines(100);
    assert_ne!(a, other);
}

#[test]
fn corpus_unicode_deterministic() {
    let a = Corpus::default().unicode_lines(50);
    let b = Corpus::default().unicode_lines(50);
    assert_eq!(a, b, "same seed must produce the same bytes");
    // Valid UTF-8: 50 lines, each exactly 80 characters (2–3 bytes each)
    // followed by \r\n.
    let text = std::str::from_utf8(&a).expect("corpus is valid UTF-8");
    let lines: Vec<&str> = text.split("\r\n").collect();
    assert_eq!(lines.len(), 51); // 50 lines + trailing empty
    assert!(lines[..50].iter().all(|l| l.chars().count() == 80));
    let other = Corpus::new(Corpus::DEFAULT_SEED + 1).unicode_lines(50);
    assert_ne!(a, other);
}

#[test]
fn corpus_control_flood_deterministic() {
    let a = Corpus::default().control_sequence_flood(1000);
    let b = Corpus::default().control_sequence_flood(1000);
    assert_eq!(a, b, "same seed must produce the same bytes");
    // No printable payload: the corpus is escape sequences only —
    // ESC, CSI introducer, parameters and final bytes.
    assert!(!a.is_empty());
    let allowed = b"\x1b[]0123456789;?hlJKHABCDm";
    assert!(a.iter().all(|b| allowed.contains(b)));
    let other = Corpus::new(Corpus::DEFAULT_SEED + 1).control_sequence_flood(1000);
    assert_ne!(a, other);
}

#[test]
fn corpus_unique_glyphs_deterministic() {
    let a = Corpus::default().unique_glyphs(500);
    let b = Corpus::default().unique_glyphs(500);
    assert_eq!(a, b, "same seed must produce the same bytes");
    // Exactly 500 lines, one glyph each (all under 0x10FFFF here).
    let text = String::from_utf8(a.clone()).unwrap();
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines.len(), 501); // 500 glyphs + trailing empty
    assert!(lines[..500].iter().all(|l| !l.is_empty()));
    // Glyphs are unique (one per line, ascending code points).
    let glyphs: Vec<char> = lines[..500]
        .iter()
        .map(|l| l.chars().next().unwrap())
        .collect();
    let mut seen = std::collections::HashSet::new();
    assert!(glyphs.iter().all(|c| seen.insert(*c)));
    let other = Corpus::new(Corpus::DEFAULT_SEED + 1).unique_glyphs(500);
    assert_eq!(
        a, other,
        "unique_glyphs is a pure function of n (seed-independent)"
    );
}

#[test]
fn engine_bench_ascii() {
    let corpus = Corpus::default().ascii_lines(1000);
    let result = bench_result(&corpus);
    assert_eq!(result.corpus_bytes, corpus.len());
    assert_eq!(result.rows, ROWS);
    assert_eq!(result.cols, COLS);
    assert!(result.feed_time > Duration::ZERO);
    assert!(result.snapshot_time > Duration::ZERO);
    assert!(result.total_time >= result.feed_time);
    assert!(result.total_time >= result.snapshot_time);
    // State correctness: the screen is not blank and the checksum is
    // stable across identical trials (deterministic corpus + engine).
    assert!(result.non_blank_cells > 0);
    let again = bench_result(&corpus);
    assert_eq!(result.state_checksum, again.state_checksum);
    assert_metadata(&result.metadata);
}

#[test]
fn engine_bench_unicode() {
    let corpus = Corpus::default().unicode_lines(500);
    let result = bench_result(&corpus);
    assert_eq!(result.corpus_bytes, corpus.len());
    assert!(result.feed_time > Duration::ZERO);
    assert!(result.snapshot_time > Duration::ZERO);
    assert!(result.non_blank_cells > 0);
    let again = bench_result(&corpus);
    assert_eq!(result.state_checksum, again.state_checksum);
    assert_metadata(&result.metadata);
}

#[test]
fn engine_bench_control_flood() {
    let corpus = Corpus::default().control_sequence_flood(5000);
    let result = bench_result(&corpus);
    assert_eq!(result.corpus_bytes, corpus.len());
    assert!(result.feed_time > Duration::ZERO);
    assert!(result.snapshot_time > Duration::ZERO);
    // The flood ends with cursor positioning/clears; the screen may be
    // blank, so no non-blank assertion here — the checksum pins state.
    let again = bench_result(&corpus);
    assert_eq!(result.state_checksum, again.state_checksum);
    assert_metadata(&result.metadata);
}

#[test]
fn engine_bench_unique_glyphs() {
    let corpus = Corpus::default().unique_glyphs(1000);
    let result = bench_result(&corpus);
    assert_eq!(result.corpus_bytes, corpus.len());
    assert!(result.feed_time > Duration::ZERO);
    assert!(result.snapshot_time > Duration::ZERO);
    assert!(result.non_blank_cells > 0);
    let again = bench_result(&corpus);
    assert_eq!(result.state_checksum, again.state_checksum);
    assert_metadata(&result.metadata);
}

#[test]
fn bench_result_metadata() {
    let corpus = Corpus::default().ascii_lines(10);
    let result = bench_result(&corpus);
    assert_metadata(&result.metadata);
    assert_eq!(result.metadata.package, "nora");
    assert!(result.metadata.logical_cpus > 0);
}

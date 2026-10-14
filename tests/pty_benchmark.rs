//! N26b: real PTY completion handshake benchmark.
//!
//! Each trial spawns the tagged-I/O helper bin (`src/bin/nora_child.rs`,
//! discovered via `CARGO_BIN_EXE_nora_child`) in `stop <n>` mode over a
//! real PTY: the child prints the `STOPPED` completion marker only after
//! it has received all `n` fed bytes and a newline, so the completion
//! time is a real handshake, not a timer on the writer. Timings are
//! informational in tests (shared CI is noisy); the `completed` flag and
//! the corpus byte count are the correctness gates.

use std::time::Duration;

use nora::dev::bench::BenchMetadata;
use nora::dev::corpus::Corpus;
use nora::dev::pty_bench::{PtyBench, PtyBenchResult, run_pty_bench};

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// A deterministic 100-line corpus (80 columns + `\r\n` per line).
fn corpus() -> Vec<u8> {
    Corpus::default().ascii_lines(100)
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

/// Common shape checks for every trial: the corpus round-trips in full
/// and the trial completed the real handshake within the deadline.
fn assert_trial(result: &PtyBenchResult, corpus: &[u8]) {
    assert_eq!(result.corpus_bytes, corpus.len());
    assert_eq!(result.rows, ROWS);
    assert_eq!(result.cols, COLS);
    assert!(
        result.completed,
        "the completion marker must appear within the deadline"
    );
    assert_eq!(result.marker, "STOPPED");
}

#[test]
fn pty_bench_spawn() {
    let corpus = corpus();
    let result = run_pty_bench(&corpus, ROWS, COLS);
    assert_trial(&result, &corpus);
    // A real spawn takes wall time and is contained in the total.
    assert!(result.spawn_time > Duration::ZERO);
    assert!(result.total_time >= result.spawn_time);
    assert_metadata(&result.metadata);
}

#[test]
fn pty_bench_feed() {
    let corpus = corpus();
    let result = run_pty_bench(&corpus, ROWS, COLS);
    assert_trial(&result, &corpus);
    // The feed (enqueue + flush) takes wall time and is contained in
    // the total.
    assert!(result.feed_time > Duration::ZERO);
    assert!(result.total_time >= result.feed_time);
    assert_metadata(&result.metadata);
}

#[test]
fn pty_bench_completion() {
    let corpus = corpus();
    let result = run_pty_bench(&corpus, ROWS, COLS);
    assert_trial(&result, &corpus);
    // The completion handshake (child receipt + marker in the engine
    // grid) takes wall time and is contained in the total.
    assert!(result.completion_time > Duration::ZERO);
    assert!(result.total_time >= result.completion_time);
    // The `PtyBench` runner and the one-shot `run_pty_bench` drive the
    // same handshake through the same session API.
    let via_runner = PtyBench::new(nora::dev::pty_bench::helper_program(), ROWS, COLS).run(&corpus);
    assert_trial(&via_runner, &corpus);
    assert_metadata(&via_runner.metadata);
}

#[test]
fn pty_bench_metadata() {
    let corpus = corpus();
    let result = run_pty_bench(&corpus, ROWS, COLS);
    assert_metadata(&result.metadata);
    assert_eq!(result.metadata.package, "nora");
    assert!(result.metadata.logical_cpus > 0);
}

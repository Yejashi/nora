//! N26b: headless renderer instrumentation.
//!
//! Each trial feeds a full-screen deterministic corpus to a fresh
//! headless engine, then times **prepare** (snapshot → draw items and
//! background fills — the same pure mapping the GPU path uses) and
//! **submit** (flatten the prepared frame into a flat byte buffer — the
//! CPU-side stand-in for the GPU vertex upload). No GPU is involved;
//! timings are informational in tests (shared CI is noisy), the item
//! counts and the N26a state checksum are the correctness gates.

use std::time::Duration;

use nora::dev::bench::BenchMetadata;
use nora::dev::render_bench::{RenderBench, RenderBenchResult, run_render_bench};

const ROWS: u16 = 24;
const COLS: u16 = 80;

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

/// Common shape checks for every trial: the viewport round-trips, the
/// screen is non-blank, and the state checksum is stable across
/// identical trials (deterministic corpus + engine + mapping).
fn assert_trial(result: &RenderBenchResult) {
    assert_eq!(result.rows, ROWS);
    assert_eq!(result.cols, COLS);
    assert!(result.corpus_bytes > 0);
    assert!(result.draw_items > 0);
    assert!(result.fill_rects > 0);
    let again = run_render_bench(ROWS, COLS);
    assert_eq!(result.state_checksum, again.state_checksum);
    assert_eq!(result.draw_items, again.draw_items);
}

#[test]
fn render_bench_prepare() {
    let result = run_render_bench(ROWS, COLS);
    assert_trial(&result);
    // Prepare (the snapshot → draw-item mapping) takes wall time and is
    // contained in the total.
    assert!(result.prepare_time > Duration::ZERO);
    assert!(result.total_time >= result.prepare_time);
    assert_metadata(&result.metadata);
}

#[test]
fn render_bench_submit() {
    let result = run_render_bench(ROWS, COLS);
    assert_trial(&result);
    // Submit (the CPU-side upload stand-in) takes wall time, uploads a
    // non-empty frame, and is contained in the total.
    assert!(result.submit_time > Duration::ZERO);
    assert!(result.submitted_bytes > 0);
    assert!(result.total_time >= result.submit_time);
    // The flattened frame is deterministic: a second trial through the
    // `RenderBench` runner uploads the same number of bytes.
    let via_runner = RenderBench::new(ROWS, COLS).run();
    assert_eq!(result.submitted_bytes, via_runner.submitted_bytes);
    assert_metadata(&result.metadata);
}

#[test]
fn render_bench_metadata() {
    let result = run_render_bench(ROWS, COLS);
    assert_metadata(&result.metadata);
    assert_eq!(result.metadata.package, "nora");
    assert!(result.metadata.logical_cpus > 0);
}

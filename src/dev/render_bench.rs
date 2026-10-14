//! Headless renderer instrumentation (N26b).
//!
//! Measures the CPU-side render pipeline on a fresh headless engine:
//! feed a deterministic corpus to fill the screen, take the final
//! snapshot, **prepare** the render pass (map the snapshot to draw items
//! and background fills — the same pure mapping the GPU path uses,
//! `crate::render::term`) and **submit** it (flatten the prepared frame
//! into a flat byte buffer — the CPU-side stand-in for the GPU vertex
//! upload). No GPU is involved; the final screen is checksummed (N26a
//! state checksum) so a fast trial is never a fast *wrong* trial.

use std::time::{Duration, Instant};

use crate::dev::bench::{BenchMetadata, state_checksum};
use crate::dev::corpus::Corpus;
use crate::engine::Engine;
use crate::render::term::{
    DrawItem, FillRect, TermLayout, build_bg_fills, build_draw_items, cursor_fill,
};

/// Feed chunk size for [`RenderBench::run`] (4 KiB, matching N26a).
const FEED_CHUNK: usize = 4096;

/// Baseline cell geometry for headless trials: the N02a baseline (20 px
/// font, 24 px row) with the nominal headless cell width. Geometry only
/// affects the items' bounds (pure terminal math), never their count.
const BASELINE_LAYOUT: TermLayout = TermLayout {
    cell_w: 8.0,
    cell_h: 24.0,
    font_size: 20.0,
};

/// Headless renderer benchmark runner (N26b).
///
/// Each [`run`](RenderBench::run) feeds a deterministic corpus to a
/// fresh engine at the runner's viewport geometry, then times prepare
/// and submit of one render pass over the final snapshot.
pub struct RenderBench {
    rows: u16,
    cols: u16,
}

impl RenderBench {
    /// Create a benchmark runner for a `rows` × `cols` viewport.
    pub fn new(rows: u16, cols: u16) -> Self {
        Self { rows, cols }
    }

    /// Feed a full-screen deterministic corpus to a fresh engine, then
    /// time prepare and submit of one render pass over the final
    /// snapshot, and return the measured result.
    pub fn run(&self) -> RenderBenchResult {
        let started = Instant::now();

        let mut engine = Engine::new(self.rows as usize, self.cols as usize);
        let corpus = Corpus::default().ascii_lines((self.rows * self.cols) as usize);
        for chunk in corpus.chunks(FEED_CHUNK) {
            engine.feed(chunk);
        }
        let snapshot = engine.snapshot();

        let prepare_started = Instant::now();
        let items = build_draw_items(&snapshot, &BASELINE_LAYOUT);
        let fills = build_bg_fills(&snapshot, &BASELINE_LAYOUT);
        let cursor = cursor_fill(&snapshot, &BASELINE_LAYOUT);
        let prepare_time = prepare_started.elapsed();

        let submit_started = Instant::now();
        let submitted_bytes = submit_frame(&items, &fills, cursor);
        let submit_time = submit_started.elapsed();

        RenderBenchResult {
            rows: self.rows,
            cols: self.cols,
            corpus_bytes: corpus.len(),
            draw_items: items.len(),
            fill_rects: fills.len() + usize::from(cursor.is_some()),
            prepare_time,
            submit_time,
            total_time: started.elapsed(),
            submitted_bytes,
            state_checksum: state_checksum(&snapshot),
            metadata: BenchMetadata::current(),
        }
    }
}

impl Default for RenderBench {
    fn default() -> Self {
        Self::new(24, 80)
    }
}

/// One-shot headless renderer benchmark at `rows` × `cols` (N26b).
pub fn run_render_bench(rows: u16, cols: u16) -> RenderBenchResult {
    RenderBench::new(rows, cols).run()
}

/// Flatten one prepared frame into a flat byte buffer: the CPU-side
/// stand-in for the GPU vertex upload (positions and colors as
/// little-endian integers, the cluster as UTF-8 with a terminator — the
/// same field order the graphics path uploads). Returns the number of
/// bytes uploaded.
fn submit_frame(items: &[DrawItem], fills: &[FillRect], cursor: Option<FillRect>) -> usize {
    let mut bytes = Vec::new();
    for item in items {
        bytes.extend_from_slice(&(item.row as u32).to_le_bytes());
        bytes.extend_from_slice(&(item.col as u32).to_le_bytes());
        bytes.extend_from_slice(item.cluster.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&[item.fg.r, item.fg.g, item.fg.b]);
        bytes.extend_from_slice(&[item.bg.r, item.bg.g, item.bg.b]);
        bytes.push(u8::from(item.is_cursor));
    }
    for fill in fills {
        bytes.extend_from_slice(&(fill.row as u32).to_le_bytes());
        bytes.extend_from_slice(&(fill.col as u32).to_le_bytes());
        bytes.extend_from_slice(&fill.span.to_le_bytes());
        bytes.extend_from_slice(&[fill.color.r, fill.color.g, fill.color.b]);
    }
    if let Some(fill) = cursor {
        bytes.extend_from_slice(&(fill.row as u32).to_le_bytes());
        bytes.extend_from_slice(&(fill.col as u32).to_le_bytes());
        bytes.extend_from_slice(&fill.span.to_le_bytes());
        bytes.extend_from_slice(&[fill.color.r, fill.color.g, fill.color.b]);
    }
    bytes.len()
}

/// One measured headless renderer trial (N26b).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderBenchResult {
    /// Viewport height for the trial.
    pub rows: u16,
    /// Viewport width for the trial.
    pub cols: u16,
    /// Bytes of the fed corpus.
    pub corpus_bytes: usize,
    /// Draw items the prepared pass mapped from the snapshot.
    pub draw_items: usize,
    /// Background fills plus the cursor fill (when visible).
    pub fill_rects: usize,
    /// Time to prepare the render pass (snapshot → draw items/fills).
    pub prepare_time: Duration,
    /// Time to submit the prepared frame (CPU-side upload stand-in).
    pub submit_time: Duration,
    /// Wall time over the whole trial (engine setup, feed, prepare, submit).
    pub total_time: Duration,
    /// Bytes the submit flattened (the CPU-side upload size).
    pub submitted_bytes: usize,
    /// FNV-1a 64 over the final screen state (state correctness).
    pub state_checksum: u64,
    /// Build and machine metadata for the trial.
    pub metadata: BenchMetadata,
}

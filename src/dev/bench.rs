//! Headless engine benchmark (N26a).
//!
//! Feeds a deterministic corpus to a fresh [`Engine`] and times engine
//! processing (feed) and state extraction (snapshot) separately, so
//! processing cost is reported apart from presentation. The final screen
//! is checksummed so a fast trial is never a fast *wrong* trial.

use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::Color;

use crate::engine::{Engine, Snapshot};

/// Feed chunk size for [`EngineBench::run`] (4 KiB — one of the
/// documented feed sizes in `docs/04-performance.md`).
const FEED_CHUNK: usize = 4096;

/// FNV-1a 64 parameters (state checksum).
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Headless engine benchmark runner (N26a).
///
/// Each [`run`](EngineBench::run) feeds a corpus to a fresh engine at
/// the runner's viewport geometry and times feed vs snapshot.
#[derive(Clone, Copy, Debug)]
pub struct EngineBench {
    rows: usize,
    cols: usize,
}

impl EngineBench {
    /// Create a benchmark runner for a `rows` × `cols` viewport.
    pub fn new(rows: usize, cols: usize) -> Self {
        Self { rows, cols }
    }

    /// Feed `corpus` to a fresh engine in 4 KiB chunks, take one final
    /// snapshot, and return the measured result.
    pub fn run(&self, corpus: &[u8]) -> BenchResult {
        let started = Instant::now();
        let mut engine = Engine::new(self.rows, self.cols);

        let feed_started = Instant::now();
        for chunk in corpus.chunks(FEED_CHUNK) {
            engine.feed(chunk);
        }
        let feed_time = feed_started.elapsed();

        let snapshot_started = Instant::now();
        let snapshot = engine.snapshot();
        let snapshot_time = snapshot_started.elapsed();

        let non_blank_cells = snapshot.cells.iter().filter(|cell| cell.ch != ' ').count();

        BenchResult {
            corpus_bytes: corpus.len(),
            rows: self.rows,
            cols: self.cols,
            feed_time,
            snapshot_time,
            total_time: started.elapsed(),
            state_checksum: state_checksum(&snapshot),
            non_blank_cells,
            metadata: metadata(),
        }
    }
}

impl Default for EngineBench {
    fn default() -> Self {
        Self::new(24, 80)
    }
}

/// One-shot headless benchmark at `rows` × `cols` (N26a).
pub fn run_bench(corpus: &[u8], rows: usize, cols: usize) -> BenchResult {
    EngineBench::new(rows, cols).run(corpus)
}

/// One measured headless-engine trial (N26a).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchResult {
    /// Bytes of the fed corpus.
    pub corpus_bytes: usize,
    /// Viewport height for the trial.
    pub rows: usize,
    /// Viewport width for the trial.
    pub cols: usize,
    /// Time to feed the whole corpus (engine processing).
    pub feed_time: Duration,
    /// Time to take the final snapshot (state extraction).
    pub snapshot_time: Duration,
    /// Wall time over the whole trial (engine setup, feed + snapshot).
    pub total_time: Duration,
    /// FNV-1a 64 over the final screen state (state correctness).
    pub state_checksum: u64,
    /// Non-space cells in the final snapshot.
    pub non_blank_cells: usize,
    /// Build and machine metadata for the trial.
    pub metadata: BenchMetadata,
}

/// Build and machine metadata attached to every [`BenchResult`] (N26a).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchMetadata {
    /// nora package name (compile-time).
    pub package: &'static str,
    /// nora package version (compile-time).
    pub version: &'static str,
    /// Build profile: `"debug"` or `"release"`.
    pub profile: &'static str,
    /// Target OS (compile-time).
    pub os: &'static str,
    /// Target architecture (compile-time).
    pub arch: &'static str,
    /// Logical CPUs visible to the process (0 when unknown).
    pub logical_cpus: usize,
}

/// Build/machine metadata for the current process.
fn metadata() -> BenchMetadata {
    let logical_cpus = std::thread::available_parallelism()
        .map(|cpus| cpus.get())
        .unwrap_or(0);
    BenchMetadata {
        package: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        logical_cpus,
    }
}

/// FNV-1a 64 over `bytes`, continuing from `hash`.
fn fnv1a_64(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Stable numeric key for a cell color (part of the state checksum).
fn color_key(color: &Color) -> u32 {
    match color {
        Color::Named(n) => 1u32 | u32::from(*n as u8),
        Color::Spec(rgb) => {
            2u32 | (u32::from(rgb.r) << 8) | (u32::from(rgb.g) << 16) | (u32::from(rgb.b) << 24)
        }
        Color::Indexed(i) => 3u32 | (u32::from(*i) << 2),
    }
}

/// FNV-1a 64 over the final screen state: every cell's character, flags,
/// colors and zerowidth stack, plus the viewport size and cursor
/// position (N26a state correctness).
fn state_checksum(snapshot: &Snapshot) -> u64 {
    let mut hash = FNV_OFFSET;
    let mut utf8 = [0u8; 4];
    for cell in &snapshot.cells {
        hash = fnv1a_64(hash, cell.ch.encode_utf8(&mut utf8).as_bytes());
        hash = fnv1a_64(hash, &cell.flags.bits().to_le_bytes());
        hash = fnv1a_64(hash, &color_key(&cell.fg).to_le_bytes());
        hash = fnv1a_64(hash, &color_key(&cell.bg).to_le_bytes());
        for zw in &cell.zerowidth {
            hash = fnv1a_64(hash, zw.encode_utf8(&mut utf8).as_bytes());
        }
    }
    hash = fnv1a_64(hash, &snapshot.rows.to_le_bytes());
    hash = fnv1a_64(hash, &snapshot.cols.to_le_bytes());
    if let Some((row, col)) = snapshot.cursor {
        hash = fnv1a_64(hash, &row.to_le_bytes());
        hash = fnv1a_64(hash, &col.to_le_bytes());
    }
    hash
}

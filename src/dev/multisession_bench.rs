//! Multi-pane latency benchmark (N28).
//!
//! Measures input responsiveness under multi-session output load. One
//! probe pane (a real PTY child that echoes) is surrounded by `pane_count`
//! flood panes that each write a sustained output corpus. For each round
//! the probe is fed a tagged echo line and the round latency (feed to
//! echoed line visible in the probe's grid) is recorded. Each probe poll
//! also samples the shared parse-lock wait and the snapshot (surface
//! preparation) cost, so processing and presentation are distinguished the
//! way the N26 instruments do.
//!
//! The probe child is `nora_child` (echo mode) and each flood child is
//! `nora_child flood <total>`, both discovered via
//! `CARGO_BIN_EXE_nora_child` (cargo sets it for integration tests). The
//! benchmark is therefore a development instrument: it needs the helper
//! bin and a real PTY, and no production code path depends on it.

use std::time::{Duration, Instant};

use crate::dev::bench::BenchMetadata;
use crate::engine::Snapshot;
use crate::session::io::WriterConfig;
use crate::session::pty::{LaunchSpec, PtySession};

/// One poll tick inside the deadline-bounded round waits.
const POLL: Duration = Duration::from_millis(10);
/// Hard deadline for every round wait and ready wait in this instrument.
const ROUND_DEADLINE: Duration = Duration::from_secs(10);
/// Default number of echo rounds per run.
const DEFAULT_ROUNDS: usize = 50;
/// Default probe/flood pane geometry.
const ROWS: usize = 24;
const COLS: usize = 80;
/// Writer budget per session (same as the N26 PTY instruments).
const CAP: WriterConfig = WriterConfig {
    max_bytes: 1024 * 1024,
    reserved_bytes: 64 * 1024,
};
/// Minimum output load per flood pane: the flood total is at least this many
/// bytes (or the corpus length, if larger), so the output load is sustained
/// across the whole run even for a small corpus.
const MIN_FLOOD_TOTAL: usize = 1024 * 1024;

/// Multi-pane latency benchmark runner (N28).
///
/// Each [`run`](MultiSessionBench::run) spawns `pane_count` real PTY
/// sessions (one probe + `pane_count - 1` flood panes), runs `round_count`
/// tagged echo rounds against the probe while the flood panes write, and
/// records the per-round latencies plus the sampled parse-lock wait and
/// snapshot cost.
#[derive(Clone, Debug)]
pub struct MultiSessionBench {
    pane_count: usize,
    round_count: usize,
    helper: String,
}

impl MultiSessionBench {
    /// Create a benchmark runner for `pane_count` panes (probe + floods)
    /// at the default geometry, with `round_count` echo rounds.
    pub fn new(pane_count: usize, round_count: usize) -> Self {
        let helper = std::env::var("CARGO_BIN_EXE_nora_child").unwrap_or_else(|_| {
            eprintln!(
                "nora: CARGO_BIN_EXE_nora_child is not set; the multi-session \
                 benchmark needs the helper bin (run under cargo test)"
            );
            String::new()
        });
        Self {
            pane_count: pane_count.max(1),
            round_count,
            helper,
        }
    }

    /// Run the multi-pane benchmark.
    ///
    /// Spawns the probe and the flood panes, waits for all to be ready,
    /// then runs `round_count` tagged echo rounds against the probe while
    /// the flood panes write output. Each round's latency (feed to echoed
    /// line visible) is recorded, and each probe poll samples the shared
    /// parse-lock wait and the snapshot cost.
    pub fn run(&self, corpus: &[u8]) -> MultiSessionBenchResult {
        let started = Instant::now();
        let mut input_service_time = Duration::ZERO;
        let mut parse_lock_wait = Duration::ZERO;
        let mut surface_wait = Duration::ZERO;
        let mut round_latencies: Vec<Duration> = Vec::new();
        let mut completed_rounds = 0usize;

        // The output load per flood pane: at least `MIN_FLOOD_TOTAL` bytes
        // (or the corpus length, if larger), so the load is sustained.
        let flood_total = corpus.len().max(MIN_FLOOD_TOTAL);

        // Spawn the probe (session 0) and the flood panes.
        let probe_spec = LaunchSpec {
            program: self.helper.clone(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
        };
        let mut probe = PtySession::spawn(probe_spec, ROWS, COLS, CAP)
            .expect("probe session spawns over a real PTY");
        let mut floods: Vec<PtySession> = Vec::with_capacity(self.pane_count - 1);
        for _ in 1..self.pane_count {
            let flood_spec = LaunchSpec {
                program: self.helper.clone(),
                args: vec!["flood".to_string(), flood_total.to_string()],
                cwd: None,
                env: Vec::new(),
            };
            let flood = PtySession::spawn(flood_spec, ROWS, COLS, CAP)
                .expect("flood session spawns over a real PTY");
            floods.push(flood);
        }

        // Wait for the probe to be ready: it blocks on stdin after READY,
        // so READY stays on screen and the wait is deterministic. The flood
        // panes start writing immediately after spawn, so by the time the
        // probe is ready they are already producing load — their READY
        // lines scroll off under the flood and are not waited on.
        wait_ready(&mut probe);

        // The shared parse lock: the probe's term handle. Each probe poll
        // samples the time to acquire it (the parse-lock wait) and the time
        // to take a snapshot (the surface preparation cost).
        let term_handle = probe.engine().term_handle();

        for round in 0..self.round_count {
            let tag = format!("PING:{round}");
            let line = format!("{tag}\n");
            let round_start = Instant::now();

            // Input service: feed + flush the tagged line to the probe.
            let input_start = Instant::now();
            let fed = probe.feed(line.as_bytes()).expect("probe feed");
            let flushed = probe.flush(CAP.max_bytes).expect("probe flush");
            input_service_time += input_start.elapsed();
            debug_assert_eq!(fed, line.len());
            debug_assert_eq!(flushed, line.len());

            // Poll until the echoed line is visible in the probe's grid.
            let mut observed = false;
            let deadline = round_start + ROUND_DEADLINE;
            while !observed {
                // Sample the shared parse-lock wait (acquire + release).
                let lock_start = Instant::now();
                {
                    let _guard = term_handle.lock();
                }
                parse_lock_wait += Instant::now() - lock_start;

                // Sample the snapshot (surface preparation) cost.
                let snap_start = Instant::now();
                let snap = probe.engine().snapshot();
                surface_wait += Instant::now() - snap_start;

                let text = grid_text(&snap);
                if text.contains(&format!("E:{tag}")) {
                    observed = true;
                } else {
                    probe.poll_events();
                    if Instant::now() > deadline {
                        break;
                    }
                    std::thread::sleep(POLL);
                }
            }

            if observed {
                round_latencies.push(round_start.elapsed());
                completed_rounds += 1;
            }
        }

        // Close every pane (probe + floods) — shutdown under load.
        probe.close();
        for flood in floods.iter_mut() {
            flood.close();
        }

        let total_time = started.elapsed();
        round_latencies.sort();

        MultiSessionBenchResult {
            pane_count: self.pane_count,
            round_count: self.round_count,
            completed_rounds,
            corpus_bytes: corpus.len(),
            rows: ROWS as u16,
            cols: COLS as u16,
            p50: percentile(&round_latencies, 0.50),
            p95: percentile(&round_latencies, 0.95),
            p99: percentile(&round_latencies, 0.99),
            input_service_time,
            parse_lock_wait,
            surface_wait,
            total_time,
            metadata: BenchMetadata::current(),
        }
    }
}

impl Default for MultiSessionBench {
    fn default() -> Self {
        Self::new(4, DEFAULT_ROUNDS)
    }
}

/// One-shot multi-pane benchmark with the default round count (N28).
pub fn run_multi_session_bench(pane_count: usize, corpus: &[u8]) -> MultiSessionBenchResult {
    MultiSessionBench::new(pane_count, DEFAULT_ROUNDS).run(corpus)
}

/// One measured multi-pane benchmark trial (N28).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiSessionBenchResult {
    /// Number of panes (probe + flood panes).
    pub pane_count: usize,
    /// Number of echo rounds attempted.
    pub round_count: usize,
    /// Number of rounds whose echoed line was observed (completed).
    pub completed_rounds: usize,
    /// Bytes of the output corpus (per flood pane, before the minimum).
    pub corpus_bytes: usize,
    /// Probe/flood pane height.
    pub rows: u16,
    /// Probe/flood pane width.
    pub cols: u16,
    /// 50th percentile round latency (feed to echoed line visible).
    pub p50: Duration,
    /// 95th percentile round latency.
    pub p95: Duration,
    /// 99th percentile round latency.
    pub p99: Duration,
    /// Total time spent in `feed` + `flush` across all rounds (input
    /// service on the main thread).
    pub input_service_time: Duration,
    /// Total time spent acquiring the shared parse lock across all probe
    /// polls (the parse-lock wait).
    pub parse_lock_wait: Duration,
    /// Total time spent taking probe snapshots across all probe polls
    /// (the surface preparation cost).
    pub surface_wait: Duration,
    /// Wall time over the whole trial (spawn, ready waits, rounds, close).
    pub total_time: Duration,
    /// Build and machine metadata for the trial.
    pub metadata: BenchMetadata,
}

/// Deadline-bounded wait for the child to print `READY` in the session's
/// grid (the helper bin prints it before doing its work).
fn wait_ready(session: &mut PtySession) {
    let deadline = Instant::now() + ROUND_DEADLINE;
    loop {
        let snap = session.engine().snapshot();
        let text = grid_text(&snap);
        session.poll_events();
        if text.contains("READY") {
            return;
        }
        if Instant::now() > deadline {
            panic!("pane did not print READY within the deadline; last grid: {text:?}");
        }
        std::thread::sleep(POLL);
    }
}

/// The engine's current grid as one row-major string, so any visible line is
/// a contiguous substring (mirrors the N26 PTY instrument and the
/// `pty_session` test suite).
fn grid_text(snap: &Snapshot) -> String {
    snap.cells.iter().map(|c| c.ch).collect()
}

/// Nearest-rank percentile over a sorted list of latencies.
fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let n = sorted.len();
    let rank = (p * n as f64).ceil() as usize;
    let idx = rank.clamp(1, n) - 1;
    sorted[idx]
}

//! Real PTY completion handshake benchmark (N26b).
//!
//! Spawns the tagged-I/O helper child (`src/bin/nora_child.rs`) over a
//! real PTY ([`PtySession`], N05a) in `stop <n>` mode: the child reads
//! its stdin until at least `n` bytes have been received and a newline
//! has been seen, and only then prints the `STOPPED` completion marker.
//! The marker can therefore appear only after the child has processed
//! every byte nora sent — a real PTY completion handshake, not a timer
//! on the generator's writes. The benchmark measures spawn (session
//! start), feed (enqueue + flush to the child), completion (time until
//! the marker is visible in the engine grid) and total wall time.
//!
//! Every wait is deadline-bounded (10 s, matching the PTY test suites);
//! a missed deadline is reported in [`PtyBenchResult::completed`]
//! rather than hanging.

use std::time::{Duration, Instant};

use crate::dev::bench::BenchMetadata;
use crate::session::pty::{DEFAULT_CAP, LaunchSpec, PtySession};

/// One poll tick inside the completion pump.
const POLL: Duration = Duration::from_millis(10);
/// Hard deadline for the completion handshake.
const DEADLINE: Duration = Duration::from_secs(10);
/// The completion marker the helper prints on full receipt (`stop` mode).
pub const COMPLETION_MARKER: &str = "STOPPED";

/// Real PTY benchmark runner (N26b).
///
/// Each [`run`](PtyBench::run) spawns the child in `stop <n>` mode over
/// a real PTY, feeds the corpus, and pumps the session until the
/// completion marker is visible in the engine grid (or the deadline is
/// reached).
pub struct PtyBench {
    program: String,
    rows: u16,
    cols: u16,
}

impl PtyBench {
    /// Create a runner that spawns `program` in `stop <n>` mode on a
    /// `rows` × `cols` PTY.
    pub fn new(program: String, rows: u16, cols: u16) -> Self {
        Self {
            program,
            rows,
            cols,
        }
    }

    /// Spawn the session, feed `corpus`, and wait for the completion
    /// handshake. The session is closed on every path.
    pub fn run(&self, corpus: &[u8]) -> PtyBenchResult {
        let started = Instant::now();

        let spawn_started = Instant::now();
        let mut session = PtySession::spawn(
            LaunchSpec {
                program: self.program.clone(),
                args: vec!["stop".to_string(), corpus.len().to_string()],
                cwd: None,
                env: Vec::new(),
            },
            self.rows as usize,
            self.cols as usize,
            DEFAULT_CAP,
        )
        .unwrap_or_else(|error| panic!("pty benchmark spawn failed: {error:?}"));
        let spawn_time = spawn_started.elapsed();

        let feed_started = Instant::now();
        session
            .feed(corpus)
            .unwrap_or_else(|error| panic!("pty benchmark feed failed: {error:?}"));
        session
            .flush(corpus.len())
            .unwrap_or_else(|error| panic!("pty benchmark flush failed: {error:?}"));
        let feed_time = feed_started.elapsed();

        let completion_started = Instant::now();
        let mut completed = false;
        let deadline = Instant::now() + DEADLINE;
        loop {
            let _ = session.poll_events();
            let text = grid_text(&mut session);
            if text.contains(COMPLETION_MARKER) {
                completed = true;
                break;
            }
            if Instant::now() > deadline {
                break;
            }
            std::thread::sleep(POLL);
        }
        let completion_time = completion_started.elapsed();

        session.close();

        PtyBenchResult {
            corpus_bytes: corpus.len(),
            rows: self.rows,
            cols: self.cols,
            spawn_time,
            feed_time,
            completion_time,
            total_time: started.elapsed(),
            completed,
            marker: COMPLETION_MARKER,
            metadata: BenchMetadata::current(),
        }
    }
}

/// The engine's current grid as one row-major string, so any visible line
/// is a contiguous substring (same idiom as the PTY test suites).
fn grid_text(session: &mut PtySession) -> String {
    let snap = session.engine().snapshot();
    snap.cells.iter().map(|c| c.ch).collect()
}

/// One-shot real PTY benchmark at `rows` × `cols` (N26b).
///
/// The child is the tagged-I/O helper binary (`src/bin/nora_child.rs`),
/// discovered via the `CARGO_BIN_EXE_nora_child` environment variable
/// that cargo sets for integration tests.
pub fn run_pty_bench(corpus: &[u8], rows: u16, cols: u16) -> PtyBenchResult {
    PtyBench::new(helper_program(), rows, cols).run(corpus)
}

/// The tagged-I/O helper binary path, auto-discovered by cargo for
/// integration tests.
pub fn helper_program() -> String {
    std::env::var("CARGO_BIN_EXE_nora_child")
        .expect("CARGO_BIN_EXE_nora_child must be set by cargo for integration tests")
}

/// One measured real-PTY trial (N26b).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtyBenchResult {
    /// Bytes fed to the child.
    pub corpus_bytes: usize,
    /// Viewport height for the trial.
    pub rows: u16,
    /// Viewport width for the trial.
    pub cols: u16,
    /// Time to spawn the PTY session (child launch + event loop start).
    pub spawn_time: Duration,
    /// Time to feed the whole corpus to the child (enqueue + flush).
    pub feed_time: Duration,
    /// Time until the completion marker was visible in the engine grid
    /// (the real handshake, not the writer's own accounting).
    pub completion_time: Duration,
    /// Wall time over the whole trial (spawn + feed + completion + close).
    pub total_time: Duration,
    /// Whether the completion marker arrived within the deadline.
    pub completed: bool,
    /// The completion marker the child prints on full receipt.
    pub marker: &'static str,
    /// Build and machine metadata for the trial.
    pub metadata: BenchMetadata,
}

//! N28 multi-pane latency benchmark tests.
//!
//! Each test launches the tagged-I/O helper bin (`src/bin/nora_child.rs`,
//! discovered via `CARGO_BIN_EXE_nora_child`) through real PTYs and asserts
//! the card's acceptance lines: input stays responsive under multi-session
//! output load, no bytes are dropped under load, shutdown remains
//! serviceable, idle sessions remain sleeping, and the p50/p95/p99 latencies
//! are measured.
//!
//! Every wait is deadline-bounded (10 s) and polls every 10 ms on observable
//! markers only — no sleep guesses.

use std::thread;
use std::time::{Duration, Instant};

use nora::dev::corpus::Corpus;
use nora::dev::multisession_bench::{MultiSessionBench, run_multi_session_bench};
use nora::render::term::is_noop_damage;
use nora::session::io::WriterConfig;
use nora::session::pty::{LaunchSpec, Lifecycle, PtySession};

/// One poll tick inside the deadline-bounded pumps.
const POLL: Duration = Duration::from_millis(10);
/// Hard deadline for every wait in this suite.
const DEADLINE: Duration = Duration::from_secs(10);
/// Pane geometry (matches the N26 PTY instruments).
const ROWS: usize = 24;
const COLS: usize = 80;
/// Writer budget per session.
const CAP: WriterConfig = WriterConfig {
    max_bytes: 1024 * 1024,
    reserved_bytes: 64 * 1024,
};

/// The helper bin path, auto-discovered by cargo for integration tests.
fn helper() -> String {
    std::env::var("CARGO_BIN_EXE_nora_child")
        .expect("CARGO_BIN_EXE_nora_child must be set by cargo for integration tests")
}

/// A deterministic output corpus (~82 KiB) used as the output load.
fn corpus() -> Vec<u8> {
    Corpus::new(Corpus::DEFAULT_SEED).ascii_lines(1000)
}

/// The engine's current grid as one row-major string, so any visible line is
/// a contiguous substring (mirrors `tests/pty_session.rs`).
fn grid_text(session: &mut PtySession) -> String {
    let snap = session.engine().snapshot();
    snap.cells.iter().map(|c| c.ch).collect()
}

/// Spawn a session with `args` over a real PTY.
fn spawn(args: Vec<String>) -> PtySession {
    PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args,
            cwd: None,
            env: Vec::new(),
        },
        ROWS,
        COLS,
        CAP,
    )
    .expect("session spawns over a real PTY")
}

/// Deadline-bounded wait for `marker` to appear in the session's grid.
fn wait_for(session: &mut PtySession, marker: &str) -> Result<(), String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let last = grid_text(session);
        session.poll_events();
        if last.contains(marker) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("deadline exceeded; last grid: {last:?}"));
        }
        thread::sleep(POLL);
    }
}

/// `multi_session_input_responsive`: input remains responsive under load.
///
/// The benchmark spawns a probe pane plus flood panes and runs tagged echo
/// rounds against the probe while the flood panes write. Every round must
/// complete (the echoed line becomes visible), and the tail latency must
/// stay well below the deadline — input is responsive under output load.
#[test]
fn multi_session_input_responsive() {
    let result = run_multi_session_bench(4, &corpus());

    assert_eq!(
        result.completed_rounds, result.round_count,
        "every echo round must complete under load; got {}/{}",
        result.completed_rounds, result.round_count
    );
    assert!(
        result.p50 <= result.p95 && result.p95 <= result.p99,
        "percentiles must be ordered: p50 {:?} <= p95 {:?} <= p99 {:?}",
        result.p50,
        result.p95,
        result.p99
    );
    assert!(
        result.p99 < DEADLINE,
        "p99 round latency {:?} must stay below the deadline",
        result.p99
    );
    assert!(
        result.total_time > Duration::ZERO,
        "total time must be measured"
    );
}

/// `multi_session_no_dropped_bytes`: no bytes are dropped under load.
///
/// A probe pane in `stop <n>` mode is fed exactly `n` bytes of the corpus
/// while flood panes write. The child prints `STOPPED` only after it has
/// received all `n` bytes (and a newline), so a short read fails the
/// deadline.
#[test]
fn multi_session_no_dropped_bytes() {
    let corpus = corpus();
    let n = corpus.len();

    let mut probe = spawn(vec!["stop".to_string(), n.to_string()]);
    let mut floods: Vec<PtySession> = (0..3)
        .map(|_| spawn(vec!["flood".to_string(), (1024 * 1024).to_string()]))
        .collect();

    // Feed the whole corpus to the probe in chunks, flushing each, while
    // the flood panes write. The `stop` child reads stdin until it has
    // received all `n` bytes (and a newline), then prints STOPPED; the PTY
    // buffers the feed, so it is safe to start feeding right after spawn.
    for chunk in corpus.chunks(4096) {
        let fed = probe.feed(chunk).expect("probe feed");
        assert_eq!(fed, chunk.len(), "probe accepted the whole chunk");
        probe.flush(CAP.max_bytes).expect("probe flush");
    }

    wait_for(&mut probe, "STOPPED")
        .expect("probe received every byte and printed STOPPED within the deadline");

    probe.close();
    for flood in floods.iter_mut() {
        flood.close();
    }
}

/// `multi_session_shutdown_serviceable`: shutdown remains serviceable.
///
/// Closing the probe (and the flood panes) while the floods are still
/// writing must return within the deadline and fold the lifecycle to
/// `Closed` — shutdown is serviceable under load.
#[test]
fn multi_session_shutdown_serviceable() {
    let mut probe = spawn(Vec::new());
    let mut floods: Vec<PtySession> = (0..3)
        .map(|_| spawn(vec!["flood".to_string(), (1024 * 1024).to_string()]))
        .collect();
    wait_for(&mut probe, "READY").expect("probe printed READY within the deadline");

    let started = Instant::now();
    probe.close();
    let close_time = started.elapsed();

    assert_eq!(
        probe.lifecycle(),
        Lifecycle::Closed,
        "probe must fold to Closed after close()"
    );
    assert!(
        close_time < DEADLINE,
        "close() under load took {close_time:?}, must stay below the deadline"
    );

    for flood in floods.iter_mut() {
        flood.close();
        assert_eq!(flood.lifecycle(), Lifecycle::Closed);
    }
}

/// `multi_session_idle_sleeping`: idle sessions remain sleeping.
///
/// After the probe prints `READY` it blocks on stdin and the screen is
/// stable: the probe's damage is the pinned no-op shape (it is sleeping —
/// producing no work) even while the flood panes write, and the probe is
/// still serviceable (a tagged echo round completes).
#[test]
fn multi_session_idle_sleeping() {
    let mut probe = spawn(Vec::new());
    let mut floods: Vec<PtySession> = (0..3)
        .map(|_| spawn(vec!["flood".to_string(), (1024 * 1024).to_string()]))
        .collect();
    wait_for(&mut probe, "READY").expect("probe printed READY within the deadline");

    // Settle until the probe reports the pinned no-op shape: the screen is
    // stable (the child blocks on stdin after READY), so the probe is
    // sleeping — producing no work while the flood panes write.
    let deadline = Instant::now() + DEADLINE;
    let (damage, cursor) = loop {
        let (d, c) = probe.engine().probe();
        if is_noop_damage(&d, c) || Instant::now() > deadline {
            break (d, c);
        }
        probe.poll_events();
        thread::sleep(POLL);
    };
    assert!(
        is_noop_damage(&damage, cursor),
        "an idle probe must report the pinned no-op shape, got {damage:?} cursor {cursor:?}"
    );

    // Still serviceable: a tagged echo round completes while the probe was
    // idle and the flood panes write.
    let tag = "PING:idle";
    let line = format!("{tag}\n");
    probe.feed(line.as_bytes()).expect("probe feed");
    probe.flush(CAP.max_bytes).expect("probe flush");
    wait_for(&mut probe, &format!("E:{tag}"))
        .expect("idle probe still services input within the deadline");

    probe.close();
    for flood in floods.iter_mut() {
        flood.close();
    }
}

/// `multi_session_p50_p95_p99`: latency percentiles are measured.
///
/// The benchmark records one latency per completed round and reports the
/// p50/p95/p99 over them. All three must be measured (finite, ordered) and
/// at least one round must have completed.
#[test]
fn multi_session_p50_p95_p99() {
    let bench = MultiSessionBench::new(4, 20);
    let result = bench.run(&corpus());

    assert!(
        result.completed_rounds > 0,
        "at least one round must complete to measure percentiles"
    );
    assert!(
        result.p50 <= result.p95 && result.p95 <= result.p99,
        "percentiles must be ordered: p50 {:?} <= p95 {:?} <= p99 {:?}",
        result.p50,
        result.p95,
        result.p99
    );
    // The sampled timings must be measured (non-negative by construction)
    // and the input-service time must account for at least the feed/flush
    // of the completed rounds.
    assert!(
        result.input_service_time > Duration::ZERO,
        "input service time must be measured"
    );
    assert!(
        result.parse_lock_wait > Duration::ZERO || result.surface_wait > Duration::ZERO,
        "the parse-lock wait or the snapshot cost must be measured"
    );
}

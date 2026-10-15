//! Opt-in startup tracing (`NORA_TRACE_STARTUP=1`): milliseconds since
//! process start at each startup stage, on stderr. Used by
//! `scripts/perf.sh` and for diagnosing slow starts; off by default.

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Option<Instant>> = OnceLock::new();

/// Record the process start; call first thing in `main`.
pub fn start() {
    START.get_or_init(|| std::env::var_os("NORA_TRACE_STARTUP").map(|_| Instant::now()));
}

/// Log `stage` with the elapsed time since [`start`], when tracing is on.
pub fn stage(stage: &str) {
    if let Some(Some(start)) = START.get() {
        eprintln!(
            "startup: {:8.1} ms {stage}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

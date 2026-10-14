//! Headless input instrumentation (N26b).
//!
//! Times the pure keyboard encoder (`crate::input::encode`) over a list
//! of key events: no terminal state, no PTY, no clocks beyond the trial
//! measurement itself. The encoder is pure, so an identical event list
//! always produces an identical byte count — the sent-byte total is the
//! correctness gate, the timings are informational on shared CI.

use std::time::{Duration, Instant};

use alacritty_terminal::term::TermMode;
use winit::keyboard::{Key, ModifiersState, PhysicalKey};

use crate::dev::bench::BenchMetadata;
use crate::input::{BindingTable, InputAction, encode};

/// One key event to encode (N26b): the plain winit fields the window
/// layer passes to [`encode`] (no `KeyEvent`).
#[derive(Clone, Debug)]
pub struct InputEvent {
    /// The event's layout text / named key.
    pub logical: Key,
    /// The layout-independent physical key.
    pub physical: PhysicalKey,
    /// The modifier state at the time of the event.
    pub mods: ModifiersState,
    /// Whether this is a key-repeat event.
    pub repeat: bool,
}

/// Headless input benchmark runner (N26b).
pub struct InputBench {
    bindings: BindingTable,
    modes: TermMode,
}

impl InputBench {
    /// Create a runner with the default binding table and no terminal
    /// modes set (the N09 rows are mode-independent).
    pub fn new() -> Self {
        Self {
            bindings: BindingTable::default(),
            modes: TermMode::empty(),
        }
    }

    /// Encode every event in order and return the measured result.
    pub fn run(&self, events: &[InputEvent]) -> InputBenchResult {
        let started = Instant::now();

        let encode_started = Instant::now();
        let mut encoded_count = 0;
        let mut sent_bytes = 0;
        for event in events {
            if let Some(action) = encode(
                &event.logical,
                &event.physical,
                &event.mods,
                event.repeat,
                &self.bindings,
                self.modes,
            ) {
                encoded_count += 1;
                if let InputAction::Send(bytes) = action {
                    sent_bytes += bytes.len();
                }
            }
        }
        let encode_time = encode_started.elapsed();

        InputBenchResult {
            event_count: events.len(),
            encoded_count,
            sent_bytes,
            encode_time,
            total_time: started.elapsed(),
            metadata: BenchMetadata::current(),
        }
    }
}

impl Default for InputBench {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot headless input benchmark (N26b).
pub fn run_input_bench(events: &[InputEvent]) -> InputBenchResult {
    InputBench::new().run(events)
}

/// One measured headless-input trial (N26b).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputBenchResult {
    /// Key events encoded.
    pub event_count: usize,
    /// Events that produced an action (the rest are `None` rows).
    pub encoded_count: usize,
    /// Bytes the terminal would receive (`InputAction::Send` totals).
    pub sent_bytes: usize,
    /// Time to encode every event.
    pub encode_time: Duration,
    /// Wall time over the whole trial.
    pub total_time: Duration,
    /// Build and machine metadata for the trial.
    pub metadata: BenchMetadata,
}

//! N26b: headless input instrumentation.
//!
//! Each trial encodes a deterministic list of key events through the
//! pure keyboard encoder (`nora::input::encode`): no terminal state, no
//! PTY, no display. Timings are informational in tests (shared CI is
//! noisy); the encoded count and the sent-byte total are the
//! correctness gates (the N09a pinned byte table).

use std::time::Duration;

use nora::dev::bench::BenchMetadata;
use nora::dev::input_bench::{InputBench, InputBenchResult, InputEvent, run_input_bench};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey};

/// A deterministic event list: 500 plain `a` presses, then 100 of each
/// of Ctrl+A, Alt+A, Enter, Backspace and Escape — 900 events, all of
/// which encode under the N09a pinned byte table:
/// `a` → 1 byte, Ctrl+A → `0x01` (1 byte), Alt+A → `0x1b` + `a`
/// (2 bytes), Enter → `0x0d`, Backspace → `0x7f`, Escape → `0x1b`
/// (1 byte each). Total: 500 + 100 + 200 + 100 + 100 + 100 = 1100 bytes.
fn sample_events() -> Vec<InputEvent> {
    let none = ModifiersState::empty();
    let mut events = Vec::with_capacity(900);
    for _ in 0..500 {
        events.push(InputEvent {
            logical: Key::Character("a".into()),
            physical: PhysicalKey::Code(KeyCode::KeyA),
            mods: none,
            repeat: false,
        });
    }
    for _ in 0..100 {
        events.push(InputEvent {
            logical: Key::Character("a".into()),
            physical: PhysicalKey::Code(KeyCode::KeyA),
            mods: ModifiersState::CONTROL,
            repeat: false,
        });
    }
    for _ in 0..100 {
        events.push(InputEvent {
            logical: Key::Character("a".into()),
            physical: PhysicalKey::Code(KeyCode::KeyA),
            mods: ModifiersState::ALT,
            repeat: false,
        });
    }
    for _ in 0..100 {
        events.push(InputEvent {
            logical: Key::Named(NamedKey::Enter),
            physical: PhysicalKey::Code(KeyCode::Enter),
            mods: none,
            repeat: false,
        });
    }
    for _ in 0..100 {
        events.push(InputEvent {
            logical: Key::Named(NamedKey::Backspace),
            physical: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            mods: none,
            repeat: false,
        });
    }
    for _ in 0..100 {
        events.push(InputEvent {
            logical: Key::Named(NamedKey::Escape),
            physical: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            mods: none,
            repeat: false,
        });
    }
    events
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

/// Common shape checks for every trial: every event encodes and the
/// sent-byte total matches the N09a pinned byte table.
fn assert_trial(result: &InputBenchResult, events: &[InputEvent]) {
    assert_eq!(result.event_count, events.len());
    assert_eq!(result.encoded_count, events.len());
    assert_eq!(result.sent_bytes, 1100);
}

#[test]
fn input_bench_encode() {
    let events = sample_events();
    let result = run_input_bench(&events);
    assert_trial(&result, &events);
    // Encode time is measured and contained in the total.
    assert!(result.encode_time > Duration::ZERO);
    assert!(result.total_time >= result.encode_time);
    // The `InputBench` runner and the one-shot `run_input_bench` encode
    // the same events through the same encoder.
    let via_runner = InputBench::new().run(&events);
    assert_trial(&via_runner, &events);
    assert_metadata(&result.metadata);
}

#[test]
fn input_bench_metadata() {
    let events = sample_events();
    let result = run_input_bench(&events);
    assert_metadata(&result.metadata);
    assert_eq!(result.metadata.package, "nora");
    assert!(result.metadata.logical_cpus > 0);
}

# Resource Limits

This document audits all growth limits in nora. Each section documents the limit, where it's enforced, how it's tested, and any known exceptions.

## History/Scrollback

**Limit:** Configurable via `Engine::with_history(rows, cols, scrollback)`. Default is 0 (no history).

**Enforced:** `src/engine.rs:472` — `Engine::with_history` creates a `Term` with `scrolling_history: scrollback`.

**Tested:** `tests/engine_snapshots.rs` — `scrollback_rows_are_stored_and_scrolled`.

**Notes:** The history size is bounded by the `scrollback` parameter. When the history is full, the oldest lines are dropped.

## Extra Cells

**Limit:** Each cell carries `zerowidth: Vec<char>` (combining marks) and `hyperlink: Option<(String, String)>`.

**Enforced:** `src/engine.rs` — `CellText` struct. The `zerowidth` vector is bounded by the input (the terminal only emits combining marks that follow a base character).

**Tested:** `tests/engine_snapshots.rs` — `combining_marks_are_preserved_in_snapshot`.

**Notes:** The `zerowidth` vector is bounded by the number of combining marks in the input. No explicit cap is set, but the terminal protocol limits this.

## Control Strings

**Limit:** The PTY writer uses a `BoundedWriter` with a configurable cap.

**Enforced:** `src/session/pty.rs` — `BoundedWriter` with `cap: usize`. The writer returns `Ok(0)` when the cap is reached.

**Tested:** `tests/session_bounded.rs` — `writer_stops_at_cap`.

**Notes:** The cap is set by the caller (typically 1 MiB). The writer is bounded by both the cap and the PTY's write buffer.

## Hyperlinks

**Limit:** Each cell carries `hyperlink: Option<(String, String)>`.

**Enforced:** `src/engine.rs` — `CellText.hyperlink`. The hyperlink is set by the terminal protocol (OSC 8).

**Tested:** `tests/engine_snapshots.rs` — `hyperlinks_are_preserved_in_snapshot`.

**Notes:** The hyperlink strings are bounded by the input. No explicit cap is set, but the terminal protocol limits this.

## Titles

**Limit:** The window title is set via `Window::set_title`. The title is bounded by the OS (typically 255 characters).

**Enforced:** `src/app/mod.rs` — `set_title` is called with the title from the terminal (OSC 2).

**Tested:** Not directly tested (OS-dependent).

**Notes:** The title is bounded by the OS. No explicit cap is set in nora.

## Font/Glyph Cache

**Limit:** glyphon's `TextAtlas` auto-grows from 256² up to the device's max texture dimension. It never frees allocated rects.

**Enforced:** `src/app/graphics.rs` — `TextAtlas` is created with a fixed size and grows as needed.

**Tested:** Not directly tested (GPU-dependent).

**Notes:** The atlas is bounded by the device's max texture dimension. If the atlas is full, `PrepareError::AtlasFull` is returned.

## Accepted+Pending Bytes

**Limit:** The `BoundedWriter` has a configurable cap (typically 1 MiB).

**Enforced:** `src/session/pty.rs` — `BoundedWriter` with `cap: usize`.

**Tested:** `tests/session_bounded.rs` — `writer_stops_at_cap`.

**Notes:** The writer is bounded by both the cap and the PTY's write buffer.

## Protocol Events

**Limit:** The `PumpMsg` and `AppEvent` channels are unbounded (mpsc::channel).

**Enforced:** `src/session/pump.rs` — `mpsc::channel` is used for both channels.

**Tested:** Not directly tested.

**Notes:** The channels are unbounded, but the events are small (a few bytes to a few KB). The risk of memory exhaustion is low.

## Paste

**Limit:** The paste policy is bounded by the `BoundedWriter` cap.

**Enforced:** `src/session/pump.rs` — Paste is sent via `PumpMsg::Input(Vec<u8>)`, which is bounded by the writer.

**Tested:** `tests/pty_session.rs` — `slow_child_input_no_loss_in_order`.

**Notes:** Paste is bounded by the writer cap.

## Snapshots

**Limit:** The snapshot slot is a single `Arc<Mutex<Option<Snapshot>>>`.

**Enforced:** `src/session/pump.rs` — The slot holds only the latest snapshot.

**Tested:** `tests/pump_session.rs` — `snapshot_slot_holds_latest_only`.

**Notes:** Only one snapshot is held at a time. Old snapshots are dropped.

## Jobs

**Limit:** The job queue is unbounded (mpsc::channel).

**Enforced:** `src/session/pump.rs` — `mpsc::channel` is used for the job queue.

**Tested:** Not directly tested.

**Notes:** The job queue is unbounded, but jobs are small (a few bytes). The risk of memory exhaustion is low.

## Known Exceptions

1. **Protocol events and jobs:** The channels are unbounded. This is acceptable because the events/jobs are small and the rate is bounded by the PTY's output rate.

2. **Font/glyph cache:** The atlas is bounded by the device's max texture dimension, but the exact limit is device-dependent.

3. **Hyperlinks and titles:** These are bounded by the terminal protocol, not by nora. No explicit cap is set.

## Escape-sequence frame caps (2026-10-15, M1.7a)

| Limit | Value | Behaviour past the limit | Evidence |
| --- | --- | --- | --- |
| OSC frame | 64 KiB (`prompt_marker::MAX_OSC_BYTES`) | frame dropped whole at its terminator; the parser never sees it | unit `unterminated_osc_is_bounded_and_dropped`; e2e `flood`: 200 MiB unterminated OSC, peak RSS +5.5 MiB (was +798 MiB) |
| APC frame | 4 MiB (`graphics_filter::MAX_APC_BYTES`) | frame ignored for graphics | unit `unterminated_apc_is_bounded` |

## Input bounds per pane (2026-10-15, M1.7c)

| Stage | Limit | Past the limit |
| --- | --- | --- |
| Pump queue (`pump::MAX_PENDING_INPUT`) | 2 MiB | keys and pastes refused (not accepted then dropped) |
| Session writer (`pty::DEFAULT_CAP`) | 1 MiB (64 KiB reserved for replies) | `feed` accepts nothing |
| In flight to the PTY (`pty::MAX_IN_FLIGHT`) | 1 MiB | `flush` holds bytes in the writer |

Evidence: `tests/pty_session.rs` `input_to_a_non_reading_child_is_bounded`.
The window-to-pump channel is drained on every pump pass. A session close
can still stall the pump for up to 5 s (audit P6, next slice); during that
window the channel holds whatever the window sent.

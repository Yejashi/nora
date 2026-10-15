# ADR-011: Kitty-graphics observation — byte-level APC filter + pinned-crate hook

- **Status**: accepted
- **Date**: 2026-10-14
- **Deciders**: nora team
- **Supersedes**: none

## Context

Kitty-style graphics (N32/N33) arrive as a terminal escape sequence that the
pinned engine must not corrupt the screen with, and that nora must observe in
order to render the image. Two facts constrain the design (verified against
the pinned sources):

1. **The sequence is an APC, not a DCS.** A kitty-graphics frame is
   `ESC _ G <control data> ; <payload> ESC \`. In pinned `vte 0.15.0`,
   `ESC _` (`0x5F`) enters `State::SosPmApcString` (`vte/src/lib.rs:377`),
   which is handled by `anywhere` (`lib.rs:438`): every byte is dropped except
   `0x18`/`0x1A` (→ `execute` + `Ground`) and `0x1B` (→ `reset_params` +
   `Escape`). The APC payload never reaches the grid. (The DCS machinery,
   `advance_dcs_entry`/`advance_dcs_passthrough`/`action_hook`, is a different
   path and is not used by kitty-graphics.)

2. **Both `nora` modes route child output through the pinned `EventLoop`, not
   `Engine::feed`.** The windowed mode (pump thread, `src/session/pump.rs:480`)
   and the headless mode (`src/main.rs` `run_session`, N05b) both spawn a
   `PtySession`, which builds the pinned `EventLoop::new(engine.term_handle(),
   engine.recorder(), pty, …)` (`src/session/pty.rs:262`). That loop reads the
   PTY master itself (`alacritty_terminal` `event_loop.rs::pty_read`) and drives
   the shared `Term` through its **own** `Parser` (`event_loop.rs:154,404`),
   bypassing `Engine::feed`/`Engine.processor`. The `Engine::feed` path (the
   `Session` type) is exercised only by tests.

Therefore a filter placed only in `Engine::feed` would cover tests but not the
actual terminal. The filter must run over the raw PTY bytes on **both** paths.

## Options

1. **Filter in `Engine::feed` only** — minimal, but does not cover the actual
   terminal (both modes use `PtySession`). Rejected for v0.1.
2. **Re-architect the PTY read** — nora owns the PTY read loop and reimplements
   the pinned `EventLoop`'s child-exit/drain/resize/`Msg` handling. Clean long-
   term, but a large, risky change for v0.1.
3. **Byte-level APC filter + minimal pinned-crate hook** — a small stateful
   filter over raw bytes, hooked into `Engine::feed` (headless/tests) and into
   the pinned `EventLoop::pty_read` (PTY) via a minimal, additive, documented
   patch. Selected.

## Decision

- **Observation is a byte-level APC framing filter, not a VT parser.** A small
  stateful filter (`GraphicsFilter`) runs over raw PTY bytes before the engine
  feed. States: `Pass`, `SawEsc`, `Apc` (collecting), plus a **byte-bounded**
  payload buffer. Transitions mirror vte's APC handling exactly so suppression
  never desyncs the parser:
  - `Pass`: forward bytes; on `0x1B` hold it → `SawEsc`.
  - `SawEsc`: on `0x5F` (`_`) suppress held `0x1B` + this byte → `Apc`; else
    forward held `0x1B` + this byte → `Pass`.
  - `Apc`: collect into the bounded buffer. On `0x18`/`0x1A` (CAN/SUB) frame
    complete → emit event, → `Pass`. On `0x1B` (the `ESC \` ST, or a mid-data
    abort) emit event (payload = bytes so far), forward the `0x1B`, → `Pass`.
    `0x9C` (C1 ST) is collected, not a terminator (vte's `anywhere` has no
    `0x9C` arm). On bound exceeded: discard payload, keep consuming to the
    terminator, → `Pass`.
  A complete APC frame is suppressed from the parser; a frame interrupted by
  `ESC` is suppressed up to that `ESC`, which is forwarded (it starts a new
  escape sequence). Because the filter mirrors vte's APC transitions, the
  parser ends in the same state it would reach had it seen the frame.

- **Plug-in points.** Headless/tests: in `Engine::feed`, immediately before
  `self.processor.advance(...)` (`src/engine.rs:538`); filter state lives on a
  new `Engine` field. PTY: in the pinned `EventLoop::pty_read`, immediately
  before `state.parser.advance(...)` (`event_loop.rs:154`), via a minimal patch
  (below).

- **Pinned-crate patch (minimal, additive, documented).** Add to
  `alacritty_terminal` `event_loop.rs` a `Filter` trait
  (`fn process(&mut self, bytes: &[u8]) -> Vec<u8>`) and an
  `Option<Box<dyn Filter + Send>>` field on `EventLoop`, settable via a
  constructor argument (default `None`, preserving existing behavior). In
  `pty_read`, when the filter is present, run `filter.process(&buf[..unprocessed])`
  and feed the returned bytes to the parser. nora provides its
  `GraphicsFilter` (wrapped to satisfy the trait; nora keeps a shared handle to
  drain parsed events). The patch changes no existing behavior when the filter
  is `None`.

- **Payload parsing: hand-written, no new dependency.** The control data is a
  comma-separated `key=value` list (not JSON) followed by the (base64-encoded)
  binary payload. `serde_json` is not in `Cargo.lock`; a ~50-line parser
  (command char, split control data on `,`, parse `key=value`, treat the
  remainder as the payload) is the narrowest reading. No new dependency.

- **Event delivery: a separate byte-bounded queue on `Engine`** (e.g.
  `VecDeque<GraphicsEvent>`, bounded in **bytes**, a few MB), polled by the
  render loop — not a new `EventRecord` variant. Payloads are large binary
  blobs (MBs); the `Recorder` caps on String-owned bytes (`MAX_RECORD_BYTES =
  1 MiB`) with drop-oldest, so a large payload would evict unrelated terminal
  events. A dedicated byte-bounded queue keeps `Recorder` semantics intact and
  satisfies the "bound in bytes as well as messages" rule.

## Consequences

- Kitty-graphics is observable on both `nora` modes (windowed + headless) and
  in tests.
- The pinned `alacritty_terminal` crate is patched (one additive `Filter`
  hook). nora pins the patched version; the patch is minimal and behavior-
  preserving when the filter is absent.
- The filter is a byte-level pre-filter for one escape family (APC); all VT
  parsing remains the pinned engine's (no new VT parser or screen model).
- Unbounded APC payloads are bounded by the filter's byte cap (discard-on-
  exceed, keep consuming to the terminator).
- Resize/shutdown mid-frame: a frame in flight at shutdown is simply
  incomplete — discard the partial payload; the parser stays consistent because
  the filter mirrors it. No special handling beyond not panicking on a
  truncated frame.
- N33 (graphics-basics) builds on this hook: parse the payload, store the image
  in a bounded store, render it in the render pass.

## Amendment (2026-10-15): deferred read errors in `pty_read`

A second vendored change, unrelated to graphics: `EventLoop::pty_read`
defers a read error (EIO when the child exits) that follows bytes already
read, parses those bytes with a blocking lock, and only then returns the
error. Upstream 0.26.0 returns immediately and drops the bytes whenever the
previous `try_lock_unfair` failed, so a fast-exiting child's final output
was lost under lock contention (CI flake in `cwd_is_honored`; deterministic
reproduction in `tests/pty_session.rs`
`output_of_exiting_child_survives_a_busy_terminal_lock`). Candidate for
upstreaming.

## Amendment (2026-10-15): in-flight input counter

A third vendored change: `EventLoop::with_in_flight(Arc<AtomicUsize>)`.
nora adds the bytes of each `Msg::Input` it sends; `pty_write` subtracts
what the PTY accepted. Upstream's input channel and write list are
unbounded, so nora's writer budget, released on send, did not bound input
memory when the child stops reading. With the counter, `PtySession::flush`
keeps at most `MAX_IN_FLIGHT` (1 MiB) between nora and the PTY (test
`input_to_a_non_reading_child_is_bounded`: 16 MiB offered, at most 4 MiB
accepted; all 16 MiB without the cap).

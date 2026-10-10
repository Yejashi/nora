# Validation plan

Every feature has meaningful evidence. A formatting-only change does not need a fabricated behavioral test. Core terminal, session, input and rendering changes do need tests that can catch broken behavior.

## Layers

| Layer | Runs headlessly? | Targets |
| --- | --- | --- |
| Unit/pure functions | Yes | Key encoding, layout, queue accounting, config validation, cache keys |
| Engine replay | Yes | Chunk invariance, modes, screen state, selection/reflow and query events |
| Real PTY integration | Linux without desktop | Child/session behavior, partial writes, replies, resize, signals and exit drain |
| Offscreen renderer | With a usable GPU/software adapter | Geometry, glyph clipping, cache invalidation, shader correctness |
| Desktop smoke | Display/compositor needed | Native Wayland/X11 lifecycle, clipboard/IME/focus/resize |
| Manual apps/hardware | User environment needed | TUIs, SSH/tmux, mixed DPI, NVIDIA/AMD, physical latency |
| Stress/property/fuzz | Headless or display by target | Memory bounds, parser recovery, scheduling and state races |

## Required regression families

### Engine adapter

Fixtures must have independently specified expected cells/modes/cursor/replies, not just snapshots produced by the same implementation. Include `abc\rZ` cursor overwrite, line wrap at a chosen small width, erase, scrolling margins, truecolor/reverse/conceal, alternate screen restoration, CJK last-column behavior, combining marks, hyperlink metadata, and primary history vs alternate-screen behavior.

Replay short byte fixtures whole, at every split boundary and with seeded random chunking. Compare final state and ordered reply bytes. Invalid/truncated UTF-8 and unterminated control strings must recover according to the pinned engine's tested contract. Compare selected cases to xterm/Alacritty, but treat differences as investigated behavior rather than universal oracle truth.

### Session and PTY

Use small Rust child helpers, not a fragile shell sleep-based synchronization scheme. A helper can report readiness, enable raw mode, echo tagged input, query/inspect window size, issue DSR, stop reading, emit final bytes and exit with a chosen code.

Test failed executable launch, exact argv with spaces/metacharacters, user working directory, partial reads/writes, input order, slow child backpressure, query flood, close while input is pending, child exit with buffered output, SIGWINCH, Ctrl+C with normal termios, repeat close/reap, descendant-held PTY timeout and `--hold`. Timeouts protect tests from hanging; readiness/acks establish sequencing.

Use artificially small queue caps to force overflow. Verify reservations are released exactly once after write/cancel/exit. Ensure a full queue cannot deadlock a parser callback or prevent shutdown. Do not infer bounded memory from the happy path.

### Rendering

Test engine-column to physical-pixel placement, wide spacers, combining attachments, fixed-cell fallback, decorations, cursor shapes, selection/search overlays and per-pane scissor. A tiny atlas forces eviction; retained geometry must never sample another glyph after UV reuse. Stale geometry/font epochs must trigger discard or rebuild.

Pin an attributed test font for stable offscreen cases; use tolerances where raster variation is expected. Include numerical geometry assertions so visually plausible but shifted output fails. Test complete fresh frames after swapchain reuse rather than relying on old pixels.

### Input and platform

Test legacy/application arrows, controls, Backspace choice, keypad, modifiers, key repetition and negotiated keyboard flags with exact byte tables. Replay IME preedit/commit/cancel to prove one text emission; then verify real native IME. Test Shift selection override, mouse modes, focus changes, ordinary/bracketed paste including terminator injection and canceled transactions.

Clipboard test matrix covers native Wayland and X11 ownership transfer, lifetime, delayed reads, copy/paste of wrapped/wide text, unavailable backend, and default rejection of OSC 52. URL activation must require an explicit local gesture and reject unsupported schemes without invoking a shell.

### Workspace and configuration

Layout property tests cover pane rectangles within the window, no overlapping child regions, deterministic remainder-pixel distribution, min grid sizes, split/collapse/focus paths and tab reorder/close. Small windows cannot request a zero-column PTY. Test tab/pane lifetime events using stale generations.

Config tests cover invalid ranges/colors/fonts/keys, unknown fields, missing/invalid files, CLI override precedence and transactional reload. Font/config changes cannot partially corrupt a working layout. Defer new shell/env settings to new sessions and document that behavior.

## App matrix

Run and record native backend, versions and results for: interactive shell editing/history; `less` search/scroll; `vim`/`nvim` alternate screen/resize; `htop` or an equivalent TUI; tmux panes/input; SSH to a host with matching terminfo or explicit fallback; a non-US layout; IME; and fallback/CJK text. No system app is silently installed by tests. Missing prerequisites are pending, not pass.

## Fuzzing and resource audit

Fuzz engine adapter byte/chunk input and session event sequences with a finite grid/history. Assert no wrapper panic, no invalid index/lifetime, and coherent shutdown. A fixed fuzz time budget is a smoke check, not a proof of safety. Minimize failures into deterministic fixtures.

Audit all growth paths: history extras, combining text, hyperlink/title strings, protocol payloads, pending writes, event queues, stale search results, font discovery/raster caches, atlas pages and worker jobs. Validate limits with both a blocked writer and attacker-controlled output. Distinguish enforced caps from measured/estimated caches. If a library cannot enforce a required bound, patch minimally or keep the release gate blocked with a specific defect.

## Feature completion evidence

For each card record:

- Behavior added and test names/fixture paths.
- Commands actually run, pass/fail and environment.
- Manual display/hardware checks passed or pending.
- Compatibility/resource/performance impact where relevant.
- Final feature commit and verified push result in the post-commit report.

Do not double-run a passing full suite without a reason. Broaden testing when a changed boundary, failure or unresolved issue warrants it. Performance measurements have their own procedure in [04-performance.md](04-performance.md).

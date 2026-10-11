# nora implementation progress

Plan created: 10 October 2026. Implementation started at N00 on 10 October 2026.

## Current state

- Last completed feature: `N04b-session-writer` (slice 2 of N04) — `validated`, commit `569fd43`, push verified. **N04 complete.**
- Active feature: `N04-bounded-io` (card slices: readiness spike → N04a byte reservation/pending writer → N04b parser callback + shutdown recovery):
  - Spike (done, worker-0, pinned alacritty_terminal 0.26.0 sources): PTY→loop path is an internal unbounded mpsc (`Msg::Input/Resize/Shutdown`); pending writes in a private unbounded `State::write_list`, private `Writing` progress — no public accessors; no per-byte drain acks and no write-side byte caps (read-side caps only: `READ_BUFFER_SIZE`, `MAX_LOCKED_READ`); `Notify::notify` fire-and-forget; PTY I/O errors kill the loop silently (`event_loop.rs:299`) → the `spawn` `JoinHandle` resolving is the loop-death signal; child exit = `Event::ChildExit(ExitStatus)`; `Pty::Drop` SIGHUPs + waits (shutdown serviceable); `tty::EventedPty` public (fake-pty capable), `Pty::from_fd` takes arbitrary child fds. Conclusion: nora drives its own bounded writer over the PTY (architecture §Bounded I/O: `session/io.rs`); reservation released on sink completion / cancel / exit.
  - N04a (reported green; commit round in progress): `src/session/io.rs` — `BoundedWriter` (one ordered queue of kind-tagged chunks, budgets computed once in `new`, one sink call per `flush` over a scratch buffer bounded by `max_bytes`, sink `Err` keeps all attempted bytes queued) + `src/lib.rs` wires `pub mod session;`. `tests/session_writer.rs` 9/9. Check set green: fmt/clippy(-D warnings)/build clean, `cargo test` 33/33. Lead reconciled one fixture slip (strict carve-out formula kept; tests 4–6 `max_bytes` 10→12). Note: `WriterError` carries a manual `PartialEq` (variant equality) — public API identical to the fixed signatures. Commit round: worker-1 (cargo + git), worker-0 read-only spike of the pinned engine's reply-producing sequences for N04b fixtures.
  - N04a shared signatures (lead decision, verbatim in both briefs):
    ```rust
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct WriterConfig {
        /// Caps all queued bytes (app + reply).
        pub max_bytes: usize,
        /// Strict carve-out of `max_bytes` that only reply bytes may occupy.
        /// Effective reply budget = min(reserved_bytes, max_bytes); the app
        /// budget is the rest, so a large paste never starves replies.
        pub reserved_bytes: usize,
    }

    #[derive(Debug, PartialEq)]
    pub enum WriterError {
        /// The writer has been closed (cancel/shutdown).
        Closed,
        /// Terminal sink error.
        Io(std::io::Error),
    }

    /// Bounded, ordered pending-write queue with a strict reply carve-out.
    pub struct BoundedWriter { /* private fields */ }

    impl BoundedWriter {
        pub fn new(cap: WriterConfig) -> Self;
        /// Accept ordered application bytes. Returns bytes accepted
        /// (0..=bytes.len(), possibly partial; 0 when the app budget is
        /// full). Accepted bytes are never dropped. Err(Closed) if closed.
        pub fn enqueue(&mut self, bytes: &[u8]) -> Result<usize, WriterError>;
        /// Accept ordered parser/control reply bytes from the reserved
        /// budget. Same return semantics as `enqueue`.
        pub fn enqueue_reply(&mut self, bytes: &[u8]) -> Result<usize, WriterError>;
        /// Send up to `limit` queued bytes, oldest first, to `sink` in one
        /// call. `sink` returns the bytes it completed; the un-completed
        /// remainder stays queued at the head, in order. Returns bytes
        /// completed. A sink error keeps all attempted bytes queued and
        /// propagates as `Err(Io)`. Err(Closed) if closed.
        pub fn flush<F: FnMut(&[u8]) -> std::io::Result<usize>>(&mut self, sink: &mut F, limit: usize)
            -> Result<usize, WriterError>;
        /// Bytes still reserved (queued app + reply). Always ≤ `max_bytes`.
        pub fn outstanding(&self) -> usize;
        /// Discard all queued bytes, release all capacity, close the writer.
        /// Idempotent: returns bytes released (0 on second call).
        pub fn cancel(&mut self) -> usize;
    }
    ```
  - N04a acceptance: fake blocked writer never exceeds the declared cap; capacity returned on write completion / cancel / exit; ordering preserved across partial writes and WouldBlock; reply carve-out prevents reply starvation; shutdown serviceable; no unbounded downstream queue.
  - Reply-byte spike (done, worker-0, pinned 0.26.0 `src/term/mod.rs:1336,1341`): DSR-5 `\x1b[5n` → `"\x1b[0n"` (4 bytes); DSR-6 `\x1b[6n` → `"\x1b[{row+1};{col+1}R"`, e.g. `"\x1b[1;1R"` (6 bytes) at origin; replies are synchronous inside `Processor::advance` — every parser turn that completes a DSR produces its reply in the same turn.
  - N04b (commit `569fd43`, push verified, review passed): `Session { engine: Engine, writer: BoundedWriter }` — `feed` drains all pending engine events every turn via `replies()`/`clear_events()` (no `src/engine.rs` changes); non-reply events are consumed and discarded (UI routing later); rejection visible via `FeedResult`; "replies don't block under the engine lock" holds by construction (headless), stated in the module doc. Shared signatures (lead decision, verbatim in both briefs):
    ```rust
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct FeedResult {
        pub replies_enqueued: usize,
        pub replies_rejected: usize,
    }
    pub struct Session { /* private */ }
    impl Session {
        pub fn new(rows: usize, cols: usize, cap: WriterConfig) -> Self;
        pub fn feed(&mut self, bytes: &[u8]) -> FeedResult;
        pub fn outstanding(&self) -> usize;
        pub fn flush<F: FnMut(&[u8]) -> std::io::Result<usize>>(&mut self, sink: &mut F, limit: usize)
            -> Result<usize, WriterError>;
        pub fn close(&mut self) -> usize;
    }
    ```
  - N04b acceptance: reply bytes (DSR) route through the bounded writer in order; split queries across feeds still reply once; a DSR flood at a small reserved budget rejects visibly (never dropped silently) and later small queries reject instead of dropping app bytes; `close()` releases everything and later replies reject as closed; `flush` after `close()` → `Err(WriterError::Closed)`.
  - Lead reconciliation (test 3 order): the brief listed the 11th DSR-6 feed after the flush, but then the queue is empty and the feed would be *accepted* (`{6,0}`); the test correctly runs feed-11 → flush so the three assertions (`{12,48}`, `{0,6}`, flush `Ok(12)` recording exactly 2 replies) hold simultaneously. Fixtures as written by worker-0 stand.
  - Lead reconciliation (Round 4): (a) reply acceptance is **atomic at the `Session` layer** — a reply is enqueued only if it fits wholly in the remaining reply headroom, else rejected in full (the `BoundedWriter` stays byte-granular; `Session` mirrors headroom via `queued_replies` + kind-tagged `runs`, credited back on `flush` completion). The `{12,48}` flood fixture demands this: 16 is not a multiple of 6, and byte-granular partial acceptance would split a reply mid-stream and report `{16,44}`. (b) worker-1 fixed `tests/session_bounded.rs` flood-length assert 30 → 40 (10 × 4-byte DSR-6 = 40; brief miscount) — behavioral assertions untouched. (c) `feed` after `close()` still feeds the engine; its replies reject as closed → `replies_rejected`.
  - N05 plan (lead draft, post-commit; spike findings pinned 0.26.0, worker-0): **N05a pty driver** — `src/session/pty.rs`: `PtySession::spawn(LaunchSpec { program, args, cwd, env }, rows, cols, WriterConfig) -> io::Result<PtySession>`; `Engine` gains `term_handle() -> Arc<FairMutex<Term>>` (the deliberately encapsulated sync boundary — the pinned `EventLoop::new(terminal, listener, pty, drain_on_exit, ref_test)` wants the same term the engine owns, `event_loop.rs:63`); spawn = `tty::new(&Options { shell: Some(Shell::new(prog, args)), working_directory, env, drain_on_exit: true }, WindowSize { num_lines, num_cols, cell_width: 0, cell_height: 0 }, window_id)` (`tty/unix.rs:195`; caller mints `window_id` as the session generation identity) then `EventLoop::new(...).spawn()` on a dedicated thread; app bytes → N04a `BoundedWriter` → `Msg::Input` via `channel().send` (`event_loop.rs:84,390`); resize → `Msg::Resize`; child exit → `Event::ChildExit(ExitStatus)`; loop death = `spawn` JoinHandle resolving; `drain_on_exit: true`, `ref_test: false`. **N05b launch & readiness** — headless readiness handshake over the PTY (no public facility in the crate — card mandates handshake/timeouts, no sleeps), tagged-I/O Rust child helper, direct argv for `-- COMMAND ARG...` (no shell interpolation — `Shell::new` passes argv straight to `Command::new`, `tty/unix.rs:215-218`), failed-launch cleanup (spawn `Err` = `"Failed to spawn command '<prog>': <err>"`; note: failed spawn leaks one global SIGCHLD hook slot per attempt, `tty/unix.rs:283-286`; bad `cwd` is silently ignored — validate `cwd` in nora before spawn), spawn env precondition: `$USER/$HOME/$SHELL` set or a passwd entry (`tty/unix.rs:213`). Test surfaces: fake `EventedPty` (two registered fds: token 0 I/O, token 1 child events) and real `Pty::from_fd` pair for headless PTY tests.
  - N05a fixed shared signatures (lead decision, verbatim in both Round-5 briefs):
    ```rust
    // src/session/pty.rs — re-exported from nora::session
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct LaunchSpec {
        pub program: String,
        pub args: Vec<String>,
        /// Pre-validated by nora (the pinned crate silently ignores a bad cwd).
        pub cwd: Option<std::path::PathBuf>,
        /// Extra env vars, added to the inherited environment.
        pub env: Vec<(String, String)>,
    }

    #[derive(Debug, PartialEq)]
    pub enum PtyError {
        /// cwd does not exist or is not a directory.
        BadCwd(std::path::PathBuf),
        /// Child could not be spawned (pinned crate wraps
        /// "Failed to spawn command '<prog>': <err>").
        Spawn(std::io::Error),
    }

    /// Per-session generation identity, minted once at spawn, attached to
    /// every event nora emits for that session.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct SessionId(u64);

    /// `status` is Some on clean child exit (Event::ChildExit), None when the
    /// loop died from an I/O or registration error. Not PartialEq-able
    /// (ExitStatus) — assert with `matches!` + `status.code()`.
    #[derive(Clone, Debug)]
    pub enum SessionEvent {
        /// The loop thread has ended.
        Terminated { session: SessionId, status: Option<std::process::ExitStatus> },
    }

    pub struct PtySession { /* private */ }
    impl PtySession {
        /// Mint id; pre-validate cwd (`PtyError::BadCwd`); spawn child
        /// (pinned `tty::new` + `EventLoop::new(..., drain_on_exit: true,
        /// ref_test: false)`) with the engine's shared term handle; run the
        /// loop on a dedicated thread; wire `Event::PtyWrite` → atomic
        /// reply headroom (shared `ReplyLedger`), `ChildExit` →
        /// `Terminated(Some)`, other events discarded.
        pub fn spawn(spec: LaunchSpec, rows: usize, cols: usize, cap: WriterConfig)
            -> Result<Self, PtyError>;
        pub fn id(&self) -> SessionId;
        /// App bytes into the writer's app budget (partial acceptance,
        /// `BoundedWriter::enqueue` semantics). Err(Closed) when closed.
        pub fn feed(&mut self, bytes: &[u8]) -> Result<usize, WriterError>;
        pub fn outstanding(&self) -> usize;
        /// Delivery to the child is internal: completed bytes are forwarded
        /// as ordered `Msg::Input` via the loop channel (no caller sink —
        /// unlike headless `Session`, the caller does not own the terminal).
        /// Sender disconnected (loop dead) → close + `Err(WriterError::Closed)`.
        pub fn flush(&mut self, limit: usize) -> Result<usize, WriterError>;
        /// Events emitted so far, in order (non-blocking drain).
        pub fn poll_events(&mut self) -> Vec<SessionEvent>;
        pub fn terminated(&self) -> bool;
        /// Block until the loop thread ends; exit status if clean child exit.
        pub fn join(&mut self) -> Option<std::process::ExitStatus>;
        /// Release writer bytes and send `Msg::Shutdown`. Idempotent.
        pub fn close(&mut self) -> usize;
        /// Same-crate accessor for engine state (tests now, UI later).
        pub fn engine(&mut self) -> &mut Engine;
    }
    ```
  - N05a other fixed points: `src/engine.rs` gains `pub fn term_handle(&self) -> <the shared term handle type N03 already holds in scope>` (the deliberately encapsulated sync boundary). `src/session/mod.rs`: `pub mod pty;` + re-exports `LaunchSpec, PtyError, SessionId, SessionEvent, PtySession`; extract the N04b reply-acceptance mirror into a private shared `ReplyLedger` used by both `Session` and `PtySession` (the six `tests/session_bounded.rs` tests must pass unchanged). Test helper bin `src/bin/nora_child.rs` (auto-discovered, no Cargo.toml edit; env `CARGO_BIN_EXE_nora_child` in integration tests) — exact behavior: no args → print `READY\n` (flush), then per line read from stdin print `E:<line>\n` (flush); `argv a1 a2 ...` → print `A:<i> <arg>\n` for i = 1.. over the args after `argv` (flush), exit 0; `pwd` → print `PWD:<absolute cwd>\n` (flush), exit 0; `status <n>` → print `READY\n`, exit code `<n>`. Tests (`tests/pty_session.rs`, worker-0) pump with deadline-bounded waits (10 ms poll, 10 s deadline) on observable markers only — no sleep guesses; engine text read via `session.engine()` using the snapshot idiom from `tests/engine_adapter.rs`.
  - N05a acceptance: real child receives accepted app bytes in order (READY marker + echoed line visible in engine cells); exact argv with spaces/metacharacters (`x;y`, `z*` stay literal — no shell); cwd honored; failed spawn → `PtyError::Spawn` (no panic/hang), bad cwd → `PtyError::BadCwd`; loop end surfaces as `Terminated { session, status }` with the session's own `id()` and `status.code()` == expected; `close()` idempotent. `-- COMMAND ARG...` CLI, tagged-I/O helper refinements and generic readiness detection = N05b.
  - N05a status (Round 5): worker-1 (node 1) reported green — `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build` clean; `cargo test` 44/44 (39 prior + 5 new in `tests/pty_session.rs`). Independent review (worker-0): pass-with-notes — ledger lockstep, helper contract, test surface, event-drain/thread-safety and AGENTS.md bounds verified OK. Lead reconciliations: (a) `drain_on_exit: true` (accepted deviation from the plan's `false`): with `false` the loop can process the child-exit token before the same-batch master read and trailing bytes are lost; `true` does one final master read on exit (03:105). (b) `Engine` holds the Term as `Arc<FairMutex<Term<Recorder>>>` (public `Recorder`); `Engine::term_handle()` and `Engine::recorder()` are the deliberately encapsulated sync boundary the pinned `EventLoop::new` requires; `feed`/`modes`/`cursor`/`snapshot` take the guard; the loop's event proxy is a shared `Recorder` clone. (c) Exactly one `Terminated` per session (first `ChildExit` wins; `status: None` when the loop JoinHandle resolves without one). (d) Pinned spawn is the free function `alacritty_terminal::tty::new(&Options, WindowSize, u64)` — not a `Pty` method; caller mints the window id (`SessionId` via `AtomicU64`, reused as the PTY window id). (e) One-line test fix accepted: `tests/pty_session.rs:59` `let mut last = String::new();` → `let mut last;`. Review fold-in (finding 1, should-fix, lead decision): main-thread drain switched from `events()` clone + `drained_events` cursor to a new atomic `Engine::drain_events() -> Vec<EventRecord>` (`mem::take` under the same mutex the loop thread pushes through) — no clone-then-clear loss window, no O(N) clone per turn, recorder bounded between main-thread drains; the `drained_events` cursor is removed. Doc notes folded: `PtyError::Spawn` reworded to "session could not be started"; `join`'s status is one-shot across surfaces; `Engine::feed` off-limits while the loop is alive (read state via `snapshot()`). Commit round in flight (worker-1): fold-in + full check set (expect 44/44) + commit + push + remote verify.
   - N05b fixed (lead decisions, verbatim in both Round-6 briefs):
    - `src/session/pty.rs` gains `pub const DEFAULT_CAP: WriterConfig = WriterConfig { max_bytes: 1024 * 1024, reserved_bytes: 64 * 1024 };` (policy values, 03:86-89).
    - `src/main.rs` (hand-rolled parsing, no parser crate): `-h/--help` → usage, exit 0; `-V/--version` → exit 0; `--working-directory PATH` (once, before `--`); `--` → rest is the command argv (program = first, args = rest; empty after `--` → stderr, exit 2); unknown flag → stderr, exit 2; no `--` → default shell = `$SHELL` or `/bin/sh` (documented fallback, 02:29) as `LaunchSpec { program: shell, args: vec![], .. }`. Headless pump: `PtySession::spawn(spec, 24, 80, DEFAULT_CAP)` (24×80 = documented headless placeholder until the UI cards) → `let status = session.join();` → `session.close()`. Exit status: explicit single command → `status.code().unwrap_or(1)` (signal death → 1; 02:52); default shell → 0 on clean close; any `PtyError` → stderr message, exit 1.
    - `src/bin/nora_child.rs` extensions (all prints flushed, 09:27): `stop <n>` → read until ≥ n bytes received (payload must end in `\n` for canonical mode), print `STOPPED`, then block on stdin reads until EOF → exit 0; `ws <rows> <cols>` → TIOCGWINSZ on stdin via libc, print `WS:<actual_rows>;<actual_cols>`, exit 0.
    - `Cargo.toml`: add `libc = "0.2"` (already locked at 0.2.190 via alacritty_terminal — matches the lock; no version drift).
    - Test `backpressure_with_stop_reading_child` (added to `tests/pty_session.rs`): spawn `nora_child stop 4096`, cap `{ max_bytes: 64 KiB, reserved_bytes: 16 KiB }`; pump until grid `contains("STOPPED")` (10 s deadline); feed 32768×`'A'` + `\n` → `Ok(32769)`; `flush(cap.max_bytes)` returns promptly (channel sink completes) and `outstanding() == 0` at every observation; 1 s bounded stall window: no `Terminated` while merely stalled; `close()` → helper EOF → exit 0 → pump for `Terminated { session: id, status }` — accept `Some(s) if s.code() == Some(0)` **or** `None` (post-close ChildExit-vs-EIO race documented, not papered over); second `close()` → 0.
    - Test `window_size_reaches_child` (added to `tests/pty_session.rs`): spawn `nora_child ws 24 80`; pump until grid `contains("WS:24;80")`; wait `Terminated` code 0.
    - `tests/cli.rs` (new; binary via `CARGO_BIN_EXE_nora`, helper via `CARGO_BIN_EXE_nora_child`): (1) `explicit_command_exit_status_propagates` — `nora -- <helper> status 7` → nora exits 7; (2) `no_command_after_dash_dash_is_an_error` — `nora --` → exit 2, stderr non-empty; (3) `unknown_flag_is_an_error` — `nora --bogus` → exit 2; (4) `help_and_version_exit_zero` — `--help`/`--version` exit 0 with non-empty stdout; (5) `working_directory_flag` — `nora --working-directory <temp> -- <helper> pwd` → exit 0; `nora --working-directory /nonexistent-nora-cwd -- <helper> pwd` → exit 1 (BadCwd).
    - N05b acceptance: explicit `-- COMMAND ARG...` runs argv directly and propagates the child's exit status; default-shell form implemented per 02:29; `--working-directory` honored, bad cwd exits 1; stop-reading child: nora-side bounds hold, no hang, no `Terminated` while stalled, clean termination after close; helper `stop`/`ws` modes per 09:27; launch defaults = 1 MiB/64 KiB.
  - N05b scope (lead draft, from requirements spike): `src/main.rs` gains `-- COMMAND ARG...` (direct argv, no parser crate — hand-rolled) + `--working-directory PATH`; default-shell resolution per 02:29; explicit single-command exit status propagated (02:52); launch defaults per 03:86-89 (accepted outbound 1 MiB / reserved reply 64 KiB per session); `nora_child` extensions per 09:27 (stop-reading, window-size verify, raw mode if dep-free); stop-reading backpressure test (nora-side boundedness only — the pinned loop's private `write_list` is unbounded inside the crate). OUT of N05b: `--hold`/lifecycle transitions (N12), `--config`/`--check-config`/`--working-directory`-config semantics (N18), color/size reply formatting + event coalescing + OSC clipboard (N06). Readiness in tests: helper READY markers + deadlines (09:29), no generic prompt detection (shell-integration card).
  - Test file layout (lead decision): `tests/session_writer.rs` (N04a), `tests/session_bounded.rs` (N04b).
- Accepted default: Rust + alacritty_terminal + winit + wgpu; text adapter subject to the N02 gate.
- Target: Linux native Wayland and X11.
- Git remote/branch: `origin` = github.com:Yejashi/nora.git, branch `main`, default branch.
- Required workflow: validate → update notes → commit → push → verify, for every completed feature change.

## Completed implementation increments

- **N04b-session-writer (slice 2 of N04)** — status: `validated`.
  - Commit `569fd43` ("feat(session): route engine replies through the bounded writer (N04b)"); remote `origin/main` = `569fd4366c60fb4e462cf0ef60ebc08b3b1c2b10` verified via `git ls-remote` after push (push `10f47ae..569fd43`).
  - Observable behavior: `nora::session::Session` — headless session owning `Engine` + `BoundedWriter`. `feed` = one engine parser turn, then drains ALL pending engine events every turn: `PtyWrite` replies accepted **atomically** into the reserved reply headroom (whole reply rejected if it does not fit; never split mid-stream), non-reply events consumed and discarded (UI routing later). `FeedResult { replies_enqueued, replies_rejected }` makes every acceptance/rejection visible. `outstanding()`, `flush` (one sink call, oldest first, sink `Err` keeps bytes queued), `close()` idempotent (releases everything; later feeds still parse but their replies reject as closed; `flush` → `Err(WriterError::Closed)`). Replies never block under the engine lock — holds by construction (headless).
  - `tests/session_bounded.rs` (6 tests): in-order DSR replies (`b"\x1b[0n\x1b[1;1R"`); DSR-6 split across two feeds replies exactly once; DSR flood at a 16-byte reply budget `{12,48}`, next single DSR-6 `{0,6}`, flush `Ok(12)` recording exactly two replies; `close()` releases 4 and a later DSR-6 rejects `{0,6}`; `flush` after `close()` → `Err(WriterError::Closed)`; empty feed no-op with empty flush `Ok(0)` and zero sink calls.
  - Validation (worker-1, rerun in commit round): `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build` all clean; `cargo test` 39/39.
  - Lead reconciliations: atomic reply acceptance at the Session layer (the `{12,48}` fixture demands it — 16 is not a multiple of 6; byte-granular partial acceptance would report `{16,44}` and split a reply); fixture flood length corrected 30 → 40 bytes (10 × 4-byte DSR-6); `feed` after `close()` still feeds the engine, its replies reject as closed.
  - Independent review (worker-0): **pass**, no blockers. Notes: (a) the flood-length assert is a trivially-true fixture guard (harmless); (b) the `runs` mirror requires every future app-byte path to go through the same ledger — satisfied by N05a's shared `ReplyLedger` design; (c) discarding non-reply events also drops PTY-query events (`ColorRequest`, `TextAreaSizeRequest`) — consistent with the N03 lossy-`EventRecord` decision; UI routing later.
- **N04a-bounded-writer (slice 1 of N04)** — status: `validated`.
  - Commit `10f47ae` ("feat(session): add bounded pending writer (N04a)"); remote `origin/main` = `10f47ae3d70bdb739b794a83959401d95eeafcec` verified via `git ls-remote` after push.
  - Observable behavior: `nora::session::io::BoundedWriter` — bounded, ordered pending-write queue. `WriterConfig { max_bytes, reserved_bytes }` is a strict carve-out (app budget = `max − min(reserved, max)`, reply budget = `min(reserved, max)`); `enqueue`/`enqueue_reply` accept ordered bytes with partial acceptance (accepted bytes never dropped); `flush` makes one sink call, oldest first, un-completed remainder stays queued, a sink `Err` keeps all attempted bytes queued and propagates `WriterError::Io`; `outstanding()` ≤ `max_bytes` always; `cancel()` is idempotent and closes (later ops → `Err(Closed)`).
  - `tests/session_writer.rs` (9 tests): app-budget cap; strict reply carve-out; ordered app+reply flush; partial write keeps head in order; WouldBlock keeps bytes queued; sink `Io` error keeps bytes queued; blocked writer never exceeds the cap (1000-iteration loop); cancel releases all and closes; empty enqueue is a no-op.
  - Validation (worker-1, rerun in commit round): `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build` all clean; `cargo test` 33/33.
  - Lead reconciliation: test fixtures 4–6 assumed 8-byte app headroom under a 10/4 config; the strict carve-out formula was kept and the fixtures moved to `max_bytes: 12`. `WriterError` uses a manual `PartialEq` (variant equality) — public API unchanged.
- **N03-engine-adapter** — status: `validated`.
  - Commit `9cdb72a` ("feat(engine): add headless terminal adapter and replay tests (N03)"); remote `origin/main` = `9cdb72a273782e372d3671ed4d4de421ed63007d` verified via `git ls-remote` after push.
  - Observable behavior: `nora` lib target exposes `engine::Engine` — headless adapter over pinned alacritty_terminal 0.26.0: `Engine::new(rows, cols)` (zero scrollback: `total_lines == rows`), `feed` (one long-lived `vte::ansi::Processor`; parser state preserved across chunks), `modes()`, `cursor()`, owned `snapshot()` consuming exactly one `TermDamage` report (first is `Damage::Full`), ordered `Recorder` sink (`EventListener` → owned `EventRecord`; PTY replies = `EventRecord::PtyWrite`) exposed via `events()`/`replies()`/`clear_events()`.
  - `tests/engine_adapter.rs` (8 tests): overwrite/CUP preserving other cells; pending wrap; SGR flags+colors until reset; ?1049 alternate-screen isolation and byte-exact restore; UTF-8 split invariance at every byte boundary (18-byte stream, 17 split points) and split escape sequence; ordered DSR/OSC-title reply capture; seeded SplitMix64 random chunking (64 runs, chunks 1–5) vs single-feed baseline.
  - Validation (worker-1, rerun in commit round): `cargo fmt --all -- --check` clean (one `Config` literal reflow in `src/engine.rs`); `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo build` OK; `cargo test` 24/24.
  - Independent review (worker-0): pass, 3 minor notes — `Snapshot::eq` excludes `colors` (palette changes invisible to snapshot equality; no fixture feeds palette changes); the wrap fixture is single-feed (cross-feed pending-wrap covered incidentally by random chunking); a no-op `snapshot()` reports `Partial` including the cursor cell (the pinned engine damages the cursor cell unconditionally, `term/mod.rs:479`) — keep in mind for N04+ damage coalescing.
  - Compatibility: engine-level evidence rows recorded in `docs/06-compatibility.md` (Text/UTF-8, cursor/erase/style, screens, queries — engine semantics only; PTY-level proof pending N04+).
- **N02b-text-spike (slice 2 of N02)** — status: `validated`; committed in this increment, remote head verified after push.
  - Observable behavior: `tests/text_render.rs` (1 test) renders a 5×2 cell grid (60×48 px, pure-black background) on the pinned tuple to a plain offscreen wgpu texture (no surface — headless via `InstanceDescriptor::new_without_display_handle`; wgpu 30 has no offscreen surface for this use), one cosmic `Buffer` per cell cluster and one `TextArea` per cell with `bounds` = the cell allocation: narrow `A`, combining `e\u{301}`, wide CJK `你` (span 2), box drawing `─` (span 1), Ethiopic `ᴀ`-class `ሀ` (span 2) via the pinned mono/CJK/Ethiopic faces. Every inked pixel lies inside its cell allocation (no column drift between terminal math and shaped geometry); PPM written to `/tmp/opencode/n02b_grid.ppm`.
  - Cache strategy (executable proof; replaces the initial SwashCache check): glyphon 0.12's `prepare` rasterizes misses via `SwashCache::get_image_uncached` and never writes `SwashCache.image_cache` — the persistent cache is `TextAtlas`' per-content `glyph_cache` (pub(crate), unbounded `LruCache` protected by `glyphs_in_use`), and the hit path (`GpuCacheStatus::InAtlas`) skips rasterization entirely. Proof: frame 3 re-renders the unchanged cells through a hermetic faceless `FontSystem` (`new_with_locale_and_db_and_fallback` with an empty `fontdb::Database`) — a miss would need to resolve a face and none exists, so the glyph would drop; frames 2 and 3 are both pixel-identical to frame 1, so every steady-state glyph was served from the atlas cache.
  - Measured (debug build, headless radv): cell width 12.0 px (whole, matches N02a); prepare frame 1 (cold) 4.67 ms, frame 2 (warm) 644 µs, frame 3 (faceless) 44 µs; full test ~0.18 s. Cache growth key is the distinct (font, size, codepoint) working set, not scrollback; the public mitigation is `TextAtlas::trim` (no public size introspection on `TextAtlas`).
  - API discoveries (verified against pinned sources): wgpu 30 delivers `map_async` callbacks from `Device::poll`/`Instance::poll_all` — blocking on the mpsc receiver alone hangs; working pattern: `map_async` → mpsc, then loop `device.poll(PollType::wait_indefinitely())` until the result arrives. `Buffer::get_mapped_range` returns a `BufferView` registering the range — drop the view before `unmap()` or wgpu panics ("cannot unmap a buffer that still has accessible mapped views"). `RenderPassColorAttachment` requires `ops: Operations`; `TextureDescriptor` requires `mip_level_count`, `sample_count`, `dimension`; `begin_render_pass`/`copy_texture_to_buffer` take `&mut self`. glyphon `bounds`/`left`/`top` are absolute surface coordinates in one shared space; per-glyph clipping is geometric.
  - Validation: `cargo fmt --all -- --check` clean; `cargo build` OK; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test` → 16 passed (nora bin unit 1, engine 4, font_stack 1, text_placement 9, text_render 1).
- **N02a-text-spike (slice 1 of N02)** — status: `validated`.
  - Committed in this increment; remote-head hash recorded after push verification.
  - Observable behavior: `tests/text_placement.rs` (9 tests) proves the per-cell model on pinned glyphon 0.12.0 / cosmic-text 0.19.0: one cosmic `Buffer` per cell cluster; cell origins are pure terminal math (`col * cell_w`, `row * cell_h`); narrow and combining clusters ≤ 1 cell (±0.5 px); CJK and Ethiopic clusters within (1, 2] cells with per-cluster provenance pinned to the three test fonts; scales 1.0/1.25/1.5/2.0 scale glyph size but not cell math; unchanged cells keep bit-identical geometry with no re-shape on frame 2; an 80×24 grid (1920 single-cell buffers) shows zero drift (min = max = 12.00 px advance at a 20 px font).
  - Measured (debug build, this machine): 1920 single-cell shapes in 178.8 ms — the CPU shaping cost the per-cell model pays; presentation cost is the N02b concern.
  - API discoveries (verified against pinned sources): `FontSystem::new_with_fonts` is not hermetic — cosmic-text calls `db.load_system_fonts()` first, so system faces (DejaVu, …) coexist in the db; family matches resolve to the system-scan copies whose `Source` is `SharedFile(path, _)` (fontdb 0.23), not `Source::File` — provenance checks must handle both arms. A CJK glyph at 20 px (~20 px advance) intentionally overflows one mono cell (~12 px); that overflow motivates the two-cell allocation, it is not a bug.
  - Pinned test fonts: `/usr/share/fonts/noto/NotoSansMono-Regular.ttf`, `/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc`, `/usr/share/fonts/noto/NotoSerifEthiopic-Regular.ttf`.
  - Validation: `cargo fmt --all -- --check` clean; `cargo build` OK; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test` → 15 passed (lib 1, engine 4, font_stack 1, text_placement 9).
- **N01-dependencies** — status: `validated`.
  - Commit: `chore(deps): pin compatible terminal graphics stack (N01)` on `main`; remote-head hash recorded at the next increment (never fabricate the hash of the commit being authored).
  - Observable behavior: `nora` (no args) opens a native window, prints platform/adapter/surface metadata and stays open until closed; `nora --smoke` runs one automated open (800x600) → resize to 640x480 → minimize → 250 ms settle → close cycle, exiting 0 on success and 1 on startup failure or timeout.
  - Pinned tuple (Cargo.lock committed): alacritty_terminal 0.26.0 (vte 0.15.0), glyphon 0.12.0 → cosmic-text 0.19.0, wgpu 30.0.1, winit 0.30.13, pollster 0.4. `cargo tree -d`: font-types 0.11.3/0.12.6 and other duplicates via harfrust/skrifa/gpu-allocator — benign, locked.
  - Live native Wayland run (10 Oct 2026, wayland-0 session): adapter AMD Radeon RX 9070 XT, Vulkan backend, DiscreteGpu, driver radv; surface `Rgba8UnormSrgb` at 800x600 scale 1.0; smoke exit 0; interactive run stayed open until killed at 5 s. Driver prints "radv is not a conformant Vulkan implementation, testing use only" — driver-side notice, not a failure.
  - Headless regression tests: `tests/engine.rs` (4 tests: bytes render into grid cells; CRLF starts new lines; bare LF keeps the column; SGR escape produces no cells) and `tests/font_stack.rs` (1 test: system monospace resolves through glyphon/cosmic-text).
  - API corrections verified against pinned sources: winit 0.30 has no `Window::close` (the window closes with the event loop); `StartCause::ResumeTimeReached` replaces `::Deadline` and lives in `winit::event`; wgpu 30 has no `winit` feature — `Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(el.owned_display_handle())))` + `create_surface(Arc<Window>)`; the wgpu `Surface` retains the `Arc<Window>`, so dropping nora's Arc does not close the window.
  - Validation: `cargo fmt --all` clean; `cargo build` and `cargo build --release` OK; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test` 6 passed; `timeout 30 ./target/release/nora --smoke` exit 0 on native Wayland; `nora --version` → `nora 0.1.0` exit 0; unknown flag exit 2.
  - Pending manual release checks: native X11 run (no X11 session in this environment; an XWayland run would not count as native X11).
- **N00-bootstrap** — status: `validated`, committed/pushed/verified.
  - Commit `c3a6707` ("chore(repo): bootstrap nora build (N00)"), remote `origin/main` = `c3a67072922d357c1ec35025d4c6d68e6aba2fda` verified via `git ls-remote`.
  - Observable behavior: clean checkout builds the `nora` binary; `nora --version` prints `nora 0.1.0`; `--help`/unknown flags exit with usage (exit 2); no terminal UI.
  - Added: `Cargo.toml` (v0.1.0, edition 2024, rust-version 1.99, no deps yet), `Cargo.lock`, `rust-toolchain.toml` (1.99.0), `.gitignore` (`/target`), `src/main.rs`, `.github/workflows/ci.yml` (fmt/clippy/build/test).
  - Checks run: `cargo fmt --check` OK; `cargo build --quiet` OK; `cargo clippy --all-targets` clean; `cargo test` 1 passed; `./target/debug/nora --version` → `nora 0.1.0`.
  - Decisions: toolchain pinned to 1.99.0 (installed stable); edition 2024; release profile `lto = "thin"`, `codegen-units = 1`; GitHub Actions CI added because the configured remote is GitHub; no Python anywhere in project files.

When recording a task, use: ID; observable behavior; validation commands/results; remaining manual release checks. Status values: `not_started`, `in_progress`, `validated`, `blocked`. Overall completion additionally requires the post-commit push verification.

## Blockers and limitations

- No terminal content rendered yet (rendering arrives at N08+); headless engine adapter pushed at N03.
- Native X11 evidence pending (no X11 session in this environment).
- End-to-end session write/event limits not implemented.
- No performance evidence yet.

## Last verified Git state

- N00: commit `c3a6707`; N01: commit `a8beb8a` — remote `origin/main` = `a8beb8ae136c7acc997fc24eeea88bffcda9773b` (verified via `git ls-remote` after the N01 push).
- N02a: commit `402594a`; remote `origin/main` = `402594a2af5d4ca7d8d8f1c6944b0f901fe9c230` (verified via `git ls-remote` after the push).
- N02b: commit `a4fc206`; remote `origin/main` = `a4fc206e11436354f626003c173b2de7b9c446d0` (verified via `git ls-remote` after the N02b push).
- N03: commit `9cdb72a`; remote `origin/main` = `9cdb72a273782e372d3671ed4d4de421ed63007d` (verified via `git ls-remote` after the N03 push).
- N04a: commit `10f47ae`; remote `origin/main` = `10f47ae3d70bdb739b794a83959401d95eeafcec` (verified via `git ls-remote` after the N04a push).

## Next action

N05-pty-session: N04 closed (commit `569fd43`, push verified, review passed). N05a implemented (worker-1, reported green 44/44) and reviewed (worker-0, pass-with-notes; should-fix finding 1 folded into the commit round per the N05a status bullet). **N05a commit round in flight** (worker-1, node 1): `Engine::drain_events` fold-in + doc lines → full check set (expect 44/44) → stage the six N05a files → commit `feat(session): launch real PTY children (N05a)` → push → remote verify. After push verified: add the N05a completed entry + last-verified Git state here, then dispatch Round 6 N05b per the N05b fixed block (worker-1 implements + owns cargo: `DEFAULT_CAP`, `src/main.rs` CLI, `nora_child` `stop`/`ws` modes, `libc` dep; worker-0 writes tests without cargo: 2 additions to `tests/pty_session.rs` + new `tests/cli.rs`; expect 51/51) → N05b commit round → N06.

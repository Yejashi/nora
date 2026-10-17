# Architecture and ownership

This is a proposed design. Resolve actual APIs in N01–N03 and record changes in an ADR. Diagrams describe logical boundaries, not separate crates or separate GPU devices.

## Module boundaries

Start with one application crate and internal modules. Add a small `nora-dev` Rust package for benchmark/test helpers once it is needed. Split crates only when a boundary needs independent compilation or testing; avoid a dozen empty crates.

| Module | Owns | Must not own |
| --- | --- | --- |
| `app` | winit lifecycle, event routing, window UI and instance shutdown | VT state mutation logic |
| `workspace` | Tabs, pane tree, focus and integer pixel layout | PTY fd operations |
| `layout` | Split pane tree, active-pane focus, deterministic integer rectangles | PTY fd operations, session state |
| `session` | PTY lifecycle, session worker, ordered writes, engine synchronization | GPU resources |
| `engine` | Alacritty adapter, state access, mode mapping, snapshot extraction | Desktop clipboard/URL operations |
| `input` | Key/mouse/focus encoding, paste normalization, local binding resolution | Shell command execution |
| `render` | Font system, cell layout, caches, geometry, GPU device/surface | Shell state or VT parsing |
| `config` | Parse/validate/defaults/reload plan | Implicit execution of config strings |
| `platform` | Clipboard, desktop integration, opening approved URLs, platform diagnostics | Protocol parsing |
| `diagnostics` | Structured timing/counters and sanitized errors | Raw PTY/input recording by default |

```mermaid
flowchart TD
    A["Window and workspace"] --> B["Input and local actions"]
    B --> C["Session worker and PTY"]
    C --> D["Terminal engine under mutex"]
    D --> E["Owned visible snapshot"]
    E --> F["Font cache and GPU renderer"]
    D --> G["Policy-controlled UI requests"]
    G --> A
    F --> A
```

## Thread model

- **Main UI thread:** winit window/events; workspace; local actions; snapshot extraction; initially font/render submission. OS event-loop requirements remain here.
- **One I/O worker per pane:** PTY readiness, reads/parser turns, ordered pending writes and lifecycle. Reuse the engine's PTY facilities. Initially use the upstream worker in the feasibility spike; N03 resolves real queue bounds before shipping input.
- **One bounded background worker, added when needed:** search/config/font preparation tasks which would otherwise stall UI. It never holds the terminal lock while doing slow work. Use a fixed pool only if measurements require it.

The terminal is shared under the adapter's `FairMutex` boundary. The I/O worker mutates it during parsing; UI selection, resize and viewport actions also mutate it under that same lock. This is serialized shared ownership, not a claimed lock-free or single-writer model. Only adapter methods can acquire the engine lock.

A dedicated rendering thread is a later optimization trigger, not the first architecture. If surface acquisition or shaping stalls input materially, measure it and move GPU work to a per-window worker while keeping window/platform events on the UI thread. Do not add one renderer/device per pane.

## State and interface contract

Use typed `SessionId`, `PaneId`, `TabId` and generation counters. Events carry a session generation so queued work from an exited/replaced session cannot target a newly created pane. UI owns the pane tree; each pane refers to a session, not vice versa.

Proposed operations are semantic contracts, not assumed upstream method signatures:

| Operation | Contract |
| --- | --- |
| `Session::spawn(LaunchSpec)` | Creates PTY child/worker or returns a contextual error without orphaning resources |
| `enqueue_input(bytes)` | Ordered acceptance; byte-capacity reservation; reports full/closed; accepted data is not dropped |
| `request_resize(size, epoch)` | Latest pending geometry may replace older pending geometry; apply at a defined synchronization point |
| `Engine::snapshot(viewport, cache)` | Copies visible cells, cursor, modes, colors and damage to reusable owned storage |
| `Engine::select/scroll/search` | Serialized engine operations with limited work per call |
| `Session::close()` | Idempotent lifecycle transition; stops new input; closes PTY and reaps the owned child |
| `Renderer::prepare(snapshot)` | No engine reference or terminal lock; cache/geometry updates |
| `Renderer::present()` | Capability-checked surface handling; marks presentation state for diagnostics |

Snapshots carry cell text/combining marks, style, resolved or symbolic color, width flags, hyperlink identity, viewport/geometry epoch, cursor and overlay information. Engine references and borrowed renderable iterators cannot escape the lock scope. Take O(visible cells) snapshots first; optimize measured costs by reusing row buffers and copying changed rows. Never clone the entire scrollback per frame.

## Workspace (N23a)

`src/workspace.rs` owns the tab and pane tree and the active-tab/active-pane focus. It is a pure, headless data structure: no I/O, PTY or window dependency, so it is testable without a display. `TabId` and `PaneId` are small owned integer handles (`u32`), minted monotonically by the workspace and never reused within a process.

- **The workspace owns all tabs and panes.** `Workspace` holds the ordered tab list, the per-tab pane lists, and the active tab and per-tab active pane. Adding a tab creates it with one pane and makes it active only when no tab is active yet; the first tab is the initial focus.
- **Each tab has independent output/history/cwd/session lifecycle.** A tab is a container for one or more panes; each pane is the routing target for one independent session owned by the `session` module (its own `Engine`, scrollback, working directory and PTY lifecycle). The workspace only *routes* — it holds no engine, PTY or session state, so a tab's data cannot leak into another tab's.
- **Active input never reaches another tab.** Input, focus, clipboard and search actions are routed through the workspace's active tab/pane. Because each pane refers to exactly one session and the workspace exposes only the active pane, a keystroke delivered to the active pane cannot be observed by a background tab.
- **Closing a tab reaps its sessions.** `close_tab` removes the tab and its pane list. When the closed tab was active, focus falls back to the last remaining tab (or none when the workspace becomes empty). The session module reaps the closed tab's sessions (bounded drain, reap-once, idempotent close) using the same lifecycle as a single session.
- **Stale events are ignored.** Events carry a session generation (the `SessionId` minted at spawn). Queued work — a snapshot, a reply, a resize — from an exited or replaced session is tagged with that generation and discarded if it no longer matches the pane it targets, so a background flood cannot build a snapshot queue for a tab that is no longer active.

## Tab UI and routing (N23b)

The window wires the workspace into the event loop. `WinApp` owns a `Workspace` (one per window) and a per-tab presentation-state map (`tab_slots`, `tab_grid`, `tab_exit`, `tab_render`, `tab_ime`, `tab_search`, `tab_modes`); the active tab's state is mirrored into the single fields the render and input paths already read, so no other module sees a tab id.

- **Tab shortcuts.** The keyboard handler maps `Ctrl+T` → new tab, `Ctrl+W` → close the active tab, `Ctrl+Tab` → next tab, `Ctrl+Shift+Tab` → previous tab. Each maps to a [`nora::workspace::TabCommand`] applied through `Workspace::apply_tab_command`; the window then swaps the active tab's mirrored state (save the old, load the new) and, for `New`/`Close`, spawns or reaps the tab's session through the pump.
- **Active input never reaches another tab.** Every input, focus, clipboard, search, scroll, resize, IME and mouse event is tagged with the active tab id before it crosses into the pump (`PumpMsg::Input { tab, .. }`, `Resize { tab, .. }`, `Scroll { tab, .. }`, `Select { tab, .. }`, `Copy { tab }`, `Paste { tab, .. }`). The pump routes each message to that tab's session only, so a keystroke delivered to the active tab cannot be observed by a background tab.
- **Closing a tab reaps its sessions.** `Ctrl+W` sends `PumpMsg::CloseTab { tab }`; the pump closes that tab's session (bounded drain, reap-once, idempotent — the same lifecycle as a single session) and drops its slot. The window drops the tab's mirrored state and falls back to the last remaining tab.
- **Stale events are ignored.** Every pump `AppEvent` is tagged with its tab (`AppEvent::DirtyPulse { tab, .. }`, `ChildExit { tab, status }`, `TabGone { tab }`, …). The window's `on_tab_event` routes each event to its tab and drops any event whose tab no longer exists (`Workspace::has_tab`), so a background flood cannot build a snapshot queue for a tab that is no longer active. Per-tab presentation state is a `CoalescingSlot` — a background tab's snapshot flood coalesces to a single pending snapshot and never builds a queue.
- **The pump owns one session per tab.** `pump_thread` holds a `HashMap<TabId, TabPump>` (one `PtySession` + one `CoalescingSlot` per tab) and one engine event loop that drains all tabs' engines each tick. `PumpMsg::NewTab` spawns a session for a new tab; `PumpMsg::CloseTab` reaps one. The window's first tab is `TabId(0)`, matching the workspace's first `add_tab()`.

## Layout tree (N24a)

`src/layout.rs` owns the split-pane arrangement of a tab's panes. It is a pure, headless data structure: no I/O, PTY or window dependency, so it is testable without a display. The layout is a **binary split tree** (`LayoutNode`): leaves are panes (`PaneId`), internal nodes are splits with a direction (`Horizontal` = side by side, `Vertical` = stacked) and a ratio (the fraction of space given to the first child, 0.0 to 1.0).

- **Each pane has independent output/history/cwd/session lifecycle.** Each leaf is the routing target for one independent session owned by the `session` module (its own `Engine`, scrollback, working directory and PTY lifecycle). The layout only *arranges* panes; it holds no engine, PTY or session state, so one pane's data cannot leak into another's.
- **Physical integer rectangles with deterministic remainder allocation.** `Layout::compute_rectangles(width, height)` tiles the given cell area exactly: at every split the first child gets `floor(size * ratio)` and the second child gets the remainder. Rectangles are nonoverlapping and deterministic for a given tree and size.
- **Minimum grids (no zero grids).** When a split has at least two cells to divide, each child gets at least one cell (the first child's share is clamped to `[1, total - 1]`); a zero cell is produced only when fewer than two cells are available to divide, which is unavoidable.
- **Focus follows documented shortcuts.** The layout tracks one active pane. Splitting focuses the new pane; `focus_pane` moves focus to any live pane and reports stale targets; when the active pane is closed, focus falls back to the first remaining pane (the leftmost live leaf). The window maps the documented split/focus/close shortcuts to these operations (N24b).
- **Closing a pane collapses the tree consistently.** `close_pane` removes the pane and replaces the closed leaf by its sibling subtree at every enclosing split, so a dead leaf never survives below the root. Closing the last pane leaves an empty layout (`pane_count() == 0`, no rectangles); splitting an empty layout re-roots at the new pane.

## Split UI and routing (N24b)

The window wires the layout into the event loop. `WinApp` owns a per-tab `Layout` (`tab_layouts`) and a per-pane presentation-state map (`pane_slots`, `pane_grid`, `pane_modes`, `pane_kitty`, `pane_exit`, `pane_render`, `pane_search`, `pane_selection`, `pane_last_visible`); the active pane's state is mirrored into the single fields the render and input paths already read, so no other module sees a pane id. `pane_grid` is the one record of the grid last sent to each pane, the active one included. The IME composition is window-wide: a pane or tab switch drops the preedit instead of moving it with the pane. The pump is keyed by a `PaneKey { tab, pane }` (one `PtySession` + one `CoalescingSlot` per pane), so each pane has independent output, history, cwd and session lifecycle.

- **Split shortcuts.** The keyboard handler maps `Ctrl+D` → split horizontally (new pane to the right), `Ctrl+Shift+D` → split vertically (new pane below), `Ctrl+X` → close the active pane, and `Ctrl+Arrow` → focus the adjacent pane in that direction. Each maps to a `Layout` operation (`split_horizontal`, `split_vertical`, `close_pane`, `focus_pane`/`adjacent_pane`) on the active tab's layout; the window then spawns or reaps the pane's session through the pump (`PumpMsg::NewPane` / `ClosePane`) and swaps the active pane's mirrored state.
- **Active input never reaches another pane.** Every input, focus, clipboard, search, scroll, resize, IME and mouse event is tagged with the active pane's `PaneKey` before it crosses into the pump (`PumpMsg::Input { pane, .. }`, `Resize { pane, .. }`, `Scroll { pane, .. }`, `Select { pane, .. }`, `Copy { pane }`, `Paste { pane, .. }`). The pump routes each message to that pane's session only, so a keystroke delivered to the active pane cannot be observed by another pane.
- **Closing a pane collapses the tree consistently.** `Ctrl+X` sends `PumpMsg::ClosePane { pane }`; the pump closes that pane's session (bounded drain, reap-once, idempotent) and drops its slot. The window removes the pane from the layout (the sibling subtree takes its place), drops the pane's mirrored state, and falls back to a live pane; the next resize re-tiles the remaining panes.
- **Stale events are ignored.** Every pump `AppEvent` is tagged with its pane (`AppEvent::DirtyPulse { pane, .. }`, `ChildExit { pane, status }`, `PaneGone { pane }`, …). The window's `on_pane_event` routes each event to its pane and drops any event whose pane no longer exists (`Layout::has_pane`), so a background flood cannot build a snapshot queue for a pane that is no longer active. Per-pane presentation state is a `CoalescingSlot` — a background pane's snapshot flood coalesces to a single pending snapshot and never builds a queue.
- **Pane resize updates PTYs.** On a window resize (`Resized` / `ScaleFactorChanged`), `resize_all_panes` recomputes every pane's integer rectangle from the layout and sends a `PumpMsg::Resize` to each pane whose grid changed (change-only, so no no-op resizes reach the pump); the next pulse carries each pane's post-resize `Full` snapshot.

## Damage and snapshot retention

Use engine damage for content plus nora-owned invalidation for cursor, selection, search, focus, DPI, palette, atlas rebuild and viewport changes. Upstream damage excludes some UI-controlled elements [S07].

Reset engine damage only after its information/content have been captured into durable renderer-side state while holding the lock. A lost/superseded frame must not lose updates. The initial renderer keeps a complete latest visible snapshot. When snapshots become deltas, use a base revision and require a full rebuild on any mismatch. Do not build an unbounded queue of deltas.

Coalesce redraw wakeups: one pending wake per instance is sufficient, with a race-safe clear/recheck of dirty generations. A PTY event arriving while the UI clears the flag must still schedule work. Do not queue a cloned screen for every parser read.

## Bounded I/O strategy

N03 must inspect the pinned upstream worker. The current inspected source's unbounded channel and pending-write deque [S22] cannot be made bounded by adding a separate bounded queue that forwards without acknowledgements.

Preferred resolution if the pinned API has no end-to-end acknowledgements: adapt the small upstream I/O loop into `session/io.rs`, with source attribution, fixed buffers, a bounded ingress and a pending-write byte budget. Reuse the published PTY/engine types and parser. Verify exposed PTY readiness APIs compile first. This is a scoped I/O adapter, not a fork of the terminal engine. Record upstream revision and maintain a reconciliation note. If public hooks do not suffice, choose a minimal pinned patch or a different engine in an ADR before deeper integration.

Capacity must count queued **plus pending/in-flight application bytes**, not just channel occupancy. Reserve on acceptance and release as writes complete or cancellation/exit discards explicitly. Ordered parser-generated replies join the same writer. Large paste is chunked and only advances when capacity returns. Keep a small reserved control/reply budget so paste cannot prevent queries or shutdown; do not interleave keys into an open bracketed-paste payload. The paste/reply policy is specified in [06-compatibility.md](06-compatibility.md); a reserved budget alone does not solve protocol ordering. Shutdown/resize have a separate bounded control path. Never block the UI to acquire input capacity.

On PTY reads, use finite bytes/time per locked parse turn and service writes/control between turns. If parser callbacks need a full reply queue, retain the bounded unread tail and pause further reads until it drains; no callback may block under the terminal lock. If a protocol event exhausts an enforceable cap, reject it according to protocol or terminate that session with a visible resource-limit error; never continue after silently dropping semantic bytes. N03 tests the chosen recovery strategy with a query flood.

## Initial policy values

These are starting proposals, not established measurements. Validate configurability and accounting in N03/N24.

| Resource | Initial proposal | Overflow/action |
| --- | --- | --- |
| PTY read buffer | 64 KiB reusable buffer | Stop the turn; continue later without truncation |
| Locked parse turn | At most 64 KiB or about 2 ms, whichever comes first | Release lock/service control; timing is best effort |
| Accepted outbound bytes | 1 MiB per session, including pending writes | Backpressure; UI retains bounded paste chunks and indicates pending state |
| Reserved reply/control bytes | 64 KiB per session | Pause parser/read; hard-limit failure is visible |
| View snapshots | Current complete snapshot plus one reusable replacement | Replace presentation state; never lose engine bytes |
| UI protocol-event queue | 256 entries plus 1 MiB total payload | Coalesce replaceable titles/bells; reject capped operations or fail session safely |
| Paste transaction | Up to 8 MiB before any write | Reject larger paste with a user-facing message; no silent truncation |
| Tabs/panes | 32 sessions per instance | Refuse another spawn cleanly |
| Glyph cache | 64 MiB CPU + 64 MiB GPU target per instance | Account actual allocated atlas pages; evict/rebuild at a frame boundary |
| History | 10,000 rows and a target 64 MiB/session | Lower row limit based on geometry/accounting; audit exceptional engine allocations |

Flood audit (N06b) for the protocol-event queue row: the pinned vte parser (`features = ["std", "ansi"]`) accumulates an open OSC payload in an unbounded `Vec<u8>` (`osc_raw`, vte-0.15.0/src/lib.rs:62-64) — bytes are pushed unconditionally while an OSC stays open and the buffer is cleared only at `osc_end` — so a client can hold arbitrary in-flight bytes in the parser before any event exists. Retention after parsing is bounded: the shared `Recorder` keeps at most 256 records and 1 MiB of String-owned bytes (drop-oldest FIFO), with replaceable presentation state coalesced before eviction (latest title wins; a bell while one is pending is skipped). Sub-cap evictions are unaccounted (no `ReplyLedger` rejection); the main thread drains the queue between feeds, so the cap bounds post-parse retention, not in-flight parser bytes.

A row limit does not strictly bound memory when cells can carry extra text or links. Audit parser strings, combining marks, hyperlink storage, history extras and third-party font caches. Enforce caps using supported APIs or a documented minimal patch. If a strict cap is impossible, record the specific exception; do not label it bounded. The resource audit is a release gate, not a fixed-size-of-Cell approximation presented as a guarantee.

See [12-resource-limits.md](12-resource-limits.md) for the full audit of all growth limits (N25).

## Resize, font and shutdown lifecycle

Derive integer physical pixel cell metrics and pane rectangles from the latest scale/size; minimum usable grid is 2 columns × 1 row. Suspend rendering for a zero-sized/minimized surface; keep the last valid PTY size. Resize terminal/grid and PTY under a documented serialized ordering; tag geometry snapshots with an epoch, then request full damage. TUIs must eventually receive SIGWINCH and agree with nora's grid.

Font/DPI changes create a new font epoch, re-evaluate geometry, rebuild needed caches and invalidate all panes. Old worker results carry an epoch and are discarded if stale. Do not replace a working font until the new choice is validated.

Session lifecycle: `Starting → Running → Exiting → Exited → Closed`; failure transitions include a user-visible reason. Drain finite final output after child exit, reap the owned child once, cancel session work, remove pending events by generation and release resources. Do not wait indefinitely for descendants keeping the slave PTY open. Close the PTY and use the engine's documented lifecycle; any explicit signal targets only the owned child/session, never an unrelated process group. Document finite shutdown/drain timeouts and `--hold` behavior.

## Child-exit lifecycle (N12a)

`PtySession` carries the pinned lifecycle chain `Starting → Running → Exiting → Exited → Closed` (`Lifecycle`, `src/session/pty.rs`): the main thread sets `Running` at spawn (after the loop handle is assigned — the loop thread never sets `Running`) and folds `Exiting` on the first child-exit fold; the session loop thread — a wrapper around the pinned PTY reader — folds `Exiting` on its return path (every exit path of the pinned loop funnels through it, covering a loop death without a child-exit event) and signals its finish; `close()` (main thread only) folds `Exited` after the bounded wait and `Closed` after joining the loop thread. `wait_drain(timeout)` and `DRAIN_TIMEOUT` (5 s) bound every join/close: the pinned `drain_on_exit` read has no in-crate timeout, so finiteness is nora-side. The window pump's terminated arm drains in 10 ms ticks (probe + non-noop snapshot only — no input, no resize) until the loop thread finished, so the final frame renders before `ChildExit`; the stop arm calls `close()` before break — the pinned loop panics in `PeekableReceiver::recv` when its sender is dropped without `Msg::Shutdown` (alacritty_terminal 0.26.0 event_loop.rs:483-488).

## `--hold` (N12b)

`--hold` is a pure UI deferral; it never alters exit-code semantics. Headless: the flag is ignored entirely — no held pane, status propagation unchanged (`run_session` is untouched). Windowed: the `ChildExit` fold captures the status and, when `hold` is set, skips only `event_loop.exit()`; the window keeps its final frame (rendering is pulse-gated — after the pump exits, no `DirtyPulse` arrives, so nothing re-renders). The visible exit status is the NORA_DEBUG `debug: child exit status` line plus the pump's `title:` eprint; there is no dynamic window title in the tree. Unhold/release goes through the existing `CloseRequested` / `Destroyed` / `Quit` arms (no new teardown path), and the exit code is the child status via the existing `child_exit` capture in `app::run_windowed`. Caveat: a window resize while held calls `graphics.set_size` with no follow-up frame (the pump is gone), so the held surface may blank until unhold/close — a held surface is a final frame, not a live session.

## Configuration and logging

Parse TOML into a validated immutable configuration. Reload builds a change plan: colors/keys immediate; font/geometry staged; launch settings for new sessions only. A malformed reload keeps the old config. Store only local diagnostic metadata; raw key/output logs require deliberate development opt-in and must not be committed.

## Graphics filter (N32a)

A bounded graphics (kitty-protocol) filter, `GraphicsFilter` in `src/graphics_filter.rs`, extracts kitty-graphics APCs (`ESC _ ... ST`) in parallel with the vte parser. It is a byte-level state machine (`Pass` → `SawEsc` → `Apc` → `ApcEsc`) that sees the same bytes as the parser, accumulates APC payloads, parses them into `GraphicsEvent`s (command char + parsed `key=value` control keys + base64-decoded data, per the real kitty protocol in ADR-011), and forwards ALL bytes to the parser (no transformation). The vte parser drops APC bytes correctly, so forwarding all bytes keeps the parser in sync and terminal byte semantics are preserved.

The filter is plugged into `Engine::feed` (headless path): it runs before `processor.advance`, pushing extracted events into a byte-bounded `GraphicsQueue` (1 MiB cap, matching the recorder cap; evicts the oldest event over cap). The render tick drains the queue via `Engine::drain_graphics` (N33). The PTY path (N32b) uses the same filter via a `Filter` trait hook in the vendored `EventLoop::pty_read`, with a shared `Arc<Mutex<GraphicsQueue>>` handle polled by the render tick.

The filter is bounded: the internal accumulation buffer grows only while an APC is in progress (terminated by ST/CAN/SUB), and the queue is byte-bounded. No unbounded growth.

## Shell integration filter (N35)

A shell-integration filter, `PromptMarkerFilter` in `src/prompt_marker.rs`, hooks OSC sequences in the output stream before the parser, following the graphics-filter pattern. It is a byte-level state machine (`Pass` → `SawEsc` → `Osc` → `OscEsc`) handling both ST forms (`ESC \` and `0x9C`) and BEL termination, with CAN/ABORT recovery. Well-formed frames are suppressed from the forwarded stream; everything else is forwarded unchanged. It handles three OSC types in one pass and emits `ShellEvent`s:

- **OSC 133** (`133;A`/`B`/`C`/`D`) → `Marker(PromptMarkerEvent)`: prompt/command/output markers.
- **OSC 7** → `Cwd(String)`: the child's working directory (`file://` or bare absolute path, percent-decoded).
- **OSC 8** → `Hyperlink(HyperlinkAtLine)`: URI + start column (the last `;`-separated field is the URI).

Markers are grid no-ops, so the filter also reports each marker's position in the forwarded stream and the consumer processes forwarded bytes in **segments** (split at marker positions), reading the grid cursor after each segment — that line is the marker's line (N35b). To keep stored lines stable across scrolls and vertical resizes, each record captures `(cursor line, grid.total_scrolled(), grid.total_pulled(), cursor col)` at detection time; the current line is `recorded_line − (now_scrolled − scrolled) + (now_pulled − pulled)` (the two monotonic counters were added to the vendored `Grid` in N35b).

Like the graphics filter it is wired into both paths: headless `Engine::feed` (segment path) and PTY `PtyFilter` (the vendored `EventLoop` runs the filter before the parser and invokes `Filter::on_marker` with the per-segment cursor and counters). Both paths push into shared handles: `Arc<Mutex<MarkerQueue>>` (drained via `Engine::drain_markers`), `Arc<Mutex<Option<String>>>` for cwd (latest wins; `Engine::cwd`), and a byte-bounded `Arc<Mutex<HyperlinkQueue>>` (`Engine::drain_hyperlinks`). Engine-level features built on these: `jump_to_previous_prompt` (N35b), `select_previous_command` (N35c), `activate_hyperlink` (N35d — direct opener argv, allowed schemes, consuming). Emission is opt-in via the shell script `shell/nora-bash.sh` (PROMPT_COMMAND + DEBUG trap); with no markers present the filter is a no-op. UI binding (keybindings/click handlers) is deferred: the features are exposed at the `Engine` API level and tested there.

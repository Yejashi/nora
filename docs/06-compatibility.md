# Terminal and input compatibility

The engine is responsible for parsing and state; nora is responsible for connecting those semantics to keys, PTY replies, rendering and desktop policy. All entries below are **required tests**, not claims that the embedding already works. Consult [S18] for xterm control definitions and [S23] for the kitty keyboard protocol.

## Compatibility matrix to maintain

Create `docs/status/COMPATIBILITY.md` during N04. For every row record: status (`untested`, `pass`, `limited`, `unsupported`), pinned engine/version, fixture/test, UI integration, and known deviations. Update it whenever behavior changes.

| Area | v0.1 expectation | Proof |
| --- | --- | --- |
| Text/UTF-8 | Chunk-invariant parsing; documented invalid-input behavior | Whole/split/random-chunk replay |
| Cursor/erase/style | Common cursor motion, erase and SGR styles/colors | State fixtures + visual tests |
| Screens/scrolling | Primary/alternate screens, margins, wrap, viewport/history | Engine integration and TUI runs |
| Queries | Supported cursor/device/color/size replies routed to child | PTY tests with exact bytes/timeouts |
| Legacy keyboard | Text, controls, navigation, keypad/application modes | Mode-dependent input tables |
| Kitty keyboard | Negotiated behavior and supported flags; accurate responses | Key press/repeat/release fixtures |
| Paste | Bracketed and ordinary paste | Exact payload tests, slow child |
| Mouse/focus | Selected modes, coordinates and focus reports | Input fixtures and real TUI |
| Synchronized output | Verified engine behavior, finite recovery timeout | Start/end/missing-end tests |
| Clipboard | User copy/paste; OSC policy disabled by default | Native Wayland/X11 tests |
| Hyperlinks | Preserve metadata; explicit user activation | URL validation + selection tests |
| IME | Preedit UI and one commit; proper candidate area | Native tests and event replay |
| Graphics | Explicitly unsupported in v0.1 | No positive support claim; bounded ignore/recovery |

**Engine-level evidence so far (headless, N03).** The following rows have passing fixtures in `tests/engine_adapter.rs`: Text/UTF-8 — whole/split (every byte boundary) and seeded random-chunk replay; Cursor/erase/style — overwrite/CUP, pending wrap, SGR flags+colors until reset; Screens/scrolling — ?1049 alternate-screen isolation and restore; Queries — DSR and OSC-title replies captured in order via `Recorder`. These prove engine semantics only; PTY-level proof (exact reply bytes/timeouts, TUI runs) and all other rows remain open until N04+. Full per-row status tracking starts at N04 in `docs/status/COMPATIBILITY.md`.

When an upstream capability fails, isolate an adapter bug from engine limitations. Fix integration first. Engine patches require a pinned diff, tests and reconciliation notes.

## Streaming and replies

Never convert PTY reads to UTF-8 strings before feeding the engine. Byte reads may split UTF-8 and escape strings. Keep decoder/parser state across reads. Synthetic fixtures include every split point of short UTF-8/CSI/OSC sequences, followed by ordinary text to verify recovery.

Route engine PTY-write events to the session's ordered writer. For color/size queries, capture the correct state and invoke the pinned engine's provided formatting callback outside the terminal lock. Avoid two writers racing on the same PTY. Policy requests must not synchronously call clipboard/window APIs from parser callbacks [S10].

For synchronized updates, first inspect the pinned parser/worker support. If it handles the mode and timeout, preserve it through the session adapter. Do not add a second CSI scanner. If needed, extend a defined engine hook or a minimal patch. Missing-end and resize/shutdown behavior must be tested. Proposed timeout: 200 ms, to be calibrated and recorded; it bounds withheld presentation, not data parsing.

## Keyboard pipeline

Use this order: IME/composition handling → enabled local shortcuts → normalized key event → mode-aware encoder → accepted ordered PTY bytes. Text events represent the active layout; physical key IDs are for explicit physical bindings. Do not produce the same text from both key events and IME commits.

Legacy fixture examples to verify in the selected mode: Enter sends CR; Ctrl+C sends byte 0x03 and the child's terminal settings determine interrupt handling; application arrows differ from ordinary arrows. Define Backspace consistently with terminfo, not a guessed universal byte. Non-US layouts, AltGr, dead keys and keypad need tests.

### Input coverage: navigation, F-keys and keypad (N10a/N10b)

Tested by `tests/navigation_input.rs` and `tests/keypad_input.rs` (pure table rows, both `TermMode` values constructed directly, plus PTY legs against `nora_child check`; headless, deadline-bounded).

| Area | v0.1 behavior | Modes | Status / proof |
| --- | --- | --- | --- |
| Arrows + Home/End | `ESC [ A`/`B`/`C`/`D`/`H`/`F` normal, `ESC O <letter>` app | DECCKM-conditional on `APP_CURSOR` | pass — table rows + normal/app PTY legs; pinned DECCKM `\e[?1h`/`\e[?1l` (alacritty_terminal 0.26.0 term/mod.rs:2001-2005) |
| Insert/Delete/PgUp/PgDn | `ESC [ 2~`/`3~`/`5~`/`6~` | mode-invariant | pass — table rows + PTY leg |
| F1–F4 | `ESC O P`/`Q`/`R`/`S` | mode-invariant | pass — table rows both modes + PTY leg; SS3 forms per xterm ctlseqs [S18] |
| F5–F12 | `ESC [ 15~`/`17~`/`18~`/`19~`/`20~`/`21~`/`23~`/`24~` | mode-invariant | pass — table rows both modes |
| F13+ | no bytes (`None`) | — | **deferred** — PC keyboards need Ctrl+Fn to reach F13+; no v0.1 encoding |
| Numpad digits 0–9 | plain `0`–`9` normal; SS3 `p`–`y` app | app = `APP_CURSOR || APP_KEYPAD` | pass — table rows + app PTY leg |
| Numpad `.` / Enter | `.` / `0x0d` normal; SS3 `n` / `M` app | app = `APP_CURSOR || APP_KEYPAD` | pass — table rows |
| Numpad operators `+`/`-`/`*`/`/`, `,`, `=` | plain byte normal; SS3 `k`/`m`/`j`/`l`/`l`/`X` app | app = `APP_CURSOR || APP_KEYPAD` | pass — table rows |
| Numpad Home/End (physicals `Numpad7`/`Numpad1`) | `None` normal; SS3 `H`/`F` app | app-only | pass — table rows both modes |
| Backspace | `0x7f` (kernel `erase`) | mode-invariant | pass — table rows |
| Numpad under `Key::Unidentified` | resolved by physical `Numpad*` scancode | as above | pass — table rows; winit 0.30.13 folds numpad logicals to `Character`/`Enter`/`Home`/`End` (registry `winit-0.30.13/src/keyboard.rs`) |
| F-key modifier encoding | Shift/Ctrl send the plain row (no `1;N~`); Alt prefixes `ESC` | — | pass — table rows; **deferred**: DECFNK `1;N~` modifier encoding is N21-era (kitty/modern protocol work) |

Mode-drive citations: DECCKM (`APP_CURSOR`) is `CSI ? 1 h/l` (pinned alacritty_terminal 0.26.0 term/mod.rs:2001-2005, vte-0.15.0 private-mode 1 = `CursorKeys`, vte-0.15.0/src/ansi.rs:900); keypad application mode (`APP_KEYPAD`) is the DECKPAM escape pair `ESC =`/`ESC >` (pinned vte-0.15.0 escape-final dispatch, vte-0.15.0/src/ansi.rs:1816-1817; recorded by the pinned alacritty term at term/mod.rs:2180-2188). Note: `CSI ? 6 h` is **not** the keypad on this stack — the pinned `PrivateMode::new` maps 6 to `Origin` (vte-0.15.0/src/ansi.rs:902). SS3 letter assignments pinned from the xterm ctlseqs numeric and editing keypads [S18].

Modern kitty keyboard behavior is negotiated by the application, with a legacy default. Implement only the flags actually supported end to end; verify stack/query/pop handling, modifiers, repeat/release and associated text. The engine exposes flags, but frontend encoding remains separate [S11, S23]. The Alacritty frontend keyboard implementation is a useful Apache-licensed reference [S30, S35]; record the exact revision/notice if adapting code.

## Paste

Read clipboard text through the platform adapter. Validate maximum accepted size before any bytes are written. Normalize newlines by a documented policy (CRLF/LF to CR for terminal paste), and distinguish ordinary paste from bracketed paste. Prevent injected bracket terminators according to a tested sanitization policy; never let pasted escape delimiters prematurely terminate the transaction. Explain any removed control characters in the user docs.

Wrap bracketed data only when the current application mode requests it. A long paste is one ordered transaction from the user's perspective; accepted keyboard input waits behind it. If canceled after begin, enqueue the end marker before normal input resumes. For v0.1, use bounded bracketed segments: a pending protocol reply may close the current segment at a payload-chunk boundary, emit the reply in query order, then resume the remaining payload in a new segment if the application still requests bracketed paste. This avoids injecting reply bytes inside a paste payload and allows a child to observe multiple paste events. Document this segmentation and test it; preserve the order of the pasted text. Ordinary paste has no delimiters, so retain its accepted byte order and document that application-initiated replies share the same input stream. Neither mode guarantees progress when the child entirely stops reading. Shutdown still has an independent control path. Do not reorder raw keyboard input to improve an artificial latency number.

### Paste policy (N17a)

Pure policy in `src/input/paste.rs` (`validate_paste`, `PasteStream`), headless fixtures in `tests/paste_input.rs`. The pump wiring (N17b) validates clipboard text, then sends the validated bytes through the bounded writer in order.

| Area | v0.1 behavior | Status / proof |
| --- | --- | --- |
| Bracketed paste | Supported: when bracketed paste mode is active the payload is wrapped in `CSI 200 ~ ... CSI 201 ~` (6 bytes per marker); ordinary paste has no delimiters | pass — `tests/paste_input.rs` |
| Maximum paste size | 1 MiB (`MAX_PASTE_SIZE`), checked against the final bytes including markers; strictly larger is rejected as `TooLarge`, exactly 1 MiB is accepted | pass — boundary rows in `tests/paste_input.rs` |
| Newline normalization | `\r\n` and standalone `\r` → `\n`, before bracketing | pass — `tests/paste_input.rs` |
| Bounded streaming | `PasteStream` delivers the validated bytes as ordered chunks of at most `max_size`; `cancel` drops the remainder, and the pump still sends the bracket terminator before normal input resumes (N17b) | pass — `tests/paste_input.rs` |

### Paste pump wiring (N17b)

`PumpMsg::Paste { text, bracketed }` validates the clipboard text with
`validate_paste` and, on success, appends the validated bytes to the pump's
`pending_input` (after any queued keystrokes) and records the in-flight paste
as a half-open byte range `[start, end)`; `PumpMsg::CancelPaste` drops the
not-yet-fed paste bytes (the block range) without touching keystrokes. Paste
bytes stream through the same bounded writer as keystrokes (N09b), in order
and capped by the writer's app cap; the in-flight block is advanced as bytes
drain (`feed_pending` → `advance_paste_block`), so a cancel always drops
exactly the paste's remaining bytes. `bracketed` is the engine's live
`TermMode::BRACKETED_PASTE` (set by `CSI ? 2004 h`, cleared by `CSI ? 2004
l`), read from the app's published `self.modes` (N10b). A new paste replaces
any previously tracked block (only the most recent is cancellable); a
`TooLarge` paste queues nothing and leaves state unchanged. Headless fixtures
in `tests/paste_pump.rs` (pure state helpers plus one real-PTY round-trip
through the bounded writer).

| Area | v0.1 behavior | Status / proof |
| --- | --- | --- |
| Paste queued behind keystrokes | `PumpMsg::Paste` appends validated bytes after queued keystrokes; the bounded writer feeds them in order | pass — `tests/paste_pump.rs` |
| Cancellation | `PumpMsg::CancelPaste` drops only the in-flight paste bytes; keystrokes unaffected | pass — `tests/paste_pump.rs` |
| Bracketed flag source | `bracketed` is the engine's live `TermMode::BRACKETED_PASTE`, read from the app's published modes | checked — `src/app/mod.rs` |
| Ordered round-trip | Paste bytes reach a real PTY child in order through the bounded writer | pass — `tests/paste_pump.rs` |

## Mouse, selection and focus

Encode mouse coordinates using current pane/grid geometry and requested mode, with correct release/motion/modifier semantics. Local selection has an explicit override modifier (proposed Shift) when the application requests mouse reporting. Scroll goes to history in normal local mode and to the application when its modes require it. Focus notifications apply to the active session consistently when pane/OS focus changes.

### Mouse reporting (N16a)

Pure encoding in `src/input/mouse.rs` (`encode_mouse_event`), headless fixtures in `tests/mouse_input.rs`. All reporting modes emit the SGR-style format `CSI < b ; x y M` (press/motion/wheel) / `CSI < b ; x y m` (release); button codes are 0/1/2 left/middle/right, motion 32 + held button (35 with no button held), wheel 64/65/66/67 up/down/left/right; modifiers add shift +4, ctrl +8, alt +64.

| Area | v0.1 behavior | Modes | Status / proof |
| --- | --- | --- | --- |
| Button press/release | `CSI < b ; x y M` / `CSI < b ; x y m`, b = 0/1/2 | X10, Normal, CellMotion, AllMotion | pass — table rows in `tests/mouse_input.rs` |
| Motion | b = 32 + held button (35 with no button) | X10: none; Normal: button held only; CellMotion/AllMotion: always | pass — table rows |
| Wheel | b = 64/65/66/67 (up/down/left/right) | every reporting mode | pass — table rows |
| Modifiers | shift +4, ctrl +8, alt +64 on the button code | all modes | pass — table rows |
| Coordinates | `col+1`/`row+1` (1-based); on-grid-edge clamps to the last cell; beyond the grid edge or coordinate >= 256 returns no report | all modes | pass — table rows |
| Mode derivation | engine `TermMode`: `MOUSE_DRAG` -> AllMotion, `MOUSE_MOTION` -> Normal, `MOUSE_REPORT_CLICK` -> X10, none -> None (most specific flag wins) | — | pass — table rows |
| SGR format (1006) / UTF-8 (1005) | same bytes as the rows above (N16a format is SGR-style for every mode); the N16b wiring confirmed no byte change | — | pass — wiring in `src/app/mod.rs` |
| Focus reporting | `CSI I` focus in / `CSI O` focus out; no report when `FOCUS_IN_OUT` is off | — | pass — table rows |
| Window wiring (winit events, Shift selection override, wheel routing) | `CursorMoved`/`MouseInput`/`MouseWheel`/`Focused` → `encode_mouse_event`/`encode_focus_event` against the engine's live `TermMode` and the current window grid, bytes via `PumpMsg::Input`; Shift held = local selection only; wheel to the terminal when a reporting mode is active, else local scrollback | all modes | pass — wiring in `src/app/mod.rs`, headless rows in `tests/mouse_input.rs` (N16b) |

### Window wiring (N16b)

`src/app/mod.rs` connects the winit events to the N16a encoders: `CursorMoved` reports motion (the engine's mode decides whether anything is encoded), `MouseInput` reports presses/releases at the last tracked cursor position (winit carries no position on the event) and tracks the pressed buttons in press order, `MouseWheel` reports one event per nonzero delta axis at the last tracked cursor position, and `Focused` reports `CSI I`/`CSI O` when `FOCUS_IN_OUT` is set. All coordinates convert from physical pixels to grid cells through the window scale factor and the pinned mono cell metrics; positions beyond the grid edge or ≥ 256 encode to no report (N16a). While Shift is held, no mouse event is reported — the event is left to local selection — and the wheel scrolls the local scrollback instead of the terminal only when no reporting mode is active. The pure decisions (coordinate conversion, button tracking, modifier mapping, the report-vs-local gate, wheel direction) are headless-tested in `tests/mouse_input.rs`; the bytes themselves are the N16a fixtures.

Selection uses engine-supported coordinates and wrapped-line information. Copy joins soft wraps without injecting newlines, preserves real newlines, and handles wide spacers/combining marks. During history trimming or reflow, use the engine's selection remapping or invalidate visibly; do not retain stale raw row indices.

## Scrollback search (N20a)

Literal search over an owned scrollback snapshot (`nora::search`, headless fixtures in `tests/search.rs`). Search is core terminal-state logic: the engine and session are untouched, and the search never reaches into unbounded history.

| Area | v0.1 behavior | Status / proof |
| --- | --- | --- |
| Matching | literal (no regex, no case folding); case-sensitive, per code point | pass — `tests/search.rs` |
| Scope | bounded to the provided snapshot (`&[Vec<char>]`, row 0 = oldest) | pass — `tests/search.rs` |
| Wrapped lines | each physical row is searched separately; a query spanning a wrap boundary matches nothing | pass — `tests/search.rs` |
| Navigation | next/previous over the match list, wrap-around at both ends; no-op when there are no matches | pass — `tests/search.rs` |
| Stale results | every search is tagged with the query's generation; a result that completes after a newer query is `Cancelled` and must be dropped without touching the state | pass — `tests/search.rs` |
| Empty query | returns no matches (no "match every cell" behavior) | pass — `tests/search.rs` |

Highlighting, the search bar and background-job wiring are N20b.

## Native clipboard and IME

Wayland clipboard ownership is a platform integration issue. Prefer a winit-compatible native Wayland helper such as `smithay-clipboard` after verifying the pinned handle/API; use a tested X11 backend for X11 [S25, S26]. Do not quietly start an X11 clipboard path in a native Wayland session. Test ownership lifetime, delayed requests and selection transfer after focus changes. PRIMARY selection is optional and documented independently from ordinary copy/paste.

Keep clipboard work asynchronous and bounded. OSC 52 reads and writes are disabled by default; opt-in policies must be separate, with payload limits and tests. A shell output string cannot open a URL, read files, or spawn a program without a defined user action.

IME preedit is UI-only, not PTY content. Commit text once, move the candidate rectangle with cursor/pane/DPI, and cancel/transfer composition deliberately on focus change [S33]. Platform availability must be tested rather than inferred from replayed events.

## TERM and terminfo

Start the development slice with `TERM=xterm-256color` only after testing its relevant advertised behavior. Document it as an initial compatibility choice, not a proof of complete xterm emulation. `COLORTERM=truecolor` can advertise validated truecolor. Never set `TERM=xterm-kitty` for branding.

Before v0.1, decide whether to ship a dedicated `nora` terminfo entry or keep the verified fallback. A dedicated entry includes only validated capabilities, compiles with `tic`, and works with `infocmp` and curses applications [S27]. For SSH, document how to install it on remote hosts or explicitly use a fallback; do not set an unknown remote TERM by default. Use runtime protocol negotiation for modern features rather than claiming they follow from the TERM string.

## Emoji and complex scripts (N13)

Emoji are rendered via the N13a font-fallback path: `first_supporting` (`src/render/fallback.rs`) selects the first stack font that supports the code point, and cosmic-text's internal fallback resolves the remaining faces. The pinned stack is NotoSansMono → NotoSansCJK → NotoSerifEthiopic (`src/app/glyphs.rs`); NotoSansCJK covers some emoji, but no dedicated emoji font (e.g., Noto Color Emoji) is loaded.

Documented limitations (no width policy invented):

- **Monochrome only** — no color-emoji format/blending path; emoji render as the fallback face's monochrome glyphs.
- **No complex grapheme clustering** — each cell is one grapheme; ZWJ sequences and other multi-cell emoji are shaped per cell, so joined sequences may render as separate components.
- **Left-to-right only** — no RTL shaping; RTL scripts render in logical order without direction-aware shaping.

## Configuration (N18a/N18b)

Validated TOML configuration lives in `src/config.rs` (`nora::config`), headless fixtures in `tests/config.rs`.

**Discovery order** (first existing file wins; otherwise built-in defaults with no source path):

1. An explicit path (loaded directly; a missing file is an error).
2. `$XDG_CONFIG_HOME/nora/config.toml` (when the variable is set).
3. `$HOME/.config/nora/config.toml`.
4. Built-in defaults.

**Available settings** (all sections optional; omitted sections/fields take defaults):

| Setting | Type | Default | Constraint |
| --- | --- | --- | --- |
| `terminal.history` | integer | `1000` | `0`..=`1000000` |
| `terminal.mouse` | boolean | `false` | — |
| `font.family` | string | `"monospace"` | — |
| `font.size` | float | `12.0` | `> 0` and `<= 1000` |
| `theme.background` | string | `"#000000"` | `#RRGGBB` |
| `theme.foreground` | string | `"#ffffff"` | `#RRGGBB` |
| `theme.cursor` | string | `"#ffffff"` | `#RRGGBB` |
| `theme.colors` | array of strings | `[]` | each `#RRGGBB` |
| `keys.bindings` | table (string → string) | `{}` | — |

**Validation.** Unknown fields are rejected at parse time (`deny_unknown_fields` on every section). Numeric ranges and `#RRGGBB` colors are checked after parsing and reported as `ConfigError::Validation` with one line per problem; I/O and TOML syntax failures are `ConfigError::Io` / `ConfigError::Parse`. Only settings that are actually implemented are accepted — the schema is intentionally minimal and grows with the features that consume it.

### CLI integration (N18b)

`src/main.rs` loads the config after flag parsing and before any display/GPU work, and passes the loaded `NoraConfig` to both the windowed (`app::run_windowed`) and headless (`run_session`) launch paths.

| Flag | Behavior |
| --- | --- |
| `--config <path>` | Use an explicit config file; a missing, unreadable or invalid file exits 1 |
| `--check-config` | Validate the loaded config and exit — headless, no display or GPU: `config OK: <path or defaults>` on stdout with exit 0, one validation error per line on stderr with exit 1 |
| `--reload` | Re-read the config from disk before launch and swap in the new state transactionally (N19a); a failed reload keeps the last valid config |
| `--working-directory <path>` | Working directory for the child process (CLI value; the config schema has no `working_directory` field yet) |

**Precedence:** CLI flags > config file > built-in defaults. `--working-directory` is the first CLI override (it lands in the launch spec's `cwd`, which outranks any future config value). Config values are accepted at launch and logged (`debug: config loaded: …`); scrollback history, font size and theme colors are consumed as the app grows support for them.

Headless CLI fixtures in `tests/cli.rs`: `check_config_valid`, `check_config_invalid`, `check_config_defaults`, `config_flag`, `reload_flag`.

### Reload (N19a)

`src/config.rs` (`nora::config`) adds a transactional reload on top of the N18 loading:

- **`ConfigState`** wraps the active [`NoraConfig`] with the source `path` and a monotonically increasing `epoch` (0 at first load, incremented on each successful reload). `ConfigState::reload()` re-reads from the same source the current state came from (the explicit path, otherwise XDG discovery) and returns a *new* state; the caller swaps it in only on success.
- **Transactional.** A reload that fails I/O, parsing or validation returns the error and leaves the current state untouched — the last valid config is kept. There is no partial application.
- **Change classification.** `classify_change(old, new)` diffs two configs and labels each changed field:
  - `Immediate` — colors (`theme.*`) and key bindings (`keys.bindings`); apply without a restart.
  - `Staged` — font (`font.family`, `font.size`); apply on the next frame.
  - `NewSession` — launch settings (`terminal.history`); apply to new sessions only.

**Trigger.** `nora --reload` re-reads the config before launch and logs each classified change plus `config reloaded (epoch N)` on stderr; a failed reload logs the error and `keeping last valid config` and continues with the last valid state. `--reload` is headless-safe (it runs before any display/GPU work). A runtime keybinding is a later slice; the config schema and the headless session pump are unchanged.

Headless fixtures: `tests/config.rs` (`config_state_new`, `config_state_reload_success`, `config_state_reload_failure`, `classify_change_immediate`, `classify_change_staged`, `classify_change_new_session`, `classify_change_no_change`) and `tests/cli.rs` (`reload_flag`).

### Zoom and geometry (N19b)

The window carries a zoom level (1.0 = 100%), stored on the app and scaled into the shaping font size at draw time (`glyphs::zoomed_font_size`); cell metrics and grid re-derivation are staged for a later slice.

| Shortcut | Behavior |
| --- | --- |
| `Ctrl+=` / `Ctrl++` | Zoom in one step (10%), capped at 4.0x |
| `Ctrl+-` | Zoom out one step (10%), floored at 0.5x |
| `Ctrl+0` | Reset zoom to 1.0x |

- **Range:** 0.5x to 4.0x in 10% steps, starting at 1.0x. Steps are rounded to one decimal place so repeated presses land on exact tenths.
- **Keying:** the shortcuts key on the physical key plus Ctrl (layout-independent), are checked before PTY encoding, and return early — they never send bytes to the child.
- **Staged font/geometry changes:** a reload that changes `font.family` or `font.size` is classified `Staged` (N19a) and logged as `applying staged font change: <field>`; the change takes effect on the next frame. Font re-initialization is a later slice, so the staged change is logged, not yet applied to the loaded font.

Headless fixtures: `tests/config.rs` (`zoom_initial`, `zoom_in`, `zoom_out`, `zoom_reset`, `zoom_max`, `zoom_min`) cover the pure zoom arithmetic in `nora::config`.

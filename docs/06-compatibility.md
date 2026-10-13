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

## Mouse, selection and focus

Encode mouse coordinates using current pane/grid geometry and requested mode, with correct release/motion/modifier semantics. Local selection has an explicit override modifier (proposed Shift) when the application requests mouse reporting. Scroll goes to history in normal local mode and to the application when its modes require it. Focus notifications apply to the active session consistently when pane/OS focus changes.

Selection uses engine-supported coordinates and wrapped-line information. Copy joins soft wraps without injecting newlines, preserves real newlines, and handles wide spacers/combining marks. During history trimming or reflow, use the engine's selection remapping or invalidate visibly; do not retain stale raw row indices.

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

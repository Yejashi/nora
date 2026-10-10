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

When an upstream capability fails, isolate an adapter bug from engine limitations. Fix integration first. Engine patches require a pinned diff, tests and reconciliation notes.

## Streaming and replies

Never convert PTY reads to UTF-8 strings before feeding the engine. Byte reads may split UTF-8 and escape strings. Keep decoder/parser state across reads. Synthetic fixtures include every split point of short UTF-8/CSI/OSC sequences, followed by ordinary text to verify recovery.

Route engine PTY-write events to the session's ordered writer. For color/size queries, capture the correct state and invoke the pinned engine's provided formatting callback outside the terminal lock. Avoid two writers racing on the same PTY. Policy requests must not synchronously call clipboard/window APIs from parser callbacks [S10].

For synchronized updates, first inspect the pinned parser/worker support. If it handles the mode and timeout, preserve it through the session adapter. Do not add a second CSI scanner. If needed, extend a defined engine hook or a minimal patch. Missing-end and resize/shutdown behavior must be tested. Proposed timeout: 200 ms, to be calibrated and recorded; it bounds withheld presentation, not data parsing.

## Keyboard pipeline

Use this order: IME/composition handling → enabled local shortcuts → normalized key event → mode-aware encoder → accepted ordered PTY bytes. Text events represent the active layout; physical key IDs are for explicit physical bindings. Do not produce the same text from both key events and IME commits.

Legacy fixture examples to verify in the selected mode: Enter sends CR; Ctrl+C sends byte 0x03 and the child's terminal settings determine interrupt handling; application arrows differ from ordinary arrows. Define Backspace consistently with terminfo, not a guessed universal byte. Non-US layouts, AltGr, dead keys and keypad need tests.

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

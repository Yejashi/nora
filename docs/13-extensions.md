# After v0.1: kitty-style extensions

These are planned phases, not shipped features. Each phase must still be split into small changes, checked, committed and pushed. The default engine must pass an extension-hook experiment before implementing graphics.

## Graphics implementation sequence

1. **Hook feasibility (N32):** locate a documented callback for APC graphics payloads and ordered replies. Verify streaming chunks and bounded accumulation without corrupting normal VT text. If unavailable, propose a minimal engine patch/upstream contribution or engine migration ADR. Do not maintain a second byte parser alongside the VT parser.
2. **Bounded inline upload (N33a):** implement a clearly documented subset of the specification [S24], with one explicit pixel format, strict base64/size validation, IDs and expected replies. Advertise only what the implementation supports.
3. **Placement (N33b):** draw an uploaded image with clipping and a defined relationship to cells/viewport; test resize, scroll, delete and alternate screen. Maintain image IDs separately from placements.
4. **Additional formats/streaming (N33c onward):** chunk reassembly, supported PNG decode, multiple placements/deletion, persistence policy and query semantics. Every capability has isolated fixtures and positive/negative tests.
5. **Advanced transports/features (N34):** consider shared memory, animation, Unicode placeholders and additional protocol features only after correct bounded basics. File/shared-memory transport is off until its scope and validation are explicitly designed; shell-controlled paths are not trusted file-open instructions.

Proposed graphics budget: 128 MiB CPU decoded storage, 128 MiB GPU image storage, 32 MiB per decoded image, maximum dimension 8192 and 1024 image IDs, additionally constrained by device capabilities. Count actual decoded pixel allocation with checked arithmetic before decode, including decompression expansion. These values need measurement and may be reduced. Budget overflow returns a protocol-appropriate error without corrupting terminal state; no silent unbounded decompression.

The graphics protocol is sizable. A narrow proof of concept is not “full kitty image support.” Maintain a capabilities table and test a neutral client in Rust. A compatible third-party image tool is an integration test, not the sole correctness oracle.

## Shell integration and hyperlinks (N35)

Add optional, small shell scripts for supported shells to emit documented prompt/command markers. No Python helper is needed. Hook markers through the engine, not a second output regex scanner. Preserve a no-integration path and never source code into user shells automatically.

Build prompt navigation, previous command selection and optional per-pane cwd tracking incrementally. Shell-supplied cwd is metadata, not authorization to access paths. Avoid parsing a printed prompt's appearance to infer semantic boundaries. Hyperlink activation requires a user gesture, allowed schemes and direct process argv or a platform opener; never interpolate the URL into a shell command.

## Later platform features (N36)

Multiple OS windows reuse the session/workspace boundaries but require explicit ownership transfer, clipboard/IME focus and shutdown tests. A single-instance daemon or remote-control socket is separate work, not a prerequisite for new windows.

Before remote control, define socket ownership/permissions, authentication, bounded messages and commands. Do not expose a TCP listener by default. Configurable sessions/layout persistence store commands/cwd only under an explicit policy; do not persist scrollback or secrets silently.

Accessibility needs a dedicated investigation into supported Linux accessibility APIs and terminal-text exposure. macOS/Windows ports require native PTY/window/font/clipboard validation; compiling common Rust modules is not a completed port. Keep these goals off the v0.1 critical path but document their present limitations.

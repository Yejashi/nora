# Product contract

`nora` is a keyboard-friendly Linux desktop terminal for interactive shells, SSH and TUIs. The requirements below are project decisions. Research evidence is in [01-research.md](01-research.md).

## Platform and identity

- Primary target: Linux x86_64, native Wayland and X11; test AMD and NVIDIA/Mesa environments when available.
- Other CPU architectures are future targets, not presumed supported merely because Rust cross-compiles.
- Project, executable, CLI, config directory and app ID contain `nora`. Choose a unique reverse-domain desktop app ID during packaging; do not claim a domain the owner does not control.
- Application/build/dev tools have no project-owned Python dependency. Native font and graphics libraries are allowed and documented.
- Opaque window and one process per launched application instance are the initial defaults. No background server is needed for v0.1.
- Provisional project license: Apache-2.0, compatible with the selected engine's published license. Maintain component-specific license notices; this choice is not a blanket determination for every dependency.

## Release tiers

| Tier | Requirements |
| --- | --- |
| Development vertical slice | One window; live PTY shell; ASCII/colors/cursor; resize; reliable child cleanup; basic keyboard input |
| v0.1 daily-use terminal | All validation gates below; scrollback/selection/clipboard/search; configurable fonts/colors/keys; Unicode and fallback within the engine's documented policy; IME; modern keyboard protocol; tabs/splits; compatibility report; packaging; performance report |
| v0.2 kitty-style extensions | Bounded graphics implementation; hyperlink UX; shell prompt markers and command navigation; richer decorations and session layouts as validated individually |
| Later | Multiple OS windows; optional remote control; accessibility integration; macOS/Windows; sixel; plugin API only if a demonstrated user need exists |

v0.1 does not claim full kitty compatibility. A usable single-pane development build is an intermediate checkpoint, not permission to omit tabs/splits or the remaining release gates.

## User-visible defaults

| Behavior | Default |
| --- | --- |
| Shell | User's configured shell with a documented fallback; explicit `-- COMMAND ARG...` runs argv directly |
| Configuration | `$XDG_CONFIG_HOME/nora/config.toml`, fallback `~/.config/nora/config.toml`; watched for live reload |
| Fonts | System monospace at 12 points; explicit fallback chain; diagnose missing fonts |
| Window | 100 columns × 30 rows as an initial preference; actual geometry respects the desktop |
| Scrollback | 10,000 rows, additionally constrained by the implemented memory policy |
| New tab | Ctrl+Shift+T |
| Close pane | Ctrl+Shift+W (closes the tab with its last pane, and the window with its last tab); a confirmation for running child work is not implemented |
| Next/previous tab | Ctrl+PageDown / Ctrl+PageUp (passed to the application when only one tab exists) |
| Split right / below | Ctrl+Shift+Enter / Ctrl+Shift+Backslash |
| Pane focus | Ctrl+Shift+arrow (passed to the application when there is no pane in that direction). Alt+arrow was dropped: shells use it for word and directory movement |
| Copy/paste | Ctrl+Shift+C / Ctrl+Shift+V |
| Search | Ctrl+Shift+F; Escape closes the search UI |
| Zoom | Ctrl+Shift+Plus / Minus; Ctrl+Shift+0 resets |
| Reload config | Ctrl+Shift+F5 (and automatically when the file changes); a failed reload keeps the previous valid configuration |
| Scrollback | Shift+PageUp / Shift+PageDown, Shift+Home / Shift+End (passed to the application in the alternate screen) |
| Clipboard protocol | OSC 52 reads disabled; writes disabled until explicit opt-in |
| Presentation | On-demand, vsync-capable default; optional low-latency mode based on supported surface modes |

Shortcut conflicts are resolved by configurable precedence, not by silently eating input. Disabled local bindings pass through. Local UI bindings apply before PTY encoding; IME composition must retain its own necessary input.

## CLI contract to implement incrementally

`nora --help`, `nora --version`, `nora --config PATH`, `nora --check-config`, `nora --working-directory PATH`, `nora --hold`, and `nora -- COMMAND ARG...`.

`--check-config` works without a display/GPU. Exit nonzero for invalid configuration, include file/field information, and do not start a shell. `--hold` retains an exited pane's output with a visible exit status. The default last-pane child exit closes the instance after draining final PTY output. The CLI's process exit status should reflect an explicit single-command launch; define/document multi-pane exit behavior rather than accidentally changing it.

## v0.1 acceptance

1. Bash/zsh or the user's shell, `less`, `vim`/`nvim`, `htop`, tmux and an SSH session work on native Wayland and X11. Record versions and skipped external prerequisites.
2. Input remains usable while another pane produces a sustained output flood. Child exit, Ctrl+C, resize and closing panes have deterministic lifecycle behavior.
3. UTF-8, basic combining text, CJK wide text, box drawing and fallback render without cursor/cell drift. Unsupported complex graphemes/RTL behavior are explicitly documented, with copy preserving engine-stored text.
4. IME commit/preedit do not duplicate input or change terminal state prematurely. Include non-US keyboard testing.
5. Scrollback, selection, bracketed paste, search, tabs, splits and live config reload have regression coverage.
6. Resource-limit audits cover engine storage and dependency queues; no claim of strict memory bounds is made where implementation cannot enforce them.
7. No continuous redraw loop while idle. Benchmark results distinguish headless processing from presentation and physical latency.
8. The repository builds from a clean checkout using pinned dependencies, includes a desktop entry/terminfo strategy, and contains notices, limitations and installation instructions.
9. Every implemented feature has validation evidence and its own committed and pushed increment. Manual release checks may block a release even when code is committed and pushed.

## Out of scope for v0.1

Custom shaders, translucent backgrounds, embedded AI features, GPU compute parsing, application scripting, an SSH client, a custom shell, a multiplexer protocol, persistent background daemon, automatic telemetry and a settings GUI. Search/config/tab UI can share the renderer but must not force a general UI framework into the core.

Screen-reader integration is an unresolved limitation and blocks claiming accessibility parity. A visually complete terminal is not automatically an accessible application.

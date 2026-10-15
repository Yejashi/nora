# Platforms (M5, 2026-10-15)

## Support levels

A platform counts as supported only when nora builds there, runs there,
and its essential functions have been tested there. The levels below are
exact; nothing here is a forecast.

| Platform | Level | Evidence |
| --- | --- | --- |
| Linux, X11 | **Verified** | CI: full test suite, `--smoke` under Xvfb, 19 end-to-end scenarios in a real window (keys, mouse, PTY, panes, config, vim/tmux/htop) |
| Linux, Wayland | **Verified** (smoke in CI; interactive use by hand) | CI: `--smoke` under headless Weston; manual use on GNOME Wayland (RADV) including the performance baseline |
| macOS (Apple silicon) | **Builds and passes headless and PTY tests in CI**; interactive use **untested** | CI job `cross-platform (macos-latest)`; local type-check for `aarch64-apple-darwin` |
| Windows (x86_64) | **Builds and passes headless tests in CI**; PTY (ConPTY) and interactive use **untested** | CI job `cross-platform (windows-latest)`; local type-check for `x86_64-pc-windows-msvc` |

"Untested" means no one has run the window, typed into a shell, or used
the clipboard on that platform. Treat those builds as previews.

## Platform-specific components

| Component | Linux | macOS | Windows | Where |
| --- | --- | --- | --- | --- |
| Window, input, IME | winit (X11 + Wayland features) | winit (AppKit) | winit (Win32) | `src/app/` |
| GPU | wgpu: Vulkan (GL fallback) | wgpu: Metal | wgpu: DX12 / Vulkan | `src/app/graphics.rs` |
| PTY | alacritty_terminal `tty::unix` | `tty::unix` | `tty::windows` (ConPTY) | `src/session/pty.rs` |
| Child kill escalation (SIGHUP, then SIGKILL to the process group) | yes | yes | not needed: closing the ConPTY ends attached processes | `PtySession::kill_child_group` |
| Default shell | `$SHELL`, else `/bin/sh` | same | `pwsh.exe`, else `powershell.exe`, else `%COMSPEC%` | `default_shell` in `src/main.rs` |
| Font lookup | fontconfig (`fc-match`, `fc-list`), full scan as fallback | full scan (fontdb) | full scan (fontdb) | `src/app/glyphs.rs` |
| Default font chain | Noto Sans Mono, DejaVu Sans Mono, monospace | + SF Mono, Menlo | + Cascadia Mono, Consolas | `Fonts::load` |
| Clipboard | arboard (X11; Wayland via XWayland) | arboard | arboard | `src/clipboard.rs` |
| Terminal identity | `TERM=nora` (terminfo) | same | not used (ConPTY) | `src/main.rs` |
| Install | `scripts/install.sh` (incl. NixOS wrapper) | none yet | none yet | `scripts/` |
| Shell integration | `shell/nora-bash.sh` | same (bash) | none | `shell/` |
| Test helper `nora_child` and PTY tests | yes | yes | Unix-only (compiled as a stub) | `src/bin/nora_child/` |

## What it takes to call macOS and Windows verified

1. Run the interactive checks by hand, with the commands and results
   recorded: launch, type, resize, copy/paste, splits and tabs, vim, and a
   config reload.
2. Windows: run a ConPTY session test (an equivalent of
   `tests/pty_session.rs` on `cmd.exe`/PowerShell) and check that the
   shell exits when its pane closes.
3. Packaging: an `.app` bundle for macOS and a zip or MSI for Windows,
   each with install and uninstall checks.
4. Fonts on macOS and Windows go through a full font scan at startup
   (nora's background loader keeps it off the first frame). Measure it
   there before deciding whether a native lookup (Core Text,
   DirectWrite) is needed.

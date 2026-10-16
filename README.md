# nora

[![CI](https://github.com/Yejashi/nora/actions/workflows/ci.yml/badge.svg)](https://github.com/Yejashi/nora/actions/workflows/ci.yml)

A fast, efficient, maintainable Linux desktop terminal written in Rust,
with native Wayland and X11 support.

## Quick start

```sh
git clone https://github.com/Yejashi/nora.git
cd nora
./scripts/install.sh   # release build + per-user install, no root
nora                   # launches your $SHELL in a new window
```

`nora --smoke` opens a window, renders one frame, resizes, minimizes and
exits; it is the fastest way to check that the display and GPU path work
on a machine. `nora -- htop` runs a specific command instead of the shell.

## Status

- **v0.1 is complete and accepted** (2026-10-14). See
  [docs/13-v0.1-acceptance.md](docs/13-v0.1-acceptance.md) for the acceptance
  assessment: application matrix, P01–P10 performance workloads, resource
  limits and known limitations.
- **Post-v0.1 extensions (N32–N36) are complete**: kitty-style terminal
  graphics, shell integration (OSC 133/7/8), prompt navigation,
  previous-command selection, working-directory tracking, hyperlink
  activation, and the accessibility feasibility ADR (ADR-012).
- The N00–N36 roadmap is closed. The next post-v0.1 increment is N37
  (accessible terminal-text exposure), scoped in
  [docs/tasks/N37-accessible-text.md](docs/tasks/N37-accessible-text.md).

## Features

**Terminal core (v0.1)**

- Live PTY sessions with reliable child lifecycle, exit-status propagation
  and bounded output queues
- Tabs and splits with live configuration reload
- Scrollback (default 1,000 lines, configurable to 1,000,000), selection and
  scrollback search
- Native clipboard (OSC 52 reads disabled; writes opt-in) and bracketed paste
- Kitty keyboard protocol, IME, mouse reporting and focus events
- UTF-8 with per-character font fallback (CJK wide text, box drawing)
- GPU rendering (wgpu) with damage tracking and a pulse-gated render loop —
  no continuous redraw while idle
- Bounded resource use, audited in `docs/12-resource-limits.md`
- `TERM=nora` terminfo identity (a strict subset of `xterm-256color`)

**Terminal graphics (N32–N34)** — kitty-style protocol over the APC hook:

- RGB (`f=24`) and RGBA (`f=32`) uploads, chunked uploads (`m=1`), query,
  placement, delete, scroll and clipping
- Budgets: 32 MiB per image, 128 MiB total, 1,024 image/placement IDs
- PNG (`f=100`) is documented as a later slice

**Shell integration (N35)** — opt-in via `shell/nora-bash.sh` or
`shell/nora-zsh.zsh`:

- OSC 133 prompt markers (A/B/C/D)
- Jump to the previous prompt (Ctrl+Shift+Z) and select the previous
  command's output (Ctrl+Shift+G)
- Working-directory tracking (OSC 7)
- zsh only: inline suggestions. The integration reports the executed
  command line and exit status (`OSC 133;C;cmdline_url`, `OSC 133;D`) and
  the line editor's buffer on every redraw (`OSC 6973`); nora suggests the
  best match from your history (frequency, recency, current directory,
  past failures), else an unambiguous path or command-name completion, as
  dim text after the cursor. Right or End types it,
  Alt+Right one word. Lines starting with a space are never recorded
  ([ADR-013](docs/adr/ADR-013-autocomplete.md))
- Hyperlinks (OSC 8): Ctrl+click opens http(s), mailto, file and ftp links
  with the platform opener (`xdg-open`, `open`, `explorer`)

## Requirements

- Linux x86_64 with a native Wayland or X11 session. macOS and Windows
  builds compile and pass the headless tests in CI but are untested
  interactively; see [docs/15-platforms.md](docs/15-platforms.md).
- Rust 1.95 or newer (`rust-version` in `Cargo.toml`). `rust-toolchain.toml`
  pins 1.99.0 for rustup users and CI; a distribution toolchain at or above
  1.95 builds the same tree.
- A Vulkan loader and driver. Any GPU with a Vulkan driver works; CI and
  GPU-less machines use Mesa's lavapipe software driver, which validates the
  rendering path but is not hardware performance evidence.
- Fonts: `font.family` in the config picks the face (any fontconfig family,
  e.g. a Nerd Font for prompt icons); `font.size` is in points (default 15).
  Without a configured family nora prefers **Noto Sans Mono** and falls back
  to any monospace font fontconfig knows. Noto Sans CJK and Noto Serif Ethiopic are optional
  script fallbacks (without them, wide and Ethiopic text renders as missing
  glyphs). The GPU and text tests assert Noto metrics, so install Noto to
  run the full test suite:

  | Distribution | Packages |
  | --- | --- |
  | Debian / Ubuntu | `fonts-noto-core fonts-noto-cjk libvulkan1 mesa-vulkan-drivers` |
  | Arch | `noto-fonts noto-fonts-cjk vulkan-icd-loader` + your GPU's Vulkan driver |
  | Fedora | `google-noto-sans-mono-fonts google-noto-sans-cjk-fonts vulkan-loader` |
  | NixOS | `noto-fonts noto-fonts-cjk-sans`; see the NixOS note under Installing |

  `NORA_TEST_FONT_MONO`, `NORA_TEST_FONT_CJK` and `NORA_TEST_FONT_ETHIOPIC`
  override the font files explicitly (app and tests).

## Building

```sh
cargo build --release
```

Windowing backend configurations (winit):

| Configuration | Command |
| --- | --- |
| Wayland + X11 (default) | `cargo build --release` |
| X11 only | `cargo build --release --no-default-features --features x11` |
| Wayland only | `cargo build --release --no-default-features --features wayland` |

## Installing

```sh
./scripts/install.sh
```

No root required. Installs `~/.local/bin/nora`, the compiled `nora`
terminfo entry, the desktop entry and icon, and copies
`resources/nora.example.toml` to `~/.config/nora/config.toml` when no config
exists (an existing config is never touched). `./scripts/uninstall.sh`
removes the installed artifacts and preserves your config.

To install only the terminfo entry on another host (for SSH remote use):

```sh
tic -x terminfo/nora.terminfo
```

### NixOS

NixOS has no global library path, and nora loads the Vulkan loader,
libwayland, libxkbcommon and the X11 client libraries at runtime. On a
NixOS host `scripts/install.sh` detects this (`/etc/NIXOS`), installs the
binary under `~/.local/libexec/nora/` and writes a small launcher to
`~/.local/bin/nora` that sets `LD_LIBRARY_PATH` from the nixpkgs store
(fetched with `nix build`, cached outputs only). Everything else is the
same. To run the GPU tests or the uninstalled binary from `target/`,
export the same library path, for example:

```sh
export LD_LIBRARY_PATH=$(sed -n 's/^export LD_LIBRARY_PATH="\([^$]*\).*/\1/p' ~/.local/bin/nora)
cargo test --workspace
```

## Usage

```
usage: nora [OPTIONS] [-- COMMAND ARG...]

Options:
  -h, --help                   Print this help message
  -V, --version                Print nora's name and version
      --smoke                  Run one automated window smoke cycle and exit
      --config PATH            Use an explicit config file
      --check-config           Validate the config and exit (no display
                               or GPU needed)
      --reload                 Re-read the config from disk before launch;
                               a failed reload keeps the last valid config
      --working-directory PATH Start the command in PATH
      --hold                   Keep the window open after the child
                               exits (windowed; headless ignores it)
      --                       Remainder is the command argv (no shell
                               interpolation)
```

With no command, the default shell (`$SHELL`, else `/bin/sh`) is launched.
The process exit status reflects the launched command. With no display
server available, the launch falls back to the headless session pump.

### Key bindings

| Action | Keys |
| --- | --- |
| Copy / paste | Ctrl+Shift+C / Ctrl+Shift+V |
| New tab | Ctrl+Shift+T |
| Next / previous tab | Ctrl+PageDown / Ctrl+PageUp |
| Split right / split below | Ctrl+Shift+Enter / Ctrl+Shift+Backslash |
| Focus pane | Ctrl+Shift+Arrow |
| Close pane (tab, window) | Ctrl+Shift+W |
| Reload config | Ctrl+Shift+F5 |
| Previous prompt / select last output | Ctrl+Shift+Z / Ctrl+Shift+G (shell integration) |
| Accept inline suggestion / its next word | Right or End / Alt+Right (zsh integration; otherwise the key goes to the shell) |
| Search (scrollback and screen) | Ctrl+Shift+F; Enter older match, Shift+Enter newer, Escape closes |
| Zoom in / out / reset | Ctrl+Shift+Plus / Ctrl+Shift+Minus / Ctrl+Shift+0 |
| Scrollback | Shift+PageUp / Shift+PageDown, Shift+Home / Shift+End |

With more than one tab a tab bar on the top row shows each tab's number
and title. Splits are separated by a thin line, and clicking a pane
focuses it.

Plain Ctrl+letter keys always reach the program in the terminal, so
Ctrl+D, Ctrl+W, Ctrl+T, Ctrl+F and Ctrl+X behave as in any terminal. A
binding with nothing to act on, such as tab switching with one tab, also
passes its key through.

### Configuration

Config lives at `~/.config/nora/config.toml` (or
`$XDG_CONFIG_HOME/nora/config.toml`); `--config PATH` picks another file.
Every key is optional. The commented example in
`resources/nora.example.toml` lists every section and key:

| Section | Keys |
| --- | --- |
| `[font]` | `family`, `size` (points) |
| `[colors]` | `foreground`, `background`, `cursor`, `selection_background`, `[colors.normal]` / `[colors.bright]` ANSI colors |
| `[window]` | `columns`, `lines` (initial size in cells) |
| `[cursor]` | `style` = `block`, `beam` or `underline` |
| `[scrolling]` | `history` (lines, default 10000), `multiplier` (wheel lines) |
| `[autocomplete]` | `enabled` (default true): inline suggestions from your command history, kept in `$XDG_STATE_HOME/nora/history` |
| `[[keyboard.bindings]]` | `keys` = chord, `action` = action name or `none` |

The file is watched: saving it applies the change to the running window,
and Ctrl+Shift+F5 reloads by hand. Colors, cursor, bindings and fonts
apply immediately, scrollback size applies to new panes, and window size
at the next start. A file that fails to parse or validate keeps the
current settings and shows the error on the bottom row. Precedence: CLI
flags > config file > built-in defaults. The pre-0.2 sections
`[terminal]`, `[theme]` and `[keys]` are still accepted; the new sections
win when both are present.

Validate a config without launching:

```sh
nora --config ~/.config/nora/config.toml --check-config
```

### Shell integration

Source `shell/nora-bash.sh` from your `.bashrc` to enable OSC 133 prompt
markers. This unlocks prompt navigation (jump to previous prompt),
previous-command selection, working-directory tracking and hyperlinks.
The hook coexists with other `PROMPT_COMMAND`/DEBUG-trap shell
integrations (e.g. starship).

For zsh, source `shell/nora-zsh.zsh` from your `.zshrc`. It adds the same
markers and directory tracking through `precmd`/`preexec`, and reports
the editor buffer through `zle-line-pre-redraw` (pure zsh, no process per
keystroke; unchanged buffers are not re-sent). Other terminals ignore the
reports.

## Development

Check set (what CI runs; see `.github/workflows/ci.yml`):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
```

CI additionally builds the X11-only and Wayland-only configurations and
runs `nora --smoke` under Xvfb and under Weston's headless backend with
the lavapipe software GPU. Tests that need a GPU adapter fail with
`a GPU adapter must be available` when no Vulkan driver is loadable; the
PTY tests have 10 s deadlines and can flake on a heavily loaded machine
(they pass when re-run alone).

Repository layout:

| Path | Purpose |
| --- | --- |
| `src/` | Library: engine, session, input, render, config, protocol filters |
| `src/app/` | Windowed application: CLI, window, workspace, render loop |
| `src/bin/` | `nora_child` test helper, `nora_bench` benchmark tool |
| `tests/` | Integration tests (one target per feature area) |
| `vendor/alacritty_terminal/` | Vendored terminal engine (path dependency) |
| `shell/` | Shell integration scripts (bash, zsh) |
| `terminfo/` | `nora` terminfo entry source |
| `resources/` | Desktop entry, icon, example config |
| `scripts/` | Clean build, install, uninstall |
| `docs/` | Product, architecture, compatibility, task cards, ADRs, status |

### Documentation

| Document | Purpose |
| --- | --- |
| [AGENTS.md](AGENTS.md) | Rules for agents implementing nora |
| [01-research.md](docs/01-research.md) | Evidence, alternatives, recommended approach |
| [02-product.md](docs/02-product.md) | What nora ships and what it defers |
| [03-architecture.md](docs/03-architecture.md) | Module ownership, concurrency, snapshots, resource limits |
| [04-performance.md](docs/04-performance.md) | Metrics, goals and benchmark design |
| [05-rendering.md](docs/05-rendering.md) | Fixed-cell fonts, caching, damage and GPU lifecycle |
| [06-compatibility.md](docs/06-compatibility.md) | Protocol contract and input behavior |
| [07-build-release.md](docs/07-build-release.md) | Dependencies, build, CI and packaging |
| [08-agent-workflow.md](docs/08-agent-workflow.md) | Small-context workflow and commit/push requirements |
| [09-validation.md](docs/09-validation.md) | Correctness and integration tests |
| [10-roadmap.md](docs/10-roadmap.md) | Ordered feature cards and release gates |
| [11-risks.md](docs/11-risks.md) | Failure modes and decision triggers |
| [12-resource-limits.md](docs/12-resource-limits.md) | Measured resource-limit evidence |
| [12-sources.md](docs/12-sources.md) | Primary-source links and evidence boundaries |
| [13-extensions.md](docs/13-extensions.md) | Graphics, shell integration and later features |
| [13-v0.1-acceptance.md](docs/13-v0.1-acceptance.md) | v0.1 acceptance assessment |
| [status/PROGRESS.md](docs/status/PROGRESS.md) | Durable implementation state |
| [status/HANDOFF_TEMPLATE.md](docs/status/HANDOFF_TEMPLATE.md) | Short recovery checkpoint |

Work proceeds in small increments against the task cards in `docs/tasks/`;
`docs/status/PROGRESS.md` is the resume index.

## License

MIT (per the `Cargo.toml` manifest).

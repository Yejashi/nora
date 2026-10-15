# nora

A fast, efficient, maintainable Linux desktop terminal written in Rust,
with native Wayland and X11 support.

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

**Shell integration (N35)** — opt-in via `shell/nora-bash.sh`:

- OSC 133 prompt markers (A/B/C/D)
- Jump to previous prompt and previous-command selection
- Working-directory tracking (OSC 7)
- Hyperlinks (OSC 8) with click activation via `xdg-open`

## Requirements

- Linux x86_64 with a native Wayland or X11 session
- Rust 1.95 (pinned by `rust-toolchain.toml`)
- A Vulkan-capable GPU (CI uses lavapipe; a software GPU validates the
  rendering path but is not hardware performance evidence)

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

### Configuration

Config lives at `~/.config/nora/config.toml` (or
`$XDG_CONFIG_HOME/nora/config.toml`). Every key is optional; omitted values
use nora's built-in defaults. See `resources/nora.example.toml` for a
commented example. Precedence: CLI flags > config file > built-in defaults.

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

## Development

Check set:

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
```

Repository layout:

| Path | Purpose |
| --- | --- |
| `src/` | Library: engine, session, input, render, config, protocol filters |
| `src/app/` | Windowed application: CLI, window, workspace, render loop |
| `src/bin/` | `nora_child` test helper, `nora_bench` benchmark tool |
| `tests/` | Integration tests (one target per feature area) |
| `vendor/alacritty_terminal/` | Vendored terminal engine (path dependency) |
| `shell/` | Shell integration script |
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

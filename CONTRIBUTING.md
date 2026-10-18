# Contributing to nora

Thank you for your interest in nora. Bug reports, fixes, tests and
documentation are all welcome.

## Before you start

- **Bugs.** Open an issue with what you did, what you expected and what
  happened. Include your distribution, Wayland or X11, your GPU driver and
  `nora --version`. Running nora with `NORA_DEBUG=1` prints a log that
  often shows the cause.
- **Larger changes.** Please open an issue first so we can agree on the
  approach before you spend time on it.
- **Security problems** go to a private advisory, not an issue: see
  [SECURITY.md](SECURITY.md).

Please be kind and patient in issues and reviews. Everyone here is
volunteering their time.

## Building

```sh
git clone https://github.com/Yejashi/nora.git
cd nora
cargo build --release
```

[`rust-toolchain.toml`](rust-toolchain.toml) pins the toolchain, and rustup
installs it on first build. The [README](README.md#requirements) lists
the system packages.

## Checks

Run what CI runs before you send a pull request:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- **GPU tests** need a Vulkan driver. Mesa's lavapipe is enough; on Debian
  and Ubuntu, select it with
  `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json`.
- **Window tests.** [`scripts/e2e-x11.sh`](scripts/e2e-x11.sh) drives a
  real nora window under Xvfb, with real key and mouse events. It needs
  Xvfb, xdotool and ImageMagick, and some scenarios need zsh, vim, tmux,
  htop, chafa or mpv; a scenario whose tools are missing is skipped. Run
  one scenario with `scripts/e2e-x11.sh search`.

## Guidelines

- **Behavior changes come with a test.** A bug fix should have a test that
  fails without it.
- **Terminal behavior comes from
  [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal).**
  It is vendored in `vendor/` with a small patch set; every change there
  is marked `// nora:` and listed in `Cargo.toml`. nora does not parse
  escape sequences itself.
- **Resources stay bounded.** Queues, caches and anything a program running
  in the terminal can grow have a limit in bytes as well as in count.
  [docs/12-resource-limits.md](docs/12-resource-limits.md) lists them.
- **No blocking work under the terminal lock:** no file, font, GPU or
  clipboard work while holding it.
- **Design decisions** (a new dependency, a renderer or threading change)
  get a short ADR in [docs/adr/](docs/adr/).
- **Commits** follow the `type(scope): summary` style used in the history,
  for example `fix(selection): ...` or `feat(graphics): ...`. Keep one
  change per commit.

## Where things are

| Path | What it holds |
| --- | --- |
| `src/engine.rs`, `src/stream.rs`, `src/session/` | Terminal engine adapter, stream filters, PTY sessions and the I/O thread |
| `src/app/` | Window, input, rendering, panes and tabs |
| `src/input/`, `src/render/`, `src/config.rs` | Key and mouse encoding, colors, configuration |
| `src/graphics_*.rs` | Kitty graphics protocol |
| `src/suggest.rs`, `src/shell_input.rs`, `shell/` | Suggestions and shell integration |
| `tests/`, `scripts/e2e-x11.sh` | Integration tests and window tests |
| `docs/` | Architecture, compatibility, resource limits, ADRs and performance reports |

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE), the license of the project.

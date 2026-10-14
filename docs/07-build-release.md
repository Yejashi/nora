# Build, dependencies and release

## Dependency selection

Versions below were observed in primary docs/upstream metadata on 10 October 2026. They are **candidate constraints**, not a compiled lockfile. N01 establishes one known-working tuple and pins it. Do not blindly paste together unrelated `latest` examples.

| Dependency | Observed candidate | Use | Verification |
| --- | --- | --- | --- |
| `alacritty_terminal` | 0.26.0 | Terminal engine/PTY facilities | Public API, source, engine/MSRV [S04] |
| `winit` | 0.30.13 | Window/input/scale/events | Lifecycle and native backend features [S12] |
| `wgpu` | 30.0.1 | GPU graphics | Platform features, surface API, Rust requirement [S13, S16] |
| `glyphon` | Upstream manifest 0.12.0 | Optional text rendering adapter | Manifest uses wgpu 30 and cosmic-text 0.19; verify published release [S29] |
| `cosmic-text` | 0.19.0 | Font system/raster/shaping | Use glyphon's compatible version/re-export if selected [S14] |
| `smithay-clipboard` / X11 clipboard helper | Select in N15 | Native clipboard | winit handles, feature flags, ownership [S25, S26] |
| `clap`, `serde`, `toml`, `log`/`tracing` | Select only needed versions | CLI/config/diagnostics | License/MSRV/tree; avoid duplicate logging stacks |
| `criterion`, property/fuzz tools | Development only | Validation/performance | Compatible toolchain; no runtime inclusion |

Use `cargo tree -d` and actual compile errors to reconcile versions. A development branch manifest is not a published crate guarantee. If a compatible tuple requires older supported releases, pin those and document why. Choose the Rust stable toolchain supported by all selected crates; `rust-toolchain.toml` pins the version, not the moving word `stable`. Check the engine's and graphics crates' actual `rust-version` constraints. Never invent the MSRV.

## Repository layout to grow into

| Path | Purpose |
| --- | --- |
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` | Reproducible build inputs |
| `src/main.rs`, `src/app/`, `src/workspace/` | CLI/window/application state |
| `src/engine/`, `src/session/`, `src/input/` | Emulation, PTY and input boundaries |
| `src/render/`, `src/platform/`, `src/config/` | Graphics and OS/config integration |
| `tests/`, `tests/fixtures/` | Deterministic engine/input/session tests |
| `tools/nora-dev/` | Rust test/benchmark generator and harness |
| `assets/` | Embedded shader/config/desktop assets and attributed test fonts |
| `resources/` | Desktop entry, icon and example config |
| `scripts/` | Clean-build, install and uninstall scripts |
| `packaging/` | Additional install metadata (grow as needed) |
| `docs/status/`, `docs/decisions/`, `docs/performance/` | Recovery, decisions, measured results |

Do not create all directories as empty abstractions in N00. Grow them with the feature that needs them. Keep the early app in a single crate; add the tools package when a real helper is implemented.

## Local checks

After workspace creation, the ordinary check set is:

```sh
cargo fmt --all --check
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --locked
```

Run targeted checks for each small increment; run the full relevant set before milestone/release commits. If tests require a display, separate them from headless tests and report both. `--all-features` is not always meaningful for mutually exclusive backends; maintain explicit supported feature configurations instead. Fuzz/nightly and performance runs are separate documented commands.

No command is evidence until the agent runs it and records its result. N00 is documentation/build scaffolding, so do not pretend that nonexistent Cargo checks ran.

## CI strategy

Create CI for formatting, headless state/input/layout/session tests, Clippy and release compilation. Establish native system dependencies from the pinned libraries' documentation and the CI image; list them in installation instructions. Names differ by distribution, so do not hardcode an assumed Arch/Ubuntu package equivalence without verification.

Add X11 smoke tests under an available virtual display and native Wayland smoke tests under a nested compositor when the environment supports them. A software GPU can validate lifecycle and rendering paths; it is not hardware performance evidence. Missing compositor jobs are explicitly pending release checks, not replaced by a passing no-display unit suite.

Test both default Linux backend features and documented X11-only/Wayland-only build configurations where supported. Cache build dependencies carefully; no cache should be required for a clean build. Commit Cargo.lock for the application and tools workspace.

Use a normal release profile first. Benchmark thin LTO, codegen-unit choices and symbol stripping later. Never set `target-cpu=native` for distributed binaries. GPU validation/debug flags must be distinguishable from shipping settings. Keep panic unwinding initially; changing to abort requires an ADR because cleanup/diagnostics behavior changes.

## Packaging and diagnostics

Nora should report version, commit/build metadata when available, renderer/backend and platform context through an explicit diagnostic mode. Default logs must not contain typed commands, clipboard data or raw terminal output.

Package a desktop entry, icon, executable, example TOML, optional compiled terminfo and notices. Follow XDG directories for configuration, state and caches [S21]. Cache files are disposable and versioned; configuration is never overwritten during install/update. Provide a portable install procedure and one tested distro packaging recipe first. A package filename does not prove installation or desktop launch succeeds.

Audit direct/transitive dependency licenses and preserve notices for any adapted upstream files. Alacritty publishes Apache-2.0 [S30]; Ghostty publishes MIT [S31]; kitty publishes GPLv3 [S32]. These are inspected upstream labels, not permission to remove component notices. Implement protocols from their specifications; importing kitty code changes the licensing analysis and requires a separate explicit decision.

### Implemented (N30)

- **CI** (`.github/workflows/ci.yml`): clean-checkout release build, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, the full test suite with a software GPU (lavapipe, for the offscreen GPU tests), and display smoke jobs — `nora --smoke` under Xvfb (X11) and under Weston's headless backend (native Wayland; a successful XWayland run does not validate native Wayland).
- **Explicit backend configurations**: `cargo build --no-default-features --features x11` (X11 only) and `cargo build --no-default-features --features wayland` (Wayland only) are documented, CI-tested configurations; the default build enables both.
- **Clean build**: `scripts/clean-build.sh` runs `cargo clean` + `cargo build --release` and verifies the binary. Set `CARGO_TARGET_DIR` to keep the shared target directory intact.
- **Verified Linux install recipe** (per user, no root required): `git clone <repo> && cd <repo>`, then `./scripts/install.sh` — builds the release binary, then installs `~/.local/bin/nora`, the compiled terminfo entry (`tic` into `~/.terminfo`), the desktop entry (`~/.local/share/applications/nora.desktop`) and the icon (`~/.local/share/icons/hicolor/128x128/apps/nora.png`). An existing `~/.config/nora/config.toml` is never touched; when absent, `resources/nora.example.toml` is copied in as a starting point. `./scripts/uninstall.sh` removes the installed artifacts and preserves the user config.
- **Desktop entry** (`resources/nora.desktop`): `Name`, `Exec=nora` (resolved from `PATH`), `Icon=nora`, `Categories=Utility;TerminalEmulator;`, `Terminal=false` (nora itself is the terminal, per the freedesktop spec).
- **Headless smoke in CI**: with no display server, the launch path (`nora -- CMD`) falls back to the headless session pump; CI runs it and checks child exit-status propagation. `nora --smoke` requires a display and runs in the display smoke jobs.
- **Packaging tests**: `tests/packaging.rs` covers the clean build, install, desktop entry, example config, and uninstall-preserves-config acceptance.
- No external release is published or tagged by this work; a release requires the owner's release authorization.

## Release gate

Run [02-product.md](02-product.md) acceptance on a clean checkout, record actual compatibility/performance results, verify native Wayland and X11, check child and memory stress behavior, and test install/uninstall without deleting user configuration. Tag a release only after the owner authorizes the public release operation; ordinary feature commits and pushes are already required by this plan.

A feature can be implemented, tested, committed and pushed while a hardware release check remains pending. Keep those statuses separate. Do not advertise a v0.1 daily-driver release before its native/manual gates pass.

## Terminal identity and SSH remote use (N29)

nora advertises its identity to child processes via `TERM=nora` (and
`COLORTERM=truecolor`). The dedicated `nora` terminfo entry is the source of
truth for what nora advertises; see
[ADR-010](adr/ADR-010-terminfo-identity.md) and the "TERM and terminfo"
section of `06-compatibility.md`.

### Installing the `nora` terminfo entry

The entry source is `terminfo/nora.terminfo` (in the nora repository).
Compile and install it on any host where nora will run:

```sh
# From the nora repository root, install for the current user:
tic -x terminfo/nora.terminfo
```

This compiles the entry and installs it under `~/.terminfo/n/nora`. To
install system-wide (so all users can use it):

```sh
sudo tic -x -o /usr/share/terminfo terminfo/nora.terminfo
```

The `-x` flag preserves extended capabilities. Verify the installation:

```sh
infocmp -1 nora
```

The `nora` entry is a strict subset of `xterm-256color`'s capabilities, so
any host that has `xterm-256color` (every Linux distribution) can run nora
even without the `nora` entry installed.

### SSH remote use

When nora is used over SSH (e.g. `ssh host` then `nora`, or `nora -- ssh
host`), the remote shell's `TERM` determines what the remote terminal
advertises. The fallback strategy:

1. **Preferred**: the remote host has the `nora` terminfo entry installed
   (via the `tic` command above). The remote terminal advertises
   `TERM=nora`, and applications use the `nora` entry.
2. **Fallback**: the remote host does not have the `nora` entry. The remote
   terminal falls back to `TERM=xterm-256color`, which is present on every
   Linux distribution. This is safe because the `nora` entry is a strict
   subset of xterm-256color.

To install the `nora` entry on a remote host without a local checkout of
the nora repository, copy the source file and compile it there:

```sh
# From the nora repository root:
scp terminfo/nora.terminfo user@remote-host:/tmp/
ssh user@remote-host 'tic -x /tmp/nora.terminfo'
```

`COLORTERM=truecolor` is set by nora for every child it spawns, so
true-color output works on the remote host regardless of the `TERM` value.

### Guardrail

No child ever sees an unknown or missing `TERM`: nora sets `TERM=nora` when
the entry is installed, and falls back to `TERM=xterm-256color` otherwise.

# nora implementation progress

Plan created: 10 October 2026. Application implementation has not started. No build, compatibility, hardware or performance test has been run for nora.

## Current state

- Active feature: none.
- Next task: `N01-dependencies` (earliest ready task after N00 completes).
- Accepted default: Rust + alacritty_terminal + winit + wgpu; text adapter subject to N02 gate.
- Target: Linux native Wayland and X11.
- Git remote/branch: `origin` = github.com:Yejashi/nora.git, branch `main`, default branch.
- Required workflow: validate → update notes → commit → push → verify, for every completed feature change.

## Completed implementation increments

- **N00-bootstrap** — status: `validated` (commit/push pending at this snapshot).
  - Observable behavior: clean checkout builds the `nora` binary; `nora --version` prints `nora 0.1.0`; `--help`/unknown flags exit with usage (exit 2); no terminal UI.
  - Added: `Cargo.toml` (v0.1.0, edition 2024, rust-version 1.99, no deps yet), `Cargo.lock`, `rust-toolchain.toml` (1.99.0), `.gitignore` (`/target`), `src/main.rs`, `.github/workflows/ci.yml` (fmt/clippy/build/test).
  - Checks run: `cargo fmt --check` OK; `cargo build --quiet` OK; `cargo clippy --all-targets` clean; `cargo test` 1 passed; `./target/debug/nora --version` → `nora 0.1.0`.
  - Decisions: toolchain pinned to 1.99.0 (installed stable); edition 2024; release profile `lto = "thin"`, `codegen-units = 1`; GitHub Actions CI added because the configured remote is GitHub; no Python anywhere in project files.

When recording a task, use: ID; observable behavior; validation commands/results; remaining manual release checks. Status values: `not_started`, `in_progress`, `validated`, `blocked`. Overall completion additionally requires the post-commit push verification.

## Blockers and limitations

- Dependency tuple not compiled/pinned.
- Fixed-cell text adapter not validated.
- End-to-end session write/event limits not implemented.
- No native display, protocol or performance evidence yet.

## Last verified Git state

None recorded. Record a previous feature's hash/remote verification only after it exists. Never fabricate the hash of the commit currently being authored.

## Next action

Read `docs/10-roadmap.md`, then the `N01` task card and only its listed documents/source files; add the window/platform and terminal-engine dependency spikes.

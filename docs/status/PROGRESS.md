# nora implementation progress

Plan created: 10 October 2026. Implementation started at N00 on 10 October 2026.

## Current state

- Active feature: none.
- Next task: `N02-text-spike` (earliest ready task after N01 completes; N03 also ready).
- Accepted default: Rust + alacritty_terminal + winit + wgpu; text adapter subject to the N02 gate.
- Target: Linux native Wayland and X11.
- Git remote/branch: `origin` = github.com:Yejashi/nora.git, branch `main`, default branch.
- Required workflow: validate → update notes → commit → push → verify, for every completed feature change.

## Completed implementation increments

- **N01-dependencies** — status: `validated`.
  - Commit: `chore(deps): pin compatible terminal graphics stack (N01)` on `main`; remote-head hash recorded at the next increment (never fabricate the hash of the commit being authored).
  - Observable behavior: `nora` (no args) opens a native window, prints platform/adapter/surface metadata and stays open until closed; `nora --smoke` runs one automated open (800x600) → resize to 640x480 → minimize → 250 ms settle → close cycle, exiting 0 on success and 1 on startup failure or timeout.
  - Pinned tuple (Cargo.lock committed): alacritty_terminal 0.26.0 (vte 0.15.0), glyphon 0.12.0 → cosmic-text 0.19.0, wgpu 30.0.1, winit 0.30.13, pollster 0.4. `cargo tree -d`: font-types 0.11.3/0.12.6 and other duplicates via harfrust/skrifa/gpu-allocator — benign, locked.
  - Live native Wayland run (10 Oct 2026, wayland-0 session): adapter AMD Radeon RX 9070 XT, Vulkan backend, DiscreteGpu, driver radv; surface `Rgba8UnormSrgb` at 800x600 scale 1.0; smoke exit 0; interactive run stayed open until killed at 5 s. Driver prints "radv is not a conformant Vulkan implementation, testing use only" — driver-side notice, not a failure.
  - Headless regression tests: `tests/engine.rs` (4 tests: bytes render into grid cells; CRLF starts new lines; bare LF keeps the column; SGR escape produces no cells) and `tests/font_stack.rs` (1 test: system monospace resolves through glyphon/cosmic-text).
  - API corrections verified against pinned sources: winit 0.30 has no `Window::close` (the window closes with the event loop); `StartCause::ResumeTimeReached` replaces `::Deadline` and lives in `winit::event`; wgpu 30 has no `winit` feature — `Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(el.owned_display_handle())))` + `create_surface(Arc<Window>)`; the wgpu `Surface` retains the `Arc<Window>`, so dropping nora's Arc does not close the window.
  - Validation: `cargo fmt --all` clean; `cargo build` and `cargo build --release` OK; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test` 6 passed; `timeout 30 ./target/release/nora --smoke` exit 0 on native Wayland; `nora --version` → `nora 0.1.0` exit 0; unknown flag exit 2.
  - Pending manual release checks: native X11 run (no X11 session in this environment; an XWayland run would not count as native X11).
- **N00-bootstrap** — status: `validated`, committed/pushed/verified.
  - Commit `c3a6707` ("chore(repo): bootstrap nora build (N00)"), remote `origin/main` = `c3a67072922d357c1ec35025d4c6d68e6aba2fda` verified via `git ls-remote`.
  - Observable behavior: clean checkout builds the `nora` binary; `nora --version` prints `nora 0.1.0`; `--help`/unknown flags exit with usage (exit 2); no terminal UI.
  - Added: `Cargo.toml` (v0.1.0, edition 2024, rust-version 1.99, no deps yet), `Cargo.lock`, `rust-toolchain.toml` (1.99.0), `.gitignore` (`/target`), `src/main.rs`, `.github/workflows/ci.yml` (fmt/clippy/build/test).
  - Checks run: `cargo fmt --check` OK; `cargo build --quiet` OK; `cargo clippy --all-targets` clean; `cargo test` 1 passed; `./target/debug/nora --version` → `nora 0.1.0`.
  - Decisions: toolchain pinned to 1.99.0 (installed stable); edition 2024; release profile `lto = "thin"`, `codegen-units = 1`; GitHub Actions CI added because the configured remote is GitHub; no Python anywhere in project files.

When recording a task, use: ID; observable behavior; validation commands/results; remaining manual release checks. Status values: `not_started`, `in_progress`, `validated`, `blocked`. Overall completion additionally requires the post-commit push verification.

## Blockers and limitations

- No terminal content rendered yet; engine adapter arrives at N03, text adapter at N02.
- Native X11 evidence pending (no X11 session in this environment).
- End-to-end session write/event limits not implemented.
- No performance evidence yet.

## Last verified Git state

- Previous increment N00: commit `c3a6707`, remote `origin/main` = `c3a67072922d357c1ec35025d4c6d68e6aba2fda` (verified via `git ls-remote`).
- N01: committed in this increment; the remote-head hash is recorded at the next increment after push verification.

## Next action

Read `docs/10-roadmap.md`, then the `N02` task card and only its listed documents/source files; implement the fixed-cell text adapter spike (smallest adapter that draws terminal cells without layout drift).

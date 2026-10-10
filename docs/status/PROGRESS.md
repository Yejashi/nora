# nora implementation progress

Plan created: 10 October 2026. Implementation started at N00 on 10 October 2026.

## Current state

- Last completed feature: `N02-text-spike` — both slices `validated` (N02a: headless per-cell shaping/placement in `tests/text_placement.rs`, 9/9; N02b: offscreen wgpu render of a cell grid via per-cell `TextArea`s in `tests/text_render.rs`, 1/1). N02 gate closed: the per-cell glyphon adapter is accepted on the pinned tuple; no ADR required.
- Next task: `N03-engine-adapter` (also ready).
- Accepted default: Rust + alacritty_terminal + winit + wgpu; text adapter subject to the N02 gate.
- Target: Linux native Wayland and X11.
- Git remote/branch: `origin` = github.com:Yejashi/nora.git, branch `main`, default branch.
- Required workflow: validate → update notes → commit → push → verify, for every completed feature change.

## Completed implementation increments

- **N02b-text-spike (slice 2 of N02)** — status: `validated`; committed in this increment, remote head verified after push.
  - Observable behavior: `tests/text_render.rs` (1 test) renders a 5×2 cell grid (60×48 px, pure-black background) on the pinned tuple to a plain offscreen wgpu texture (no surface — headless via `InstanceDescriptor::new_without_display_handle`; wgpu 30 has no offscreen surface for this use), one cosmic `Buffer` per cell cluster and one `TextArea` per cell with `bounds` = the cell allocation: narrow `A`, combining `e\u{301}`, wide CJK `你` (span 2), box drawing `─` (span 1), Ethiopic `ᴀ`-class `ሀ` (span 2) via the pinned mono/CJK/Ethiopic faces. Every inked pixel lies inside its cell allocation (no column drift between terminal math and shaped geometry); PPM written to `/tmp/opencode/n02b_grid.ppm`.
  - Cache strategy (executable proof; replaces the initial SwashCache check): glyphon 0.12's `prepare` rasterizes misses via `SwashCache::get_image_uncached` and never writes `SwashCache.image_cache` — the persistent cache is `TextAtlas`' per-content `glyph_cache` (pub(crate), unbounded `LruCache` protected by `glyphs_in_use`), and the hit path (`GpuCacheStatus::InAtlas`) skips rasterization entirely. Proof: frame 3 re-renders the unchanged cells through a hermetic faceless `FontSystem` (`new_with_locale_and_db_and_fallback` with an empty `fontdb::Database`) — a miss would need to resolve a face and none exists, so the glyph would drop; frames 2 and 3 are both pixel-identical to frame 1, so every steady-state glyph was served from the atlas cache.
  - Measured (debug build, headless radv): cell width 12.0 px (whole, matches N02a); prepare frame 1 (cold) 4.67 ms, frame 2 (warm) 644 µs, frame 3 (faceless) 44 µs; full test ~0.18 s. Cache growth key is the distinct (font, size, codepoint) working set, not scrollback; the public mitigation is `TextAtlas::trim` (no public size introspection on `TextAtlas`).
  - API discoveries (verified against pinned sources): wgpu 30 delivers `map_async` callbacks from `Device::poll`/`Instance::poll_all` — blocking on the mpsc receiver alone hangs; working pattern: `map_async` → mpsc, then loop `device.poll(PollType::wait_indefinitely())` until the result arrives. `Buffer::get_mapped_range` returns a `BufferView` registering the range — drop the view before `unmap()` or wgpu panics ("cannot unmap a buffer that still has accessible mapped views"). `RenderPassColorAttachment` requires `ops: Operations`; `TextureDescriptor` requires `mip_level_count`, `sample_count`, `dimension`; `begin_render_pass`/`copy_texture_to_buffer` take `&mut self`. glyphon `bounds`/`left`/`top` are absolute surface coordinates in one shared space; per-glyph clipping is geometric.
  - Validation: `cargo fmt --all -- --check` clean; `cargo build` OK; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test` → 16 passed (nora bin unit 1, engine 4, font_stack 1, text_placement 9, text_render 1).
- **N02a-text-spike (slice 1 of N02)** — status: `validated`.
  - Committed in this increment; remote-head hash recorded after push verification.
  - Observable behavior: `tests/text_placement.rs` (9 tests) proves the per-cell model on pinned glyphon 0.12.0 / cosmic-text 0.19.0: one cosmic `Buffer` per cell cluster; cell origins are pure terminal math (`col * cell_w`, `row * cell_h`); narrow and combining clusters ≤ 1 cell (±0.5 px); CJK and Ethiopic clusters within (1, 2] cells with per-cluster provenance pinned to the three test fonts; scales 1.0/1.25/1.5/2.0 scale glyph size but not cell math; unchanged cells keep bit-identical geometry with no re-shape on frame 2; an 80×24 grid (1920 single-cell buffers) shows zero drift (min = max = 12.00 px advance at a 20 px font).
  - Measured (debug build, this machine): 1920 single-cell shapes in 178.8 ms — the CPU shaping cost the per-cell model pays; presentation cost is the N02b concern.
  - API discoveries (verified against pinned sources): `FontSystem::new_with_fonts` is not hermetic — cosmic-text calls `db.load_system_fonts()` first, so system faces (DejaVu, …) coexist in the db; family matches resolve to the system-scan copies whose `Source` is `SharedFile(path, _)` (fontdb 0.23), not `Source::File` — provenance checks must handle both arms. A CJK glyph at 20 px (~20 px advance) intentionally overflows one mono cell (~12 px); that overflow motivates the two-cell allocation, it is not a bug.
  - Pinned test fonts: `/usr/share/fonts/noto/NotoSansMono-Regular.ttf`, `/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc`, `/usr/share/fonts/noto/NotoSerifEthiopic-Regular.ttf`.
  - Validation: `cargo fmt --all -- --check` clean; `cargo build` OK; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test` → 15 passed (lib 1, engine 4, font_stack 1, text_placement 9).
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

- No terminal content rendered yet; the text adapter was proven at N02, the engine adapter arrives at N03.
- Native X11 evidence pending (no X11 session in this environment).
- End-to-end session write/event limits not implemented.
- No performance evidence yet.

## Last verified Git state

- N00: commit `c3a6707`; N01: commit `a8beb8a` — remote `origin/main` = `a8beb8ae136c7acc997fc24eeea88bffcda9773b` (verified via `git ls-remote` after the N01 push).
- N02a: commit `402594a`; remote `origin/main` = `402594a2af5d4ca7d8d8f1c6944b0f901fe9c230` (verified via `git ls-remote` after the push).
- N02b: committed in this increment; the remote-head hash is verified after the push.

## Next action

Implement N03-engine-adapter: read `docs/tasks/N03-engine-adapter.md` first; slice the terminal engine adapter (alacritty_terminal → renderable cell state, terminal state kept out of GPU/platform modules).

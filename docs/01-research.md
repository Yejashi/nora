# Research and recommendation

Research date: 10 October 2026. Sources are primary project documentation, public APIs, specifications and upstream code. Stable source labels resolve in [12-sources.md](12-sources.md). Dependency versions and upstream branches must be rechecked at implementation time. No nora code was built or benchmarked for this report.

## Recommended approach

Build an original Rust desktop frontend around `alacritty_terminal`. Use `winit` for the Linux window/event layer, `wgpu` for rendering, and `cosmic-text` for font discovery, fallback, shaping and rasterization. Evaluate `glyphon` only as a replaceable GPU text helper. Verify fixed-cell placement, cache limits and compatible versions before committing to it.

The recommendation optimizes for implementation reliability and maintainability with a local coding agent. It is not a measured speed ranking. A mature engine avoids spending the project's early effort reproducing escape sequences, alternate screens, scrolling and wide-character behavior. A renderer and scheduler built for terminal cells provide a place to improve responsiveness and add nora's UI features.

## What existing terminals establish

| Project | Observed design | Lesson for nora | Boundary |
| --- | --- | --- | --- |
| kitty | C for performance-sensitive work, Python for UI/extensibility, Go for command-line helpers; OpenGL rendering [S01] | Removing Python alone is not a performance strategy | Protocol compatibility does not require copying kitty's implementation |
| Alacritty | Rust desktop terminal with an OpenGL frontend and separately published `alacritty_terminal` library [S03, S04] | Reuse its terminal engine while owning nora's frontend | Its desktop renderer is not supplied by the engine crate |
| Ghostty | Shared Zig core with platform-specific applications; Linux OpenGL and macOS Metal described upstream [S05, S06] | Native compiled terminals can be feature-rich; study mature designs | Do not interpret the full app's capabilities as the exposed VT library API |
| `libghostty-vt` | Available for Zig/C; focused on parsing and state; upstream says API signatures remain in flux and it is not yet version-tagged [S06] | A credible alternative engine, especially for later protocol breadth | Requires a pinned revision and an FFI/build-maintenance decision for Rust |

Kitty's performance documentation distinguishes energy use, keyboard-to-screen latency and throughput. Its throughput tool normally suppresses rendering, and its published table uses older releases and a particular machine [S02]. Therefore neither an old table nor a raw `cat` timing establishes the fastest terminal on the user's current machine.

Foot was considered as a C/Wayland design reference. The canonical repository at Codeberg could not be retrieved during this research. Its architecture and numerical performance are therefore not used as verified evidence for this recommendation. Revisit the canonical project before using its design or copying code.

## Options and engineering tradeoffs

| Path | Advantages | Cost/risk | Decision |
| --- | --- | --- | --- |
| Rust frontend + Alacritty engine | Rust dependency integration; mature state/PTY foundation; small adapter boundary | Graphics extensions may exceed exposed engine hooks; cell model has Unicode limitations | Default |
| Fork the complete Alacritty application | Existing window, input and renderer integration | Ongoing fork reconciliation; multi-pane frontend changes may be invasive | Consider if the rendering spike shows excessive integration cost |
| Zig frontend + `libghostty-vt` | Direct fit to an available Zig engine; potential richer protocol base | Different toolchain; evolving library API; engine still needs frontend and renderer | Strong alternate if required capabilities block Alacritty |
| Rust frontend + Ghostty C API | Rust UI with a compiled Zig engine | FFI, bindings, Zig build and revision pinning | Only after a concrete capability experiment |
| New parser/grid in Rust/C/C++/Zig | Full control | Large compatibility and maintenance burden before useful UI | Reject for this project |
| C/C++ frontend using an existing engine | Native performance possible | Memory/lifetime discipline and extra integration choices | Feasible; no demonstrated benefit over the selected Rust path |

These costs are our engineering assessments, not statements established by upstream benchmarks. Rust is selected for explicit ownership, strong tooling and the direct engine integration; it does not automatically produce fewer allocations, lower startup time or faster glyph work than C or Zig.

## Capabilities actually exposed

`alacritty_terminal` documents terminal state, renderable content, damage, selection/search facilities, modes, PTY modules and an event loop [S04, S07–S11]. Its events include PTY replies, clipboard requests, title changes and wakeups [S10]. The UI must route and apply policy to those events; embedding the engine does not automatically complete input handling or desktop clipboard integration.

The documented mode flags include application cursor/keypad, bracketed paste, mouse/focus modes and kitty keyboard flags [S11]. Nora still needs the actual key encoder and tests. Do not assume that a mode flag alone means the frontend correctly implements the protocol.

`winit` supplies window creation, event handling and scale information [S12]. `wgpu` supplies graphics APIs, not terminal layout [S13]. `cosmic-text` offers font discovery, fallback, shaping and rasterization [S14]. `glyphon` caches/render glyphs using these libraries [S15]. Their generic text APIs must be adapted to a fixed terminal grid; independently selecting every newest version is unsafe.

## Early experiments that decide feasibility

1. **Compile and pin the stack:** window + surface + engine + font lookup in a release build. Check exact APIs and duplicate dependency versions.
2. **Engine contract:** replay split UTF-8/control sequences, alternate-screen input and DSR replies; extract owned visible cells without locking during font/GPU work.
3. **Fixed-cell rendering:** ASCII, combining marks, CJK, box drawing and Ethiopic fallback at multiple DPI values. Cursor and selection stay aligned with engine columns.
4. **I/O bounds:** inspect the pinned event loop's queue and pending-write semantics. Current upstream source uses `std::sync::mpsc` and a pending-write queue [S22]; wrapping its sender does not establish end-to-end byte limits. Verify the chosen revision, then reuse it only if limits are demonstrable, or adapt its I/O loop behind nora's session boundary.
5. **Idle/presentation:** redraw on demand, handle surface errors, and measure startup and idle wakeups on actual hardware.

Each experiment is a small task with evidence, a commit and a push. Failed experiments trigger a targeted ADR, not an automatic restart of the entire project.

## Where efficiency can come from

Proposed design measures: short terminal-lock duration; pooled visible-cell snapshots; cached font/glyph work; retained row geometry; batched GPU draws; coalesced redraws; bounded parsing turns; lossless input with backpressure; and sleeping while idle. Every optimization must have a relevant benchmark and a correctness check.

Not every parsed screen state needs to be displayed during a flood. All accepted bytes must still be processed. Damage-based CPU work does not imply only damaged pixels are drawn to an acquired swapchain image: a fresh frame must remain visually complete unless the renderer explicitly manages a retained offscreen target.

Use a portable default release build before testing LTO, different allocators, CPU-specific optimization or custom atlases. Cross-GPU behavior, compositor scheduling and font coverage can dominate individual microbenchmarks. The eventual optimization order should follow nora's own profile, not an assumed language advantage.

## Feasibility and scope

A staged daily-use Linux terminal is feasible with this foundation. Exact kitty feature parity is a substantially larger project: modern keyboard behavior, graphics, IME, Unicode, shell integration, multi-window lifecycle and packaging each require dedicated work. No delivery date or speedup is claimed.

The plan defines v0.1 as a dependable terminal with tabs and splits, and v0.2 as a route to richer kitty-style protocols. If full graphics support becomes a day-one requirement, run the engine-extension gate before frontend implementation and reconsider the default engine.

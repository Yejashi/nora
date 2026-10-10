# Ordered implementation roadmap

All cards begin unimplemented. Follow dependencies; do not jump into extensions while v0.1 is unfinished. Each completed feature/slice is tested, committed, pushed and remotely verified before another feature starts.

## Milestones and gates

| Milestone | Cards | Gate |
| --- | --- | --- |
| Foundation | N00–N04 | Reproducible stack, fixed cells, engine semantics, bounded I/O proof |
| Working terminal slice | N05–N12 | Real shell/input/rendering/resize/exit with tests |
| Daily-use single pane | N13–N22 | Text, history, clipboard/mouse/paste, config/search/modern keyboard/IME |
| Workspace | N23–N25 | Tabs/splits, correct routing, complete resource audit |
| Performance and distribution | N26–N30 | Correct measurements, targeted optimization, identity and packaging |
| v0.1 assessment | N31 | Full product/native/manual/performance gates; honest release decision |
| Kitty-style graphics | N32–N34 | Engine hook gate, bounded subset, capability-by-capability expansion |
| Other later features | N35–N36 | Opt-in shell integration; owner-selected future platform work |

N02 and N03 may be tackled in either order after N01, but a single agent still completes and pushes one increment before starting another. The table is not an instruction to spawn other agents. N35 is independent of graphics once v0.1 is assessed. N36 is a planning placeholder until a specific future goal is selected.

If a gate fails, record evidence and resolve the smallest blocking dependency/ADR before continuing downstream work. If a large card needs slices, add Nxxa/Nxxb to progress with explicit acceptance and dependency; never use the card's broad scope as an excuse for a giant change.

## Task index

| ID | Feature | Dependencies |
| --- | --- | --- |
| [N00](tasks/N00-bootstrap.md) | Repository and minimal build scaffold | none |
| [N01](tasks/N01-dependencies.md) | Compile and pin the graphics/engine tuple | N00 |
| [N02](tasks/N02-text-spike.md) | Prove fixed-cell text placement | N01 |
| [N03](tasks/N03-engine-adapter.md) | Headless terminal adapter and replay tests | N01 |
| [N04](tasks/N04-bounded-io.md) | Resolve end-to-end I/O and event bounds | N03 |
| [N05](tasks/N05-pty-session.md) | Real PTY shell session | N04 |
| [N06](tasks/N06-engine-events.md) | Route replies and UI requests | N05 |
| [N07](tasks/N07-snapshots.md) | Owned visible snapshots and damage | N06 |
| [N08](tasks/N08-ascii-render.md) | Live ASCII/color/cursor rendering | N02, N07 |
| [N09](tasks/N09-text-input.md) | Basic text and control input | N08 |
| [N10](tasks/N10-navigation-input.md) | Application-mode navigation and keypad | N09 |
| [N11](tasks/N11-resize.md) | Window/DPI to grid and PTY resize | N10 |
| [N12](tasks/N12-child-exit.md) | Child exit, final output and pane close | N11 |
| [N13](tasks/N13-unicode-styles.md) | Unicode, fallback and text styles | N12 |
| [N14](tasks/N14-selection.md) | Scrollback and correct selection/copy text | N13 |
| [N15](tasks/N15-clipboard.md) | Native Wayland and X11 clipboard | N14 |
| [N16](tasks/N16-mouse-focus.md) | Mouse reports and focus behavior | N15 |
| [N17](tasks/N17-paste.md) | Bounded ordinary and bracketed paste | N16 |
| [N18](tasks/N18-configuration.md) | Validated TOML and CLI precedence | N17 |
| [N19](tasks/N19-config-reload.md) | Transactional config reload and zoom | N18 |
| [N20](tasks/N20-search.md) | Bounded scrollback search | N19 |
| [N21](tasks/N21-kitty-keyboard.md) | Negotiated modern keyboard encoding | N20 |
| [N22](tasks/N22-ime.md) | Native IME and composition UI | N21 |
| [N23](tasks/N23-tabs.md) | Tabs and active-session routing | N22 |
| [N24](tasks/N24-splits.md) | Split panes and deterministic layout | N23 |
| [N25](tasks/N25-resource-audit.md) | Enforce and document all growth limits | N24 |
| [N26](tasks/N26-benchmark-harness.md) | Rust benchmarks and structured diagnostics | N25 |
| [N27](tasks/N27-damage-optimization.md) | Optimize measured snapshot/glyph/geometry costs | N26 |
| [N28](tasks/N28-scheduling.md) | Multi-pane fairness and presentation latency | N27 |
| [N29](tasks/N29-terminfo.md) | Validated terminal identity and SSH fallback | N28 |
| [N30](tasks/N30-packaging.md) | Clean-build CI and Linux install artifacts | N29 |
| [N31](tasks/N31-v01-acceptance.md) | Daily-use v0.1 release assessment | N30 |
| [N32](tasks/N32-graphics-hook.md) | Feasibility of graphics engine hooks | N31 |
| [N33](tasks/N33-graphics-basics.md) | Bounded inline images and placement | N32 |
| [N34](tasks/N34-graphics-expansion.md) | Expand graphics protocol by capability | N33 |
| [N35](tasks/N35-shell-integration.md) | Opt-in prompt markers and hyperlink UX | N31 |
| [N36](tasks/N36-future-platforms.md) | Later windows, accessibility and ports | N31 |

## Definition of completed feature change

The observable behavior works; meaningful relevant checks ran; docs/progress reflect actual state; the diff is scoped; a commit exists; the configured remote contains it. Record unavailable release-only hardware/manual tests separately. A push failure leaves the feature committed/unpushed and blocks the next feature.

## Do not preload

Read this index/status to choose work, then the selected card and its reference sections. The whole pack is a reference library for a 128k local agent, not a prompt to load in full. Preserve context for debugging and exact source APIs.


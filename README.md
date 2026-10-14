# nora

A fast, efficient, maintainable desktop terminal for Linux, written in Rust,
with native Wayland and X11 support.

## v0.1 Release Notes

All 30 feature cards (N00-N30) are complete: terminal engine, PTY sessions,
windowed and headless rendering, tabs/splits, search, selection, clipboard,
configuration, benchmarks and release packaging. See
[docs/13-v0.1-acceptance.md](docs/13-v0.1-acceptance.md) for the v0.1
acceptance assessment (compatibility, performance, resources, known
limitations).

Research snapshot: 10 October 2026. This is an implementation plan, not a working terminal or a benchmark result.

**Goal:** build `nora`, a fast, efficient, maintainable desktop terminal in Rust, with no Python dependency in its application, build scripts, or project-owned development tools. Start on Linux with native Wayland and X11. Deliver a reliable daily-use terminal before expanding toward kitty-style features.

**Recommended foundation:** `alacritty_terminal` for terminal semantics and PTY integration; `winit` for windows and input; `wgpu` for graphics; `cosmic-text` for font discovery, fallback and rasterization. Evaluate `glyphon` behind a rendering adapter, but do not mistake paragraph layout for terminal cell layout. Resolve dependency compatibility and bounded I/O in small, executable spikes.

This recommendation is an engineering judgment based on the linked primary sources. It does not establish that Rust or nora will outperform kitty. Kitty already places performance-sensitive work in C.

## Put this pack in an empty repository

Extract the archive directly into the repository root. It supplies `AGENTS.md`, this file, the initial prompt, documentation, task cards and status templates. It deliberately supplies no speculative Cargo workspace or uncompiled application code. The agent creates those incrementally.

Before implementation, configure the repository's intended Git remote and working branch, and make sure the agent's environment can authenticate to push. The agent must inspect the existing Git state and preserve any user changes. It must not invent a remote, change Git identity, or embed credentials.

## Start the agent

Use the complete message in [INITIAL_AGENT_PROMPT.md](INITIAL_AGENT_PROMPT.md). The agent should first read [AGENTS.md](AGENTS.md), [the workflow](docs/08-agent-workflow.md), [the status file](docs/status/PROGRESS.md), and the next task card. It should then implement one small increment, validate it, commit it, and push it before taking the next task.

On the Tellico cluster, run it as a single agent from the repository root: `opencode-tellico 0 --solo --steps 100000`, at the default medium thinking (`--steps` lifts the 200-step turn limit so an unattended session does not stop and wait); use `/think-xhigh` for an individual hard design or debugging question rather than a whole session. The default orchestrate lead cannot run `cargo` or commit, and the roadmap is sequential, so solo is the fit.

**A completed feature always gets a commit and push.** An authentication or remote failure is a reported blocker, not permission to claim completion or continue piling up unpushed features. Do not force-push.

## Reading map

| Document | Purpose | Read when |
| --- | --- | --- |
| [01-research.md](docs/01-research.md) | Evidence, alternatives, recommended approach | Planning or challenging the foundation |
| [02-product.md](docs/02-product.md) | What nora ships and what it defers | Before feature work |
| [03-architecture.md](docs/03-architecture.md) | Module ownership, concurrency, snapshots, resource limits | Before core or session changes |
| [04-performance.md](docs/04-performance.md) | Metrics, provisional goals and benchmark design | Instrumentation or optimization |
| [05-rendering.md](docs/05-rendering.md) | Fixed-cell fonts, caching, damage and GPU lifecycle | Rendering changes |
| [06-compatibility.md](docs/06-compatibility.md) | Protocol contract and input behavior | Terminal, input or clipboard changes |
| [07-build-release.md](docs/07-build-release.md) | Dependencies, build, CI and packaging | Bootstrap or release work |
| [08-agent-workflow.md](docs/08-agent-workflow.md) | Small-context workflow and commit/push requirements | Every session |
| [09-validation.md](docs/09-validation.md) | Correctness and integration tests | Every feature's test selection |
| [10-roadmap.md](docs/10-roadmap.md) | Ordered feature cards and release gates | Selecting work |
| [11-risks.md](docs/11-risks.md) | Failure modes and decision triggers | A spike fails or scope changes |
| [12-sources.md](docs/12-sources.md) | Primary-source links and evidence boundaries | Verifying upstream claims |
| [13-extensions.md](docs/13-extensions.md) | Graphics, shell integration and later features | After the v0.1 release gate |
| [status/PROGRESS.md](docs/status/PROGRESS.md) | Durable implementation state | Every session |
| [status/HANDOFF_TEMPLATE.md](docs/status/HANDOFF_TEMPLATE.md) | Short recovery checkpoint | Ending or resetting context |

## Success means

Nora runs real shells and TUIs correctly; remains interactive while another pane produces output; has useful tabs, splits, search, selection and configuration; uses bounded resources; and has reproducible measurements on actual Linux hardware. Builds and tests are reproducible from Git. Limitations are documented honestly, and no feature is marked shipped merely because a window opens.

The task cards are deliberately narrower than milestones. If a card still exceeds a manageable context slice, split it into separately useful, tested increments, with a commit and push for each.

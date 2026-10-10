# Risks and decision triggers

| Risk | Early signal | Response/gate |
| --- | --- | --- |
| Agent API hallucination | Code copied from incompatible `latest` examples | Compile minimal pinned examples; record tuple; inspect exact docs |
| Too much work per context | Whole milestone rewrite or repeated loss of state | Split card; checkpoint; read targeted files; commit/push useful slices |
| Generic text layout breaks cells | CJK/combining/cursor alignment drift | N02 fixed-cell gate; replace only the text adapter if needed |
| Engine width limitations | ZWJ/complex scripts mis-modeled despite correct rasterization | Document exact limits; reconsider engine only if required scope demands it |
| Unbounded downstream queue | Bounded sender forwards into unchecked pending writes | N04 validates accounting through completed writes; scoped I/O adaptation |
| Parser callback deadlock | Reply or clipboard callback waits while engine lock held | Nonblocking policy/ordered writer; pause bounded input or fail visibly |
| Lost damage | Dropped frame/delta clears an update that never presents | Retain coherent latest snapshot; use revisions; full rebuild on mismatch |
| GPU blocks UI | Input stalls in surface acquisition/shaping | Instrument; dedicated render thread only after a measured trigger |
| Idle CPU/GPU waste | Poll loop or perpetual redraw | On-demand events, finite timers, focused blink only |
| History is only row-bounded | Combining/link extras grow despite fixed rows | Audit/patch caps; distinguish actual bounds from estimates |
| Cache eviction corrupts text | UV handles survive an atlas reuse | Generations and frame-safe invalidation; tiny-atlas regression |
| Slow paste monopolizes writer | Query/shutdown starved or input reordered | Transactional bounded paste; reserved control capacity; stress tests |
| Wayland only works via XWayland | Wrong clipboard/window backend | Native backend diagnostics and separate compositor tests |
| Reflow/search races | Stale indexes select wrong text after output/resize | Generation-tagged snapshots/results, cancel/restart or invalidate visibly |
| Child/PTY leak | Exited pane leaves zombies/threads | Explicit close/reap state machine; finite drain; repeated spawn/close test |
| Graphics requires invasive parser changes | No APC hook or limits in current engine | N32 hook gate; minimal upstream patch or ADR engine reconsideration |
| Dependency/toolchain churn | Duplicate wgpu types/MSRV mismatch | Cargo.lock/toolchain pin; update one compatible tuple per increment |
| Feature drift | Plugins/shaders/AI added before usable terminal | Enforce v0.1 acceptance and staged roadmap |
| Performance marketing outruns evidence | Parser speed described as physical latency | Separate benchmark layers; raw data and machine metadata |
| Unpushed features accumulate | Agent continues after failed push | Preserve validated commit; stop new feature work until push fixed |
| Accessibility omitted | Visual renderer has no accessible text interface | Document limitation; later integration gate; no parity claim |

## Decision process

Use the smallest intervention that solves the evidenced problem. An engine integration bug does not justify replacing the engine. A text placement failure does not justify replacing winit. A surface latency profile may justify a rendering worker; a throughput benchmark alone does not.

An ADR states the failing test/profile, exact dependency revision, alternatives, decision, maintenance cost and acceptance proving the solution. Update related tasks and scope in the same decision increment. Every completed decision/change is committed and pushed.

No engine, language or toolkit change is required merely because it is fashionable or an external benchmark ranks it highly. If no supported exposed APIs satisfy required bounds/protocols, document that obstacle early and finish the capability experiment before expanding the frontend.

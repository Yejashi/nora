# N27: Optimize measured snapshot/glyph/geometry costs

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N26.
- Read: [04-performance.md](../04-performance.md), [05-rendering.md](../05-rendering.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Improve a measured hot path while keeping coherent visual state.

## Small implementation increment

Profile current renderer/adapter and select one concrete cost. Prefer retained rows/pools/cache reuse before complex GPU partial redraw. Compare identical workloads before/after. Validate dirty revisions, atlas rebuild, selection/cursor and full-frame coherence. A separate optimization gets a separate commit.

If the listed work needs multiple concepts/commits, create explicit N27a/N27b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Relevant measured cost improves beyond noise or a justified tradeoff is documented. State/geometry/image tests remain unchanged. Engine damage cannot be lost when frames are superseded or skipped.

## Validation

Before/after raw samples/profile; lost-redraw, palette/atlas/font/viewport invalidation tests; idle draw count and warm-cache allocation check.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `perf(render): reduce measured frame preparation cost (N27)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

If profile shows no meaningful issue, document the measured decision instead of inventing an optimization.


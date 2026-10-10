# N07: Owned visible snapshots and damage

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N06.
- Read: [03-architecture.md](../03-architecture.md), [05-rendering.md](../05-rendering.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Provide coherent visible screen data to rendering without engine lifetimes or long-held locks.

## Small implementation increment

Create pooled owned snapshots with text/extras/styles/width/cursor/colors/modes/epochs. Capture damage and add UI invalidation. Retain a complete latest visible state; reset engine damage only after capture. Add coalesced dirty-generation wakeups with clear/recheck logic.

If the listed work needs multiple concepts/commits, create explicit N07a/N07b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

No engine borrow survives lock release. History size does not change the per-frame copy range. Dropped/superseded frame requests cannot lose dirty content. UI overlays invalidate correctly.

## Validation

Replay mutations between snapshots, lost redraw/coalescing races, viewport change and cursor/selection updates. Track visible cell count and allocations after warmup.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(engine): publish coherent visible snapshots (N07)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not clone whole history or queue one snapshot per read.


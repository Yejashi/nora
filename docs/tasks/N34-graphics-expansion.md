# N34: Expand graphics protocol by capability

Implementation status: complete (N34a formats f=24/f=32 + N34b chunk assembly; see `docs/status/PROGRESS.md` for evidence).

- Dependencies: N33.
- Read: [13-extensions.md](../13-extensions.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Grow the image implementation only through independently tested capabilities.

## Small implementation increment

Add chunk assembly, additional formats, multiple placements and richer delete/query behavior one at a time. Reassess budgets/cache policy. Consider advanced shared-memory/file/animation/placeholder features only when their threat/resource/transport contracts are designed and needed.

If the listed work needs multiple concepts/commits, create explicit N34a/N34b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Each new capability has exact advertised scope, positive/negative tests and accounting. Old uploads/placements still work. The documented subset and benchmark results match the actual implementation.

## Validation

Per-capability protocol fixtures, cache/decode/placement stress, client integration and memory/presentation workloads.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(graphics): extend one validated capability (N34)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

No automatic file opens from untrusted shell-supplied paths; advanced transports remain off until explicitly designed.


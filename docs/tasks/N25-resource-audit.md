# N25: Enforce and document all growth limits

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N24.
- Read: [03-architecture.md](../03-architecture.md), [09-validation.md](../09-validation.md), [11-risks.md](../11-risks.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Turn resource-limit proposals into demonstrated limits or explicit blocking defects.

## Small implementation increment

Audit history/extra cells/control strings/hyperlinks/titles, font/glyph caches, accepted+pending bytes, protocol events, paste, snapshots and jobs. Account actual allocated pages and exceptional engine growth. Use supported caps or a minimal pinned patch; reduce configurable limits safely with geometry changes.

If the listed work needs multiple concepts/commits, create explicit N25a/N25b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Blocked writers, query floods and glyph/history stress cannot exceed enforced budgets. Capacity cleanup is correct after close/cancel. Any unavoidable library exception is specifically reported and release-blocking until resolved, not disguised as a size estimate.

## Validation

Tiny-cap stress, large combining/control payloads, unique-glyph churn and repeated sessions; memory accounting/RSS plateau with declared tolerances; resource policy documentation.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `fix(resources): enforce audited memory and queue limits (N25)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not declare a strict bound solely from rows multiplied by size_of Cell.


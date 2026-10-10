# N06: Route replies and UI requests

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N05.
- Read: [06-compatibility.md](../06-compatibility.md), [03-architecture.md](../03-architecture.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Connect engine replies and policy events without deadlocks or duplicate PTY writes.

## Small implementation increment

Route PtyWrite and formatted color/size replies to the single ordered writer. Coalesce replaceable title/bell events and enforce payload caps. Apply disabled-by-default OSC clipboard policy. Ensure callbacks only enqueue bounded requests and never call desktop APIs while locked.

If the listed work needs multiple concepts/commits, create explicit N06a/N06b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

DSR/color/size queries receive correct current values; title changes reach UI state; OSC requests follow default policy; a query flood cannot deadlock or allocate beyond audited limits.

## Validation

Real-PTY query tests with exact replies and timeout; queue-full event tests; policy tests; stale generation tests.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(engine): route terminal replies and policy events (N06)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Make reply formatting/geometry source explicit; do not hardcode guessed terminal answers.


# N04: Resolve end-to-end I/O and event bounds

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N03.
- Read: [03-architecture.md](../03-architecture.md), [11-risks.md](../11-risks.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Prove the writer and protocol callbacks stay bounded under blocked-child and query-flood conditions.

## Small implementation increment

Inspect the pinned worker's channel/pending-write behavior. If no drain acknowledgements/caps exist, adapt the smallest upstream I/O loop using published PTY/engine APIs, with provenance. Required slices: readiness/public-API spike; byte reservation/pending writer; parser callback and shutdown recovery. Each useful slice compiles, is tested, committed and pushed.

If the listed work needs multiple concepts/commits, create explicit N04a/N04b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

A fake or real blocked writer cannot exceed declared queued+pending bytes. Capacity is returned on completed writes/cancel/exit. Replies do not block under engine lock. Shutdown remains serviceable. No hidden unbounded downstream queue is accepted as bounded.

## Validation

Use very small caps; force partial writes/WouldBlock, cancellation, queue-full replies and close. Trace byte accounting and ordering. Write an ADR for any patch.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(session): enforce bounded I/O accounting (N04)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not replace VT parsing. If public hooks cannot support the contract, resolve that blocker before frontend expansion.


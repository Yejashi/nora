# N20: Bounded scrollback search

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N19.
- Read: [03-architecture.md](../03-architecture.md), [09-validation.md](../09-validation.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Search history without freezing the UI or using stale coordinates.

## Small implementation increment

Start with literal search using engine search facilities or bounded chunk snapshots; add a small query/next/previous UI and highlight overlay. Define behavior when output/resize changes history. Use generation-tagged jobs and bounded pending requests; discard/cancel stale results. Regex is optional after literal correctness.

If the listed work needs multiple concepts/commits, create explicit N20a/N20b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Search finds known wrapped/wide-text fixtures; next/previous navigation moves the viewport correctly; rapid query changes do not resurrect old results; history under continuous output is usable.

## Validation

Fixed-history expected-match fixtures; trim/reflow/cancellation and stale-result tests; output+search lock-time measurement.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(search): add responsive history search (N20)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Never copy unlimited history under a long-held terminal mutex.


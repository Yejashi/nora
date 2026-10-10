# N32: Feasibility of graphics engine hooks

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N31.
- Read: [13-extensions.md](../13-extensions.md), [11-risks.md](../11-risks.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Resolve whether bounded graphics parsing can be integrated without a parallel VT parser.

## Small implementation increment

Inspect pinned public hooks for APC/chunk/reply handling and implement the smallest synthetic payload experiment. Verify ordinary text before/after, chunking and accumulation limits. If hooks are missing, produce a scoped patch or migration ADR with compiled evidence before implementing images.

If the listed work needs multiple concepts/commits, create explicit N32a/N32b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

A supported path can deliver bounded graphics commands/replies while preserving terminal byte semantics, or a specific technical blocker and chosen next intervention are documented.

## Validation

Hook replay/chunk/oversize/recovery tests; compile/link patch/alternative if needed; maintain provenance and dependency revision.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `chore(graphics): validate bounded protocol hooks (N32)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not equate the full Ghostty application's graphics support with its exposed VT API.


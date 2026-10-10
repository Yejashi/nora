# N36: Later windows, accessibility and ports

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N31.
- Read: [13-extensions.md](../13-extensions.md), [03-architecture.md](../03-architecture.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Investigate and stage future requirements without destabilizing the Linux release.

## Small implementation increment

Choose one owner-requested feature at a time: multiple OS windows, accessible terminal-text exposure, or a native platform port. Write a feasibility ADR and separate task cards with platform-specific acceptance. A remote-control server is independently scoped with permissions/authentication/limits.

If the listed work needs multiple concepts/commits, create explicit N36a/N36b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

The selected investigation has executable evidence, accurate limitations and small follow-up cards. Any implemented platform feature has its own tests, commit and push. No broad compatibility claim follows from cross-compilation alone.

## Validation

Platform-specific smoke and lifecycle tests; accessibility behavior with actual tools if pursued; documented prerequisites and pending hardware.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `chore(platform): investigate one future integration (N36)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not treat these optional explorations as already authorized implementation of an arbitrary daemon or external release.


# N03: Headless terminal adapter and replay tests

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N01.
- Read: [03-architecture.md](../03-architecture.md), [06-compatibility.md](../06-compatibility.md), [09-validation.md](../09-validation.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Wrap terminal state with a narrow, tested engine boundary.

## Small implementation increment

Implement constructor/feed-for-test/mode inspection/owned state extraction around pinned alacritty_terminal APIs. Keep parser state across chunks and route events to a test sink. Create the compatibility status matrix. Use the engine's parser/state; no independent escape scanner.

If the listed work needs multiple concepts/commits, create explicit N03a/N03b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Independent fixtures prove overwrite/wrap/SGR/alternate-screen and UTF-8 split invariance. Expected reply events are captured in order. Engine-specific types stay within adapter/session synchronization boundaries.

## Validation

Run short fixtures at every split boundary and seeded random chunking; compare cursor/cells/modes/replies to explicit expected results.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(engine): add tested terminal adapter (N03)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

This is headless semantics, not desktop input or rendering.


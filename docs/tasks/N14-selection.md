# N14: Scrollback and correct selection/copy text

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N13.
- Read: [06-compatibility.md](../06-compatibility.md), [03-architecture.md](../03-architecture.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Let users navigate history and extract engine-correct selected text.

## Small implementation increment

Connect engine scroll/selection operations to local UI; highlight selected ranges; handle real vs soft-wrapped lines, wide spacers and combining extras. Keep selection stable or visibly invalidate under history trim/reflow. Add local copy action returning text for the platform boundary.

If the listed work needs multiple concepts/commits, create explicit N14a/N14b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

History scrolling works without copying all history per frame. Selection/copy preserve original text and expected newlines. Resize/history trim cannot copy unrelated stale rows.

## Validation

Small-width wrapped selection fixtures; wide/combining/trim/reflow cases; viewport/cursor overlay invalidation; live mouse selection.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(selection): add scrollback and text selection (N14)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Clipboard transport is a separate card; do not mix platform ownership into engine tests.


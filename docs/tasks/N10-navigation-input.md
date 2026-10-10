# N10: Application-mode navigation and keypad

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N09.
- Read: [06-compatibility.md](../06-compatibility.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Correctly encode non-text keys according to current terminal modes.

## Small implementation increment

Implement arrows/Home/End/Insert/Delete/function keys and keypad as narrow table-driven slices. Read only the mode bits needed; keep encoder pure. Verify modifier cases and Backspace/terminfo consistency. Update compatibility entries with exact supported keys.

If the listed work needs multiple concepts/commits, create explicit N10a/N10b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Normal/application arrows differ correctly; supported navigation and keypad fixtures match independently specified protocol expectations. Editing and a full-screen TUI remain usable.

## Validation

Pure mode/key/modifier byte tables; run vim/less or a neutral Rust key inspector; record actual external versions.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(input): encode mode-aware navigation (N10)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Split function-key and keypad work if they would turn this into a broad encoder rewrite.


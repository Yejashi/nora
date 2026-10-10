# N09: Basic text and control input

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N08.
- Read: [06-compatibility.md](../06-compatibility.md), [09-validation.md](../09-validation.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Make the live shell usable with ordinary text and controls.

## Small implementation increment

Normalize winit text/layout events, add basic enabled local binding precedence, encode Enter/Tab/Ctrl combinations and enqueue accepted bytes. Establish one documented Backspace choice. Keep physical-key bindings separate from layout-generated text. Avoid IME duplication even before the full preedit UI.

If the listed work needs multiple concepts/commits, create explicit N09a/N09b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Typing/editing shell commands works; Ctrl+C reaches terminal semantics; disabled local bindings pass through. Accepted input is ordered and full capacity never blocks the UI.

## Validation

Exact byte unit tables plus PTY shell-helper tests; non-US/dead-key cases where available; input under a slow child.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(input): send text and control keys (N09)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not assert interrupts arise from directly sending signals; normal child termios handles control bytes.


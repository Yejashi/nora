# N15: Native Wayland and X11 clipboard

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N14.
- Read: [06-compatibility.md](../06-compatibility.md), [07-build-release.md](../07-build-release.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Connect user copy/paste to the actual native clipboard backend.

## Small implementation increment

Verify handle/API compatibility and use a Wayland helper on native Wayland plus an X11 backend on X11. Keep operations asynchronous/bounded, preserve ownership lifetime and diagnose unavailable services. Implement default clipboard text retrieval without yet adding large paste transactions.

If the listed work needs multiple concepts/commits, create explicit N15a/N15b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Copy transfers exact selected text to another application; paste obtains text; native backend is confirmed. Default OSC 52 reads/writes remain disabled; no X11 fallback silently masquerades as Wayland.

## Validation

Native ownership/focus/lifetime/manual tests for each backend; platform adapter tests; unavailable-service and payload-cap tests.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(platform): integrate native clipboard (N15)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

If no native display exists, record manual tests pending and do not claim clipboard validation.


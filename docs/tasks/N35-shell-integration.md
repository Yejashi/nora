# N35: Opt-in prompt markers and hyperlink UX

Implementation status: complete (N35a `6fa16bb`+`4edfffb`, N35b `797d7a2`+`65d0f0b`, N35c `6a892bb`, N35d `d90f5e7`; see `docs/status/PROGRESS.md` for evidence).

- Dependencies: N31.
- Read: [13-extensions.md](../13-extensions.md), [06-compatibility.md](../06-compatibility.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Add useful navigation metadata without changing shells implicitly.

## Small implementation increment

Implement one supported shell integration script/marker hook first; then prompt navigation, previous-command selection and optional cwd tracking as separate features. Add explicit-user hyperlink activation with allowed schemes and direct opener argv. Preserve normal operation without integration.

If the listed work needs multiple concepts/commits, create explicit N35a/N35b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Opt-in supported shell produces usable markers; no printed-prompt scraping or automatic sourcing occurs. Hyperlinks need a user action and cannot execute shell interpolation. Missing markers fall back cleanly.

## Validation

Synthetic marker/unsupported-shell tests; supported-shell manual run; navigation across scrollback trim; malicious/unsupported URL scheme tests.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(shell): add opt-in prompt navigation (N35)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Treat remote cwd/link strings as metadata, not authorization to read files or spawn commands.


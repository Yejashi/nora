# N30: Clean-build CI and Linux install artifacts

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N29.
- Read: [07-build-release.md](../07-build-release.md), [02-product.md](../02-product.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Make nora reproducible and installable from a clean repository.

## Small implementation increment

Finish headless CI, explicit backend configurations and display smoke jobs where supported. Add desktop entry/icon/config example/notices and one verified Linux install recipe. Preserve user config. Build portable release artifacts and diagnostics. Keep packaging and CI improvements in separate completed increments where useful.

If the listed work needs multiple concepts/commits, create explicit N30a/N30b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Clean checkout builds/tests; desktop launch uses nora; config checking needs no display. Install/uninstall does not erase user data. Native system dependencies and actual CI/display coverage are documented.

## Validation

Clean-checkout build/install smoke; desktop entry validation if available; notices/sample config checks; both native backend smoke results or explicit pending release checks.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `chore(release): package reproducible Linux builds (N30)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not publish or tag an external release without the owner's release authorization.


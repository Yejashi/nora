# N01: Compile and pin the graphics/engine tuple

Implementation status: validated (10 October 2026). See `docs/status/PROGRESS.md` for evidence.

- Dependencies: N00.
- Read: [01-research.md](../01-research.md), [07-build-release.md](../07-build-release.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Prove that one compatible dependency set can create a real Linux window and GPU surface.

## Small implementation increment

Compile a minimal winit/wgpu window lifecycle with the terminal and font dependencies linked. Verify feature flags, system dependencies and exact APIs. Commit Cargo.lock and resolved version/toolchain notes. Exercise resize/minimize once. Keep engine/font test code small and useful for later integration.

If the listed work needs multiple concepts/commits, create explicit N01a/N01b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

The candidate tuple compiles in release mode; opens a window on an available backend; reports backend/device metadata; closes cleanly. Native Wayland and X11 tests are recorded separately, with unavailable environments pending.

## Validation

cargo tree -d; standard build checks; live open/close smoke where a display exists. Do not report a virtual X11 run as native Wayland.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `chore(deps): pin compatible terminal graphics stack (N01)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

No independent newest-version selection or unverified source-branch dependency.


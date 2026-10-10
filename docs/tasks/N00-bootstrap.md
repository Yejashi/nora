# N00: Repository and minimal build scaffold

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: none.
- Read: [07-build-release.md](../07-build-release.md), [08-agent-workflow.md](../08-agent-workflow.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Establish a reproducible, minimal Rust project and a verified Git workflow before any application work.

## Small implementation increment

Inspect owner changes, branch and configured remote. Initialize only if needed. Add the smallest application manifest, pinned toolchain, .gitignore and a CLI reporting nora's name/version; no fake terminal UI. Add headless build/format CI only if the configured repository supports it. Record the intended remote/branch without secrets.

If the listed work needs multiple concepts/commits, create explicit N00a/N00b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

A clean checkout builds the minimal binary and nora --version succeeds. Project-owned files require no Python. Existing user files remain unchanged. The first commit is pushed and the remote result verified, or a specific remote/auth blocker is recorded.

## Validation

Run formatting/check/build and the CLI. Inspect the staged diff. If the remote is absent, preserve the validated bootstrap commit and report committed/unpushed; do not invent a remote.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `chore(repo): bootstrap nora build (N00)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not scaffold unused modules or pretend this creates a terminal.


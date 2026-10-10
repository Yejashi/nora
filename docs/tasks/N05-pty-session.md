# N05: Real PTY shell session

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N04.
- Read: [03-architecture.md](../03-architecture.md), [07-build-release.md](../07-build-release.md), [09-validation.md](../09-validation.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Run a real child process with correct PTY and shell semantics.

## Small implementation increment

Connect LaunchSpec/argv/cwd/env to the engine's PTY facility and selected bounded worker. Start one shell session and expose its state/events. Keep child/session ownership explicit. Add a Rust child helper for readiness and tagged I/O. Implement direct argv execution for -- COMMAND ARG....

If the listed work needs multiple concepts/commits, create explicit N05a/N05b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

A real child receives accepted bytes in order and output changes the engine. Exact argv/cwd work; failed launch cleans resources. Session events include generation identity. No graphical output is required yet.

## Validation

Headless real-PTY tests for echo, argv spaces/metacharacters, launch failure and stop-reading child. Use readiness handshakes/timeouts rather than sleep guesses.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(session): launch real PTY children (N05)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not spawn commands by shell-interpolating CLI strings.


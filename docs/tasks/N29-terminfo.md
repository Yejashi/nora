# N29: Validated terminal identity and SSH fallback

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N28.
- Read: [06-compatibility.md](../06-compatibility.md), [07-build-release.md](../07-build-release.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Advertise only validated terminal capabilities and document remote use.

## Small implementation increment

Decide dedicated nora terminfo vs tested xterm-256color fallback in an ADR. If shipping an entry, derive a minimal capability set from tested behavior, compile/install under user paths and preserve notices. Document COLORTERM and remote host install/fallback without claiming xterm-kitty.

If the listed work needs multiple concepts/commits, create explicit N29a/N29b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

tic/infocmp and curses/TUI tests pass for the chosen identity. An SSH session works with matched terminfo or explicit fallback. Capability entries do not advertise unsupported images or keys.

## Validation

Terminfo compile/readback and clean environment CLI/TUI tests; remote SSH check when available; matrix review against every advertised capability.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(compat): ship validated terminal identity (N29)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

No default unknown TERM on remote hosts without an explained installation/fallback path.


# N21: Negotiated modern keyboard encoding

Implementation status: done 2026-10-17 (all five flags; alacritty_terminal keeps the stack, `src/input/kitty.rs` encodes; evidence: `tests/kitty_keyboard.rs`, e2e `kitty` and `nvim`). The earlier N21a/N21b stack and encoder were removed (audit F5).

- Dependencies: N20.
- Read: [06-compatibility.md](../06-compatibility.md), [12-sources.md](../12-sources.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Make the supported kitty keyboard protocol work end to end.

## Small implementation increment

Verify engine mode/stack/query behavior, then implement frontend encoding in flag-sized increments. Include modifiers, repeat/release, functional keys and associated text as actually supported. Prefer an attributed, revision-pinned adaptation of a mature encoder over invented tables when appropriate.

If the listed work needs multiple concepts/commits, create explicit N21a/N21b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Default remains legacy. Negotiated key events/replies match supported flags; push/pop/query restore behavior; local bindings and IME text do not produce duplicates. Unsupported enhancements are documented honestly.

## Validation

Independent byte fixtures per flag and key/event type; neutral Rust key inspector; TUI integration; mode-stack boundary tests.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(input): support negotiated kitty keyboard modes (N21)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

One engine flag does not establish that all key encoding behavior is implemented.


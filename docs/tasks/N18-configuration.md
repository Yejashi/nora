# N18: Validated TOML and CLI precedence

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N17.
- Read: [02-product.md](../02-product.md), [07-build-release.md](../07-build-release.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Provide predictable configuration without code execution.

## Small implementation increment

Add typed defaults, XDG discovery, --config/--check-config/--working-directory and CLI precedence. Validate numeric limits, colors, keys and font settings; reject unknown fields with useful diagnostics. Document settings actually implemented. Split font/theme/key sections if needed.

If the listed work needs multiple concepts/commits, create explicit N18a/N18b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

--check-config runs without display/GPU and exits correctly. Invalid config does not start a shell. Defaults match documented UI behavior, and argv/environment configuration has no shell interpolation.

## Validation

Pure parse/default/range/unknown-field/precedence fixtures; headless CLI exit tests; startup with valid/invalid files.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(config): validate TOML configuration (N18)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not ship a sample full of settings the program ignores.


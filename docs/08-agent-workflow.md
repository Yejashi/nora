# Incremental agent workflow

The implementation agent has a 128k context window. Treat that as a ceiling, not a preload target. Small features, selective reads and durable state are required by the owner.

## Session entry

1. Read `AGENTS.md`, this workflow and `docs/status/PROGRESS.md`.
2. Inspect `git status --short`, `git branch --show-current`, `git remote -v` and the last few commits. Do not print embedded remote credentials.
3. If prior work is committed/unpushed, resolve/verify that push first. If work is uncommitted, recover its acceptance condition from the handoff and complete or report it before taking another task.
4. Choose the earliest ready task from [10-roadmap.md](10-roadmap.md). Read its card and only its listed documents/source files.
5. State the observable behavior, affected boundary and checks in a short working note. Do not rewrite the full project plan.

## Context budget

| Material | Suggested active budget |
| --- | --- |
| Rules/status/current card | 2k–5k tokens |
| Relevant architecture/protocol sections | 3k–8k |
| Relevant source/test excerpts | 4k–16k |
| Implementation and debugging | Leave substantial headroom |

Prefer `rg` and targeted ranges over whole-tree reads. Limit command output; save large traces outside tracked source, then inspect summaries and specific failures. Read source of pinned dependencies only around the APIs in question. Keep total context comfortably below the ceiling, and checkpoint when debugging begins displacing important decisions. Do not assume a fresh context remembers a successful manual test.

## How to size a feature

One feature should have one primary acceptance behavior and fit a short review. Examples: implementing ordinary arrows with mode tests; adding exit-drain handling; adding one pane split orientation; enforcing one real byte limit. A whole renderer, all keyboard protocols or all tabs/splits is too much for one increment.

Cards can be subdivided into IDs such as `N13a` and `N13b`. Add dependencies and acceptance to the roadmap/status before starting the subtask. A useful compiling spike is a completed chore and gets its own commit/push. Avoid placeholder traits, no-op methods or tests that only assert a mocked implementation matches itself.

## Mandatory commit and push loop

For every completed feature change:

1. Run the relevant deterministic regression checks and formatting/Clippy/build checks appropriate to the touched code.
2. Update tracked progress notes, task acceptance evidence, compatibility entries and behavior docs as needed. Include known manual-test limitations.
3. Inspect `git diff --check`, the actual diff and `git status --short`. Stage an explicit file list. Exclude unrelated user changes, credentials, large traces, generated build products and personal config.
4. Commit with a concise type/scope and feature ID, for example `feat(input): encode application arrows (N10)`.
5. Push that commit to the preconfigured intended remote branch. Prefer `git push` when the tracking branch is verified. If no tracking branch exists but the intended remote/branch are already known, use an explicit non-force push and establish tracking.
6. Verify remote state, not just the local commit. For the ordinary single-writer branch, use `git ls-remote --heads <remote> <branch>` and compare to local HEAD. If others have advanced the remote, fetch and verify the local commit is an ancestor of its head. Never force-push to make equality true.
7. Report the change, check results, commit ID and verified push status. Then take the next feature.

Do not wait until the entire milestone to commit or push. Do not merge several completed features into one broad commit. Bugfixes, necessary refactors, documentation and dependency pins are also durable completed increments and should follow this loop.

## Git failure handling

| Situation | Required action |
| --- | --- |
| Empty repository, no Git history | Initialize Git if appropriate; inspect owner-supplied branch/remote configuration |
| No remote or intended branch | Complete reviewable bootstrap work and validation; preserve local commit; report the exact missing setup; do not invent a URL or proceed through unrelated features |
| Authentication or network failure | Keep the commit; report committed/unpushed; retry after setup is corrected |
| Non-fast-forward | Fetch and inspect divergence; reconcile only understood changes without overwriting user work; never force-push |
| Unrelated dirty user files | Preserve them; explicitly stage only agent changes |
| Required GPU/manual test unavailable | Run independent checks; commit/push the implemented feature with pending test recorded; keep release blocked |
| Feature incomplete or tests failing | Do not claim completion; keep a detailed checkpoint; diagnostic WIP only on an established owner-approved development branch |

Do not change Git user identity, authenticate through invented secrets, publish a repository, change branch protections or delete remote branches as part of routine implementation. The user explicitly requires feature pushes to an intended configured repository, not arbitrary external publication.

## Progress and handoffs

Update `docs/status/PROGRESS.md` **before** the feature commit. It records `not_started`, `in_progress`, `validated`, or `blocked` implementation states and exact validation evidence. A commit cannot contain its own hash; the post-push report supplies that hash and push result. At the next feature's entry, verify the previous push and optionally record its hash as historical evidence in the next commit.

Use [HANDOFF_TEMPLATE.md](status/HANDOFF_TEMPLATE.md) for a compact handoff. Never carry a vague “almost done” across contexts. Include current files, exact failing command, accepted design, tests that actually ran, and one next action. A task is overall complete only when its required checks and Git push are satisfied; pending release-only manual checks stay visible separately.

## Changes to the plan

Routine implementation details can be resolved locally. For a major engine/renderer/threading change, write an ADR using [ADR_TEMPLATE.md](decisions/ADR_TEMPLATE.md), cite the failed gate/profile, state alternatives and define a test proving the change. Update relevant docs/task dependencies in the same small decision increment. Do not overwrite research observations with implementation guesses.

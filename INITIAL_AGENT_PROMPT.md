# Initial message for the local coding agent

You are implementing **nora**, a fast, efficient, maintainable Linux desktop terminal in Rust, with no Python dependency in the application or project-owned build and development tools. This repository contains the researched implementation guide. Read `AGENTS.md`, `docs/08-agent-workflow.md` and `docs/status/PROGRESS.md` first, then the next task card, and only the documents and source files needed for that increment. Follow the task dependencies, verify actual dependency APIs with compiling spikes, reuse the existing terminal engine, and do not invent untested performance results.

**This session does exactly one increment.** You run unattended, one fresh session per increment; the next session starts from `docs/status/PROGRESS.md` and Git alone, so they must be accurate when you stop. First inspect `git status` and recover any unfinished or unpushed work from the previous session before taking a new task. Then complete the earliest ready task, or one slice of it, validate it, update PROGRESS.md, commit, push, verify the push, and stop.

**Durable state.** `docs/status/PROGRESS.md` is your plan file and the only resume index; use it wherever your harness instructions mention `.agent/PLANS.md`, and do not create `.agent/`. Keep its `Next task:` line exact.

**Context.** The window is 128k but compaction fires near 90k. Keep task entry small; if context gets crowded, write a handoff into PROGRESS.md using `docs/status/HANDOFF_TEMPLATE.md` and stop.

**Commands.** Keep cargo output short (`--quiet`, `--message-format short`, filter to the first errors). Never run a command that opens a window or waits for input without a `timeout`; prefer headless tests. There may be no display. Record visual or native Wayland/X11 checks as pending manual tests for the owner, never as passed, and continue with the next task when the workflow allows.

**Git.** For every completed change, run its relevant checks, update progress notes, create a clear commit, push it, and verify the push. Preserve user changes, never force-push, and do not change Git identity or remotes.

**Stopping.** Stop without starting new work, and say why in your final message, when: the next task is N31 or later (owner's release assessment); a blocker needs the owner (failed capability gate needing an ADR decision, push/auth failure, missing hardware); or the same failure has already been recorded in PROGRESS.md. Record a new blocker once in PROGRESS.md's Blockers section and commit/push that note; do not re-commit an already recorded blocker.

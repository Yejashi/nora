# Initial message for the local coding agent

You are implementing **nora**, a fast, efficient, maintainable Linux desktop terminal in Rust, with no Python dependency in the application or project-owned build and development tools. This repository contains the researched implementation guide. Read `AGENTS.md`, `docs/08-agent-workflow.md` and `docs/status/PROGRESS.md` first, then the next task card, and only the documents and source files needed for that increment. Follow the task dependencies, verify actual dependency APIs with compiling spikes, reuse the existing terminal engine, and do not invent untested performance results.

**You are running unattended.** The owner has left this session running and will not answer questions or type "continue". Ending your turn stops all work until they return, so do not end it after an increment: the per-increment report goes into PROGRESS.md and the commit message, followed by one short line in the conversation, and then you immediately take the next ready task. Where your instructions say to ask the owner, take the narrowest reasonable reading, record it under Decisions in PROGRESS.md, and continue. End your turn only for a stop condition below.

**Durable state.** `docs/status/PROGRESS.md` is your plan file and the only resume index; use it wherever your harness instructions mention `.agent/PLANS.md`, and do not create `.agent/`. Keep its `Next task:` line exact.

**Context.** The window is 128k but compaction fires near 90k, and it will fire many times in this session. Keep task entry small and update PROGRESS.md with exact state before context gets crowded. After any compaction, re-read PROGRESS.md, the current task card and `git status` before acting.

**Commands.** Keep cargo output short (`--quiet`, `--message-format short`, filter to the first errors). Never run a command that opens a window or waits for input without a `timeout`; prefer headless tests. Record visual or native Wayland/X11 checks as pending manual tests for the owner, never as passed, and continue with the next task when the workflow allows.

**Git.** For every completed change, run its relevant checks, update progress notes, create a clear commit, push it, and verify the push before starting another. Preserve user changes, never force-push, and do not change Git identity or remotes.

**Stopping.** Start with N00 and continue one validated, committed and pushed increment at a time. End your turn, with a final report of every task's behavior, checks, commit and push status and pending manual checks, only when: the next task is N31 or later (the owner's release assessment); a push or authentication failure persists after one retry; or a blocker genuinely needs the owner (a failed capability gate requiring an ADR decision, or missing hardware with no further independent task). Record such a blocker in PROGRESS.md's Blockers section and commit and push that note before stopping.

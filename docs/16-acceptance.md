# Acceptance status (re-established 2026-10-15, updated 2026-10-16)

The original v0.1 acceptance (docs/13-v0.1-acceptance.md) was withdrawn
by the runtime audit: the interactive window had never been exercised.
This record re-establishes acceptance per roadmap milestone
(docs/14-production-roadmap.md) from end-to-end evidence. Statuses:
**accepted** (acceptance criteria met, evidence in CI or recorded),
**partial** (some criteria open, listed), **open**.

Evidence sources: `scripts/e2e-x11.sh` (23 scenarios in a real X11 window
with real key and mouse events, run in CI job `e2e-x11`), the test suite
(about 600 tests), `scripts/perf.sh`, and the audit table of fixes
(docs/audit/2026-10-15-runtime-audit.md).

| Milestone | Status | Evidence | Open |
| --- | --- | --- | --- |
| M0 evidence harness | accepted | e2e-x11 CI job; mutation checks show the redraw, selection, flood and input-bound scenarios fail without their fixes | — |
| M1.1 colors | accepted | e2e `render`, palette tests | — |
| M1.2 PTY | accepted | e2e `tty`, `resize`; headless exit status; PTY tests | — |
| M1.3 keyboard | accepted | e2e `keys` (byte-exact), encoder tests | kitty keyboard protocol unsupported (documented) |
| M1.4 mouse | accepted | e2e `mouse` (byte-exact SGR), encoder tests | — |
| M1.5 frames | accepted | e2e `redraw`, `selection`, `idle` (fully and partly scrolled back: 0 frames idle), `viewlat` (view change on an idle pane 7-9 ms) | atlas trim not soak-tested |
| M1.6 fonts and metrics | accepted | e2e `zoom`, metric tests | fractional scale verified by unit geometry only |
| M1.7 bounds and latency | accepted | e2e `flood` (+5.5 MiB under a 200 MiB OSC flood), input-bound test, close escalation test; key-to-present ~1 ms | — |
| M1.8 reports and environment | accepted | e2e `reports`, `hyperlink`; OSC 7 decoder byte-safe (RV-OSC7) | prompt navigation has unit tests only (needs the bash integration in e2e) |
| M2 multi-pane UI | accepted | e2e `panes`, `tinysplit`, `paneexit`, `search` (scrollback, chords), `title`; review RV3, RV4, RV6, RV8-RV10 fixed | IME preedit and candidate placement unverified with a real input method (manual check under fcitx5/ibus) |
| M3 performance | accepted (baseline recorded) | docs/performance/2026-10-15-baseline.md, input latency record | window-path throughput ~56 MiB/s vs ~110 MiB/s headless (snapshot under the terminal lock, audit P8); per-frame allocations not measured |
| M4 customization | accepted | e2e `config` (startup, live reload incl. palette repaint of an idle pane, invalid edit), schema tests; RV5, RV7 fixed | — |
| M5 cross-platform | partial | Linux X11/Wayland verified; Windows: build + headless tests green in CI; macOS: build, unit, platform-neutral and PTY tests green in CI (first full green run `80047545`) | `cwd_is_honored` failed once on macOS (suspected flaky; panic messages now reach CI annotations); macOS/Windows interactive use untested; no macOS/Windows packaging |
| M6 autocomplete | design accepted (ADR-013) | — | implementation started 2026-10-16 (M1-M4 accepted; M5's open items are interactive checks on other OSes, not blockers for a Linux-first feature) |

Real applications exercised end to end: vim (insert, Escape, `:wq`), tmux
(status line and pane), htop (colors and meters).

What "accepted" does not claim: hardware coverage beyond one AMD GPU
(RADV) and Mesa lavapipe; compositors beyond GNOME and Weston; a manual
accessibility review.

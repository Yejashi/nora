# Window-path throughput (2026-10-17, M3, audit P8)

The baseline (2026-10-15) reported the window path at about 56 MiB/s
against about 110 MiB/s headless and attributed the gap to the full-grid
snapshot taken under the terminal lock. Re-measured, the gap is mostly a
measurement artifact plus a scrollback cost in the engine; the window
path itself costs little.

## Method

The 32 MiB corpus of `scripts/perf.sh` is streamed with `cat` inside the
PTY. The child times `cat` itself (`date +%s%N` around it): the PTY buffer
is a few KiB, so `cat` ends when nora has consumed the stream. The old
`perf.sh` method (wall time polled every 100 ms, minus a 1 s sleep and the
`nora -- true` startup median) had an error of the order of the stream
time (about 300 to 400 ms). `perf.sh` now uses the child's timing.

Three paths, each run several times, all at 80x24 with an isolated
default configuration (Xvfb and Mesa lavapipe for the window, Arch Linux):

- **window**: `nora -- sh -c '...cat...'` with a display;
- **headless**: the same with no display (no window, no snapshots;
  headless sessions have no scrollback);
- **raw PTY**: `script -qfc '...cat...' /dev/null`, which only copies.

The window run of the first measurement used the user's configuration
(120x35); wider rows cost more to clear on scroll, so all runs below use
the default 80x24.

## Results

| Scrollback (window) | Window | Headless (no scrollback) | Raw PTY |
| --- | --- | --- | --- |
| 0 | 80-88 MiB/s | 87-93 MiB/s | |
| 1 000 | 91-95 MiB/s | 88-92 MiB/s | |
| 10 000 (default) | 70-80 MiB/s | 94-98 MiB/s | 116-128 MiB/s |

`perf record` of the PTY reader thread at 80x24, 4 kHz, samples:

| Symbol | Window, 10 000 lines | Headless |
| --- | --- | --- |
| `Term::input` | 636 | 697 |
| `Row::reset` | 396 | 111 |
| prompt-marker + graphics filters | 143 | 145 |
| Reader thread total | 1 482 | 1 270 |

The pump thread does not appear in the profile, and the snapshot is not
a measurable cost. The whole difference is `Row::reset` in
alacritty_terminal's grid: with a 10 000-line scrollback the row recycled
at each scroll was written 10 000 lines earlier and is cold in the cache;
without scrollback it was just written. At 1 000 lines (about 2 MB of
cells, cache resident) the window path matches headless.

## Conclusion

No change to nora: the window path's own cost is within the run-to-run
spread, and the remaining cost of a long scrollback is the engine's
grid, which nora does not replace (AGENTS.md). The audit P8 hypothesis is
not supported by the profile.

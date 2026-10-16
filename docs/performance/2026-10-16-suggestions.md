# Inline suggestion cost (2026-10-16)

ADR-013 step 5. Measured, not targets.

**zsh hook, per keystroke.** `__nora_line_report` (base64-encode the
buffer in pure zsh, write one OSC 6973) timed over 2 000 calls with
`$EPOCHREALTIME`, zsh 5.9.1, x86_64 NixOS:

| Buffer length | Per report |
| --- | --- |
| 10 characters | 105 µs |
| 40 characters | 208 µs |
| 120 characters | 469 µs |

The cost grows linearly with the buffer (one arithmetic step per byte).
Unchanged buffers and cursors are not re-sent. A long pasted line costs a
few milliseconds once; ordinary command lines stay well under a frame.

**Key to suggestion on screen.** `NORA_TRACE_LATENCY=1` logs
key-to-suggestion: from the latest key press to the first presented frame
showing a new suggestion. The path is key → PTY → zsh echo and buffer
report → OSC filter → pump (history lookup) → window → frame.
`scripts/e2e-x11.sh suggest` under Xvfb with Mesa lavapipe (software
Vulkan), 3 runs, 9 samples each:

| Run | Samples (ms) |
| --- | --- |
| 1 | 2.28 1.06 1.37 2.48 1.34 1.15 1.49 1.17 2.71 |
| 2 | 1.59 1.14 1.15 0.91 0.92 0.82 1.22 1.33 2.82 |
| 3 | 2.08 1.50 2.78 1.51 1.95 1.41 1.41 1.50 3.46 |

Median 1.4 ms, maximum 3.5 ms. The history held one command; ranking is
a linear scan, at most 5 000 entries (not timed at that size).

# Input latency and idle wakeups (2026-10-15)

**What is measured.** Key-to-present: the time from winit delivering a key
press to the next frame nora presents (`NORA_TRACE_LATENCY=1`). It covers
the window thread, the pump, the PTY round trip through bash's echo, the
parser and the frame build and submit. Compositor and scan-out time are
not included.

**Setup.** `scripts/e2e-x11.sh latency`: Xvfb 1024x700, Mesa lavapipe
(software Vulkan), release build, bash `--norc`, 40 characters typed by
xdotool at 120 ms intervals. Host: x86_64 NixOS, AMD Radeon RX 6750 XT
present but unused (lavapipe forced). Software rendering inflates frame
cost, so absolute numbers on a hardware GPU will differ. The comparison is
like for like.

| Build | Runs (median / p95, ms, n=40 each) |
| --- | --- |
| Before (`065f9e5`): pump sleeps 10 ms per loop | 12.08 / 20.93; 15.97 / 17.44; 19.33 / 21.06 |
| After: pump blocks on its channel, woken by PTY output | 1.05 / 1.24; 1.10 / 1.36; 1.06 / 1.47; 0.81 / 1.01 |

**Idle wakeups.** nora's own threads over 10 s idle under Xvfb, after the
change: 21 context switches in total, from the 500 ms pump heartbeat. The
previous loop woke at least 100 times per second by construction
(`thread::sleep(10 ms)`). The lavapipe worker threads wake on their own
(about 4 per second each) independent of nora; nora presented 2 frames in
9 idle seconds.

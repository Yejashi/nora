# Allocations per frame (2026-10-17, M3, audit R12)

Method: a measurement build (`cargo build --release --features
alloc-count`) installs a counting global allocator that counts each
thread's `alloc`, `alloc_zeroed` and `realloc` calls. With
`NORA_TRACE_FRAMES=1` every `frame:` line then reports the window thread's
allocations for the frame, in total and per stage (gather, acquire, scene,
encode, present). Normal builds do not contain the counter.

Environment: Xvfb with Mesa lavapipe (software Vulkan, so the driver runs
in the process and its allocations on the window thread are counted in
`present`), Arch Linux, Noto fonts. These are counts, not timings; the
timings below are lavapipe's and not hardware evidence.

Cases:

- **Stream**: `scripts/perf.sh --xvfb` (32 MiB `cat` with SGR colors,
  grid 120x35, about 2 300 drawn cells per frame).
- **Typing**: `echo the quick brown fox jumps over the lazy dog` typed
  into bash with xdotool at 80 ms per key (80x24, one frame per echo).

## Results

| Case, allocations per frame | Before | After glyph cache | After inline cell text |
| --- | --- | --- | --- |
| Stream, median (max) | 25 596 (26 600) | 2 476 (3 500) | 181 (1 188) |
| Stream, encode stage | 23 149 | 29 | 29 |
| Stream, scene stage | 2 363 | 2 363 | 51 |
| Typing, median (p95) | 381 (578) | 179 (210) | 170 (173) |

Stream encode time (median) went from 1.37 ms to 0.41 ms on lavapipe.

Causes and changes:

- **Glyph buffers cloned per cell.** The glyph cache handed out a clone of
  each cached cosmic-text `Buffer` for every drawn cell on every frame
  (about 10 allocations per cell), and built a `String` key per lookup.
  The cache now returns slots into buffers it owns; the text areas borrow
  them. Lookups reuse one scratch key, so a hit allocates nothing. The
  entry cap is applied at the start of a frame, so a slot stays valid for
  the whole frame.
- **A `String` per drawn cell.** The scene stored each cell's cluster in a
  new `String`. Clusters of up to 15 bytes (a character and a few
  combining marks) are now stored inline.

What remains per frame: wgpu and lavapipe work in `present` (about 67),
per-frame GPU vertex buffers and glyphon preparation in `encode` (about
30 to 55), and vector growth in the scene. Persistent vertex buffers
would remove part of the encode share; not done, as the remaining count
is small and constant per frame.

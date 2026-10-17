# ADR-014: Displaying kitty graphics

- **Status**: accepted
- **Date**: 2026-10-17
- **Supersedes**: the handler semantics of N33/N34 (ADR-011's APC hook stays)

## Problem and evidence

nora parses kitty-graphics APCs (ADR-011) but never draws an image: the
engine's graphics queue has no consumer (audit R13), and the handler in
`graphics_image.rs` is not the protocol. Its replies are the literal text
`"ESC _ G … ST"`, `a=T` only stores (the spec: transmit and display, `a=t`
transmits), `X`/`Y` are read as cell positions (the spec: pixel offsets
inside the cursor cell; images are placed at the cursor), and nothing
moves the cursor after a display.

A conforming display needs, from the spec
(<https://sw.kovidgoyal.net/kitty/graphics-protocol/>):

1. the cursor position at the APC's place in the stream (images are put at
   the cursor) and the cursor moved after a display (`C=0`: right by the
   image's columns, down by its rows minus one, as kitty does);
2. replies ordered with the parser's own replies (support detection sends
   a query followed by `CSI c` and relies on the order);
3. images that scroll with the text and are drawn under or over it by
   z-index;
4. formats 24, 32 and 100 (PNG), zlib compression, direct and file media.

## Decision

- **State in the engine, handled on the reader thread, in stream order.**
  A `GraphicsState` (images with a byte budget, placements, chunk assembly)
  lives behind the engine's mutex like the marker store. The PTY filter
  runs the prompt-marker filter first and the graphics filter on its output
  (the graphics filter forwards bytes unchanged), so both report positions
  in the same forwarded stream. The vendored loop already parses in
  segments around reported positions; its hook gains the screen size and
  the alternate-screen flag, and a way to inject bytes at that point.
- **Cursor movement by injection.** After a display without `C=1` the hook
  injects ordinary control bytes (IND, then `CSI n C`, or CR LF when
  the image reaches the right edge) into the parser at the APC's position,
  so scrolling and margins behave as for text.
- **Replies through the recorder.** The hook records a reply as a
  `PtyWrite` event at its position, the same path as `CSI c` and DSR
  replies, so the order holds. `q=1` drops OK replies, `q=2` errors.
- **Anchors like prompt markers.** A placement stores the cursor line with
  the grid's `total_scrolled`/`total_pulled` counters (as N35b markers do),
  so it scrolls with the text and survives vertical resizes; it is dropped
  once it has scrolled out of the history. Placements belong to the main
  or the alternate screen. `CSI 2 J` deletes the placements on the screen,
  `CSI 3 J` and `ESC c` all of them (as kitty does on clear).
- **Drawing.** Snapshots carry the visible placements (viewport cell,
  pixel offsets, size in cells, source rectangle, z) with a shared handle
  to the image's RGBA pixels. The renderer keeps one texture per image
  (keyed by id and upload generation), drops unused ones, and draws
  textured quads in three bands: z below −1 073 741 824 under cell
  backgrounds, other negative z under the text, z ≥ 0 over it.
- **Media reads.** A command with a file medium (`t=f`, `t=t`, `t=s`) is
  read and decoded without the terminal lock: a file on a hung NFS or
  sshfs mount would otherwise freeze the window. The PTY filter runs
  under the lock, so it only marks such a command (following the
  chunking state, so a chunk continuation is never read); the vendored
  loop asks `Filter::needs_unlocked` before each reported position and,
  when it says yes, releases the lock and its lease, calls
  `Filter::run_unlocked` (the read, decode and the `t=t`/`t=s`
  deletions), and locks again for `on_marker`, which handles the
  command with the loaded result. The parser keeps its state across the
  gap, so the bytes are still parsed in order and replies keep their
  place. `Engine::feed` runs the filter before it locks, so it loads
  files there (up to 64 MiB decoded per chunk; past that, in place). A
  path sent in chunks (`m=1`) is answered `EBADF` and never read.
  Rejected: refusing network and FUSE filesystems by `fstatfs` magic
  (a local disk can also stall, and it blocks legitimate use).
- **Dependencies.** `png` (0.18.1) and `flate2` (1.1.10) become direct
  dependencies; both are already in the lock through `arboard`, so no new
  crate is compiled.

## Scope

In: actions `t`, `T`, `p`, `q`, `d`; formats 24/32/100; `o=z`; media `d`,
`f`, `t` and `s` (regular files only, a uniform `EBADF` on any read
failure, `/proc`, `/sys` and `/dev` other than `/dev/shm` refused, `t`
deletes only files under a temp directory whose path contains
`tty-graphics-protocol`, `s` reads `/dev/shm/<name>` and unlinks it once it
decoded); `i`, `I`, `p`;
source rectangle `x,y,w,h`; `X,Y`; `c,r` (aspect kept when one is given);
`C`; `z`; delete `a/A i/I n/N c/C p/P q/Q x/X y/Y z/Z r/R`; chunking.

Unicode placeholders (`U=1` virtual placements and `U+10EEEE` cells) were
added after the first slices.

Limits added by the 2026-10-17 bug sweep: a placement is at most 10,000
columns and rows (`c`, `r` and the size derived from the pixels are
clamped to it), and a display moves the cursor at most one screen. An
image stored with neither `i` nor `I` gets an internal id that was never
told to the client: no client command (put, delete by id or range,
transmit, placeholder) addresses it, and a client image that takes the
same id moves the internal one aside instead of replacing it. A file
transfer (`t=f`) honours the `O` offset and `S` size.

Out (documented, answered with an error where a reply is due):
animation (`a=f`, `a=a`, `a=c`), relative placements (`P,Q,H,V`).

## Consequences

- The reader thread decodes PNG and inflates zlib for images it receives;
  this is bounded by the per-image and total byte budgets and is in stream
  order anyway (the parser waits for it, as for any output). Files are
  read and decoded off the terminal lock (Media reads); direct data is
  still decoded under it.
- The renderer gains a texture pipeline and a per-image texture budget.
- The old `GraphicsHandler` tests are replaced by protocol tests.

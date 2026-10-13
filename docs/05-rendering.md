# Fixed-cell rendering plan

Terminal geometry is authoritative. The renderer turns the engine's cells into pixels; it must not recompute terminal column widths using paragraph layout. `cosmic-text` and `glyphon` are text tools, not a complete terminal renderer [S14, S15].

## Coordinates and shaping

- Track physical pixels, logical window units and terminal row/column explicitly. Use typed helpers or named conversions.
- Compute physical cell width/height and baseline from the chosen primary font/size/scale. Round consistently once; columns use `pane_x + column * cell_width`.
- Honor engine wide-lead/trailing/spacer flags. Shape the lead cell's text plus stored combining marks; a wide glyph occupies the engine's allocated two columns. Do not render a second glyph from its spacer.
- Default to no optional cross-cell programming ligatures. Required combining/script shaping may occur within an engine cell cluster; the allocated cells remain fixed. Do not reorder terminal columns for bidi in v0.1.
- Font fallback supplies missing glyphs without changing cell advance. Prefer a suitable size/vertical alignment; clip ink to the permitted span rather than shrinking all glyphs inconsistently.
- Preserve the original engine text for selection/copy; do not copy shaped glyph IDs or substitute glyph strings.
- ZWJ emoji/complex graphemes may expose limitations of the engine's cell model [S09]. Test and document exact behavior; adding a shaper cannot repair the engine's width policy by itself.

Unicode segmentation and East Asian width are separate concepts [S19, S20]. Pin/record the engine's Unicode policy and version when possible. Locale/application width disagreement is a compatibility concern, not something to fix by stretching glyph advances.

## N02 glyphon feasibility gate

Test fixed-column placement using the pinned glyphon/cosmic versions. A whole-row proportional Buffer is not sufficient evidence. Per-cell clusters, appropriately controlled runs or custom glyph placement may work, but benchmark the number of buffers/runs and confirm ligatures/fallback do not shift columns. Generic glyphon clipping/scale does not prove cell correctness.

Accept glyphon only if it provides: stable placement and combining/CJK alignment; reuse without shaping every unchanged cell; bounded atlas/cache behavior or a safe rebuild policy; compatible GPU/device types; and manageable maintenance. N02 may first validate ASCII, then complex cases return in N13. Keep its usage behind `TextRenderer`, not throughout the application.

If it fails, use cosmic-text raster output with a small nora-owned glyph atlas and positioned quads. Keep shaping/font discovery in the library; avoid reimplementing a font engine. Use `etagere` or another verified atlas allocator only when needed. Record an ADR and benchmark the replacement against the spike. Do not maintain two full text renderers indefinitely.

## Cache keys and bounds

Glyph cache keys include font identity/content epoch, glyph ID, physical size, style, subpixel/raster parameters and color-vs-mask mode. Shaped-cell cache keys also include text, assigned span and shaping settings. Palette changes normally invalidate color/geometry, not monochrome raster masks.

Account allocated atlas pages, temporary staging memory and CPU glyph images. A 2048×2048 RGBA page costs 16 MiB before overhead; eight such pages already exceed the proposed 64 MiB GPU budget. Device maximum texture dimensions are capabilities, not recommended allocation sizes.

Eviction cannot leave retained geometry pointing to reused UV rectangles. Use atlas generations with glyph-handle validation or rebuild affected geometry after eviction. Protect resources used by submitted frames; use wgpu lifecycle rather than manual unsafe reclamation. Test a deliberate tiny atlas to force the failure paths.

## Frame pipeline

1. Capture/copy the owned visible snapshot and damage under the engine lock.
2. Release the lock. Resolve style/color and update dirty cell/row layout.
3. Populate needed raster glyphs and atlas uploads; record hits/misses/evictions.
4. Update retained background/text/decoration/overlay geometry.
5. Acquire a supported surface frame outside the engine lock.
6. Draw a complete coherent window image with per-pane clipping; submit/present.
7. Record the snapshot/font/geometry revisions actually submitted.

Use batched backgrounds and glyphs, with scissor clips per pane and a few passes by texture/format. Render selected/search colors deterministically; honor reverse video, conceal, faint, bold/italic, underline and strikeout. Add styled underline geometry as an incremental task. Use consistent alpha conventions, an appropriate surface color format and grayscale antialiasing initially. Color emoji must have its own format/blending path if supported.

Cache CPU geometry for unchanged rows first. Partial GPU buffer updates come after a profile; a coherent full draw of cached rows can still be efficient. Do not assume swapchain images preserve old pixels. A retained offscreen target is optional and must be justified by measurements.

## Scheduling and surfaces

Request one redraw when state changes, not one per byte. Use `Wait`/`WaitUntil` for idle/cursor/timeout scheduling. Blink schedules only while visible/focused; disabling blink leaves no animation timer. Synchronized-output mode defers content presentation, with a finite timeout; engine support must be verified and reused.

The main thread may block in surface acquisition; instrument this explicitly. Default to a supported vsync-capable mode. A low-latency option can use a supported alternative; it must not promise no compositor queueing. Limit unnecessary queued frames using supported configuration, and do not spin on timeouts [S16, S17].

Handle minimized zero-size surfaces, resize/outdated/lost surface, timeout, device loss and memory failure using the exact pinned API's variants. Reconfigure/rebuild from the latest owned state when recoverable. Use bounded retry with actionable diagnostics for fatal failure; never recursively recreate devices forever. A software GPU adapter is diagnostic/fallback where available, not a promised CPU renderer.

## Visual tests

Use a redistributable pinned test font and offscreen snapshots for stable cases, with tolerance for antialiasing differences. Verify cell boundaries/cursor positions numerically as well as with images. Manual hardware cases include 1.0/1.25/1.5/2.0 scale, mixed DPI monitors, font reload, complex fallback, rapidly resized splits and GPU recovery. Test glyph ink clipping, wide chars at the last column, underlines and selection spanning wrapped rows.

## Window geometry and resize (N11)

- The grid that fits the window is always derived from its **logical** size: `app::window_grid(window, fonts)` converts `window.inner_size()` to logical pixels via `window.scale_factor()`, then divides by the mono cell metrics in `src/app/glyphs.rs` (`FONT_SIZE = 20.0`, `CELL_H = 24.0` — logical pixels). Dividing a physical size by logical metrics is correct only at scale 1.0; the `Resized` arm, the render-path stale gate and the session spawn in `src/main.rs` all use `window_grid`.
- **Stale-geometry rejection**: the geometry pair is the staleness tag in this single-window architecture — `snapshot_is_stale(snap_rows, snap_cols, grid_rows, grid_cols)` (`src/render/policy.rs`) is true when the fetched snapshot's rows or cols differ from the window's current `window_grid`. The render pass computes the current grid after taking the slot and, on staleness, drops the snapshot (debug line `debug: stale snapshot RxC != grid GxG`) instead of presenting old geometry to the new surface. `Snapshot.epoch` remains the per-capture monotonic id and is not consulted by the gate.
- **Zero retention**: a zero on either axis keeps the last valid value for that axis, per level. `clamp_grid(rows, cols, prev_rows, prev_cols)` (`src/render/policy.rs`) clamps the pair the window arms send (a tiny or minimized window never requests a zero row or column), and `PtySession::resize` (`src/session/pty.rs`) retains per axis before anything is applied — on an unchanged retained pair the resize is a no-op (no engine resize, no `TextAreaSizeRequest`, no event emitted); the engine must never see a zero.
- The window loop handles **`WindowEvent::ScaleFactorChanged`** (pinned winit 0.30.13 `event.rs` variant — the `inner_size_writer` is left untouched, so the OS-driven default applies): the physical size is unchanged (no `set_size`, no reconfigure); the logical grid is re-derived via `window_grid`, the change is folded through the render machine exactly like `Resized` (forcing `Damage::Full`), and the clamped resize is sent on change. No `request_redraw` in this arm — the redraw arrives via the existing SITE 1 (DirtyPulse) when the pump takes the post-resize `Full` snapshot; the exactly-three `request_redraw` call sites (DirtyPulse / Resized / Reconfigure) remain intact.
- The `Resized`/`ScaleFactorChanged` arms send `PumpMsg::Resize` only when the clamped grid differs from `last_grid` (initial `(0, 0)` = never sent), so repeated no-op derivations emit nothing.
- The session spawn in `src/main.rs` uses `app::window_grid(&window, &fonts)` for the child's WINSIZE (the window is created before the spawn, so the child sees the window's grid from the first byte); a degenerate display (grid `(0, 0)`) falls back to `(24, 80)` with a debug line.

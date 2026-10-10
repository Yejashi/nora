# Primary sources and evidence boundaries

Retrieved/checked on 10 October 2026. Sources with `/latest`, `/main` or `/master` are moving references; implementation must pin actual versions/revisions. A source's own performance claim is not an independent benchmark. These links are for the agent's targeted verification, not a requirement to ingest every linked page.

| ID | Primary source | Evidence used |
| --- | --- | --- |
| S01 | [kitty overview](https://sw.kovidgoyal.net/kitty/overview/) | C/Python/Go division and OpenGL design |
| S02 | [kitty performance](https://sw.kovidgoyal.net/kitty/performance/) | Separate performance axes; rendering-suppressed benchmark caveat; hardware/version-specific table |
| S03 | [Alacritty repository](https://github.com/alacritty/alacritty) | Rust/OpenGL desktop application and engine separation |
| S04 | [alacritty_terminal public API](https://docs.rs/alacritty_terminal/latest/alacritty_terminal/) | Published 0.26.0 crate and terminal/PTY modules |
| S05 | [About Ghostty](https://ghostty.org/docs/about) | Shared core and platform applications |
| S06 | [Ghostty upstream README](https://github.com/ghostty-org/ghostty/blob/main/README.md) | Available VT library; API in flux and no library version tag; scope distinction |
| S07 | [Term API](https://docs.rs/alacritty_terminal/latest/alacritty_terminal/term/struct.Term.html) | Renderable content, damage, selection/search/resize; UI damage exclusions |
| S08 | [Engine EventLoop API](https://docs.rs/alacritty_terminal/latest/alacritty_terminal/event_loop/struct.EventLoop.html) | Shared terminal/PTY worker integration |
| S09 | [Cell API](https://docs.rs/alacritty_terminal/latest/alacritty_terminal/term/cell/struct.Cell.html) | Per-cell character, flags, extra combining/hyperlink data |
| S10 | [Engine Event API](https://docs.rs/alacritty_terminal/latest/alacritty_terminal/event/enum.Event.html) | PTY/clipboard/color/size/title events and formatter callbacks |
| S11 | [TermMode flags](https://docs.rs/alacritty_terminal/latest/alacritty_terminal/term/struct.TermMode.html) | Keyboard, paste, mouse and focus mode flags |
| S12 | [winit API](https://docs.rs/winit/latest/winit/) | Window/event/scale responsibilities; observed 0.30.13 |
| S13 | [wgpu API](https://docs.rs/wgpu/latest/wgpu/) | GPU abstraction; observed current 30.x API family |
| S14 | [cosmic-text API](https://docs.rs/cosmic-text/latest/cosmic_text/) | Font discovery/fallback/shaping/rasterization; observed 0.19.0 |
| S15 | [glyphon API](https://docs.rs/glyphon/latest/glyphon/) | Text renderer/atlas layer over wgpu and cosmic-text |
| S16 | [wgpu PresentMode](https://docs.rs/wgpu/latest/wgpu/enum.PresentMode.html) | Supported modes differ by backend; capability selection and queue behavior |
| S17 | [wgpu SurfaceConfiguration](https://docs.rs/wgpu/latest/wgpu/type.SurfaceConfiguration.html) | Surface configuration is version-specific |
| S18 | [xterm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) | Authoritative xterm control definitions and query semantics |
| S19 | [Unicode UAX #11](https://www.unicode.org/reports/tr11/) | East Asian width properties; not a universal terminal layout algorithm |
| S20 | [Unicode UAX #29](https://www.unicode.org/reports/tr29/) | Text/grapheme segmentation, separate from column width |
| S21 | [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir/latest/) | Config/state/cache directory semantics |
| S22 | [Alacritty event loop source](https://github.com/alacritty/alacritty/blob/master/alacritty_terminal/src/event_loop.rs) | Inspected channel/pending-write architecture; must re-audit pinned version |
| S23 | [kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) | Legacy default and negotiated enhancements |
| S24 | [kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/) | Graphics transport/control specification for later milestones |
| S25 | [smithay-clipboard](https://github.com/Smithay/smithay-clipboard) | Native Wayland clipboard helper candidate |
| S26 | [arboard Clipboard API](https://docs.rs/arboard/latest/arboard/struct.Clipboard.html) | Desktop clipboard helper candidate; backend/lifetime verification needed |
| S27 | [ncurses terminfo manual](https://invisible-island.net/ncurses/man/terminfo.5.html) | Capability database contract and custom-entry rationale |
| S28 | [Alacritty vte](https://github.com/alacritty/vte) | Parser library; a parser alone is not an entire terminal |
| S29 | [glyphon Cargo manifest](https://github.com/grovesNL/glyphon/blob/main/Cargo.toml) | Observed 0.12.0 manifest uses wgpu 30 and cosmic-text 0.19; not a compiled tuple |
| S30 | [Alacritty license](https://github.com/alacritty/alacritty/blob/master/LICENSE-APACHE) | Apache-2.0 published license |
| S31 | [Ghostty license](https://github.com/ghostty-org/ghostty/blob/main/LICENSE) | MIT published license |
| S32 | [kitty license](https://github.com/kovidgoyal/kitty/blob/master/LICENSE) | GPLv3 published license; protocol implementation vs code import distinction |
| S33 | [winit IME events](https://docs.rs/winit/latest/winit/event/enum.Ime.html) | Preedit/commit lifecycle; platform validation still required |
| S34 | [Engine FairMutex](https://docs.rs/alacritty_terminal/latest/alacritty_terminal/sync/struct.FairMutex.html) | Fair synchronization primitive for the shared terminal |
| S35 | [Alacritty frontend keyboard source](https://github.com/alacritty/alacritty/blob/master/alacritty/src/input/keyboard.rs) | Input encoding reference separate from engine flags |

## What these sources do not establish

- They do not show that nora is faster than any terminal. Nora has not been implemented here.
- They do not establish a working Cargo dependency tuple. The agent must compile and pin it.
- They do not establish that generic glyphon layout meets terminal-cell constraints or desired cache limits.
- They do not promise that `alacritty_terminal` exposes hooks for the entire kitty graphics protocol.
- They do not prove native Wayland/X11 clipboard or IME works in nora.
- They do not establish a universal hardware-independent throughput/latency/memory target.
- They do not validate arbitrary third-party Rust bindings for Ghostty as official or stable.

The selected architecture, budgets, task order, performance goals and agent workflow are original project proposals/requirements. They are labeled as such rather than attributed to upstream projects.

## Follow-up research when needed

Canonical foot repository: https://codeberg.org/dnkl/foot (not successfully retrievable in this research). Unix PTY behavior: consult the installed Linux/libc documentation and pinned engine Unix PTY source. Offscreen GPU/Wayland APIs: consult the exact pinned versions instead of extrapolating from this snapshot. Do not rely on unofficial mirrors when importing code without verifying provenance.

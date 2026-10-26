<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/nora-wordmark-dark.svg">
    <img src="docs/images/nora-wordmark-light.svg" alt="nora" width="380">
  </picture>
</p>

<p align="center">
  <strong>A GPU-accelerated terminal for Linux that finishes your commands.</strong><br>
  Written in Rust. Native Wayland and X11. Suggestions from your own shell history.
</p>

<p align="center">
  <a href="https://github.com/Yejashi/nora/actions/workflows/ci.yml"><img src="https://github.com/Yejashi/nora/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-8b5cf6.svg" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.95%2B-4cc9f0.svg" alt="Rust 1.95+"></a>
  <img src="https://img.shields.io/badge/platform-Linux-5ef2b5.svg" alt="Platform: Linux">
  <img src="https://img.shields.io/badge/status-0.1%20pre--release-lightgrey.svg" alt="Status: 0.1 pre-release">
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#features">Features</a> ·
  <a href="#installation">Installation</a> ·
  <a href="#configuration">Configuration</a> ·
  <a href="#shell-integration">Shell integration</a> ·
  <a href="CONTRIBUTING.md">Contributing</a>
</p>

<br>

<p align="center">
  <img src="docs/images/screenshot.png" alt="nora with three panes in a pastel theme: an anime-style twilight scene with a sakura tree and a full moon, shown with kitten icat; zsh with a dim inline suggestion completing &quot;kitten i&quot; from history; and vim showing nora's suggestion code" width="900">
</p>

## Why nora?

Most terminals draw what programs print. nora also works **with your
shell**. As you type, the shell tells nora what is on the command line,
and nora suggests the rest from the commands you have actually run. It
ranks them by how often and how recently you ran them, whether you ran
them in this directory, and whether they failed. The suggestion appears
as dim text, and <kbd>→</kbd> accepts it.

Because the shell's line editor stays the source of truth, suggestions
stay right through history recall, <kbd>Tab</kbd> completion and
kill/yank, which a terminal that guesses from the screen gets wrong.

Underneath, nora aims to be **correct first and fast second**:

- every keystroke and byte of output goes through in order;
- memory and queues are bounded;
- the window sleeps when nothing changes;
- every listed feature has a test that drives a real window.

## Quick start

```sh
git clone https://github.com/Yejashi/nora.git
cd nora
./scripts/install.sh          # builds and installs for your user, no root needed
```

Then turn on suggestions in zsh:

```sh
echo 'source ~/path/to/nora/shell/nora-zsh.zsh' >> ~/.zshrc
```

Open **Nora** from your application launcher, or run `nora`.

## Features

<table>
  <tr>
    <td width="50%" valign="top">
      <h4>Inline suggestions</h4>
      Completions from your own history, ranked by frecency, directory and
      exit status. With no match, an unambiguous path or command name.
      Everything stays on your machine.
    </td>
    <td width="50%" valign="top">
      <h4>Native Wayland and X11</h4>
      GPU rendering through wgpu (Vulkan, with a GL fallback). Only
      changed cells are rebuilt, and nothing is drawn while idle. The
      clipboard is native on both.
    </td>
  </tr>
  <tr>
    <td valign="top">
      <h4>Tabs, splits and search</h4>
      A tab bar once there is more than one tab, panes split right or
      below, and search across 10,000 lines of scrollback.
    </td>
    <td valign="top">
      <h4>Images and video</h4>
      The kitty graphics protocol: PNG and raw images, Unicode
      placeholders (images inside tmux), shared memory, and video
      through <code>mpv --vo=kitty</code>.
    </td>
  </tr>
  <tr>
    <td valign="top">
      <h4>Shell integration</h4>
      Jump to the previous prompt, select the last command's output, track
      the working directory, and Ctrl+click hyperlinks (OSC 8).
    </td>
    <td valign="top">
      <h4>Compatible</h4>
      xterm key and mouse encodings, the kitty keyboard protocol (all five
      flags), bracketed paste, focus events, true color and wide
      characters. vim, Neovim, tmux, htop and less run in CI.
    </td>
  </tr>
  <tr>
    <td valign="top">
      <h4>Accessible</h4>
      The active pane's text, cursor and selection are exposed to screen
      readers over AT-SPI2 through AccessKit. Input methods work, with
      ibus checked end to end.
    </td>
    <td valign="top">
      <h4>Safe by default</h4>
      Programs are untrusted: OSC 52 clipboard access is off, control
      sequences are size-capped, and hyperlinks open only on Ctrl+click.
      Live, validated TOML configuration.
    </td>
  </tr>
</table>

## Installation

nora is built from source; there are no packages yet.

### Requirements

- Linux with a Wayland or X11 session.
- Rust 1.95 or newer. [rustup](https://rustup.rs) installs the pinned
  1.99.0 automatically.
- A Vulkan driver. Any GPU with one works, and Mesa's lavapipe software
  driver works without a GPU.
- A monospace font. nora prefers Noto Sans Mono and falls back to any
  monospace font fontconfig knows. Noto CJK is optional, for wide scripts.

| Distribution | Packages |
| --- | --- |
| Arch | `base-devel vulkan-icd-loader noto-fonts noto-fonts-cjk`, plus your GPU's Vulkan driver |
| Debian / Ubuntu | `build-essential libvulkan1 mesa-vulkan-drivers fonts-noto-core fonts-noto-cjk` |
| Fedora | `gcc vulkan-loader mesa-vulkan-drivers google-noto-sans-mono-fonts google-noto-sans-cjk-fonts` |
| NixOS | `noto-fonts noto-fonts-cjk-sans`; the install script handles the libraries ([below](#nixos)) |

### Build and install

```sh
./scripts/install.sh
```

The script builds a release binary and installs it for your user:

| What | Where |
| --- | --- |
| The binary | `~/.local/bin/nora` |
| The `nora` terminfo entry (needs `tic`) | `~/.terminfo` |
| A desktop entry and icon, for application launchers | `~/.local/share/applications`, `~/.local/share/icons` |
| An example config, only if you have none | `~/.config/nora/config.toml` |

`./scripts/uninstall.sh` removes everything except your config.

To build without installing, run `cargo build --release`; the binary is
`target/release/nora`. To build a single windowing backend:

```sh
cargo build --release --no-default-features --features x11      # X11 only
cargo build --release --no-default-features --features wayland  # Wayland only
```

### NixOS

nora loads Vulkan, Wayland, xkbcommon and the X11 libraries at run time,
and NixOS has no global library path. The install script detects NixOS
and installs a small launcher that points at the right libraries in the
Nix store.

### Remote hosts

Programs on a remote machine need the terminfo entry to know nora's
capabilities. Copy [`terminfo/nora.terminfo`](terminfo/nora.terminfo) there
and run:

```sh
tic -x nora.terminfo
```

Without it, use `TERM=xterm-256color` for that session.

## Usage

```sh
nora                          # your login shell in a new window
nora -- htop                  # run a program instead (no shell involved)
nora --working-directory ~/src
nora --hold -- make test      # keep the window open after the program exits
nora --smoke                  # open, draw a frame, resize and exit: checks your GPU setup
```

Run `nora --help` for every option. nora exits with the status of the
program it ran.

### Key bindings

| Action | Keys |
| --- | --- |
| Copy / paste | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>C</kbd> / <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>V</kbd> |
| New tab / next / previous | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>T</kbd> / <kbd>Ctrl</kbd><kbd>PgDn</kbd> / <kbd>Ctrl</kbd><kbd>PgUp</kbd> |
| Split right / below | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>Enter</kbd> / <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>\\</kbd> |
| Focus a pane | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>Arrow</kbd>, or click it |
| Close pane | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>W</kbd> |
| Search | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>F</kbd>; <kbd>Enter</kbd> older, <kbd>Shift</kbd><kbd>Enter</kbd> newer, <kbd>Esc</kbd> closes |
| Scroll | <kbd>Shift</kbd><kbd>PgUp</kbd> / <kbd>Shift</kbd><kbd>PgDn</kbd>, <kbd>Shift</kbd><kbd>Home</kbd> / <kbd>Shift</kbd><kbd>End</kbd>, mouse wheel |
| Zoom in / out / reset | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>+</kbd> / <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>-</kbd> / <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>0</kbd> |
| Previous prompt / select last output | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>Z</kbd> / <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>G</kbd> (with shell integration) |
| Accept suggestion / one word | <kbd>→</kbd> or <kbd>End</kbd> / <kbd>Alt</kbd><kbd>→</kbd> |
| Open a hyperlink | <kbd>Ctrl</kbd> + click |
| Reload config | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>F5</kbd> |

Plain <kbd>Ctrl</kbd>+letter keys always reach the program, so
<kbd>Ctrl</kbd><kbd>C</kbd>, <kbd>Ctrl</kbd><kbd>D</kbd> and the rest
behave as usual, on any keyboard layout. A binding with nothing to act on,
such as <kbd>→</kbd> with no suggestion shown, passes its key through.
Every binding can be changed or removed in the config.

### Images and video

Programs that speak the kitty graphics protocol work as they do in kitty:

```sh
kitten icat picture.png                        # kitty's image viewer
chafa -f kitty picture.png                     # chafa
mpv --vo=kitty --vo-kitty-use-shm=yes clip.mp4 # video, frames through shared memory
```

Images scroll with the text, sit under or over it, and survive inside
tmux through Unicode placeholders (with `allow-passthrough on`).

## Configuration

nora reads `~/.config/nora/config.toml` (or
`$XDG_CONFIG_HOME/nora/config.toml`; `--config PATH` picks another file).
Every key is optional, and unknown keys are errors, so typos are caught.

```toml
[font]
family = "JetBrains Mono"
size = 13.0

[colors]
foreground = "#d8dee9"
background = "#1e222a"

[cursor]
style = "beam"

[[keyboard.bindings]]
keys = "ctrl+shift+d"
action = "split_right"
```

| Section | Keys |
| --- | --- |
| `[font]` | `family` (any fontconfig family), `size` (points, 4 to 200, default 15) |
| `[colors]` | `foreground`, `background`, `cursor`, `selection_background`; `[colors.normal]` and `[colors.bright]` for the 16 ANSI colors |
| `[window]` | `columns`, `lines`: the initial size in cells; `opacity` (0.0 to 1.0, default 1.0): the default background's opacity, text and colored cells stay opaque; `blur`: blur what shows through (KDE Wayland) |
| `[cursor]` | `style`: `block`, `beam` or `underline` |
| `[scrolling]` | `history` (lines, default 10,000), `multiplier` (lines per wheel step) |
| `[autocomplete]` | `enabled` (default `true`) |
| `[[keyboard.bindings]]` | `keys` (a chord such as `"ctrl+shift+d"`), `action` (an action name, or `"none"` to free a default) |

[`resources/nora.example.toml`](resources/nora.example.toml) documents
every key, its default and every action name.

The file is watched. Saving it applies colors, fonts, the cursor and key
bindings to the running window at once. Scrollback size applies to new
panes, and window size at the next start. A file with an error keeps the
current settings and shows the error at the bottom of the window. To check
a file without opening a window:

```sh
nora --check-config
```

## Shell integration

Shell integration is opt-in: add one line to your shell's startup file.

```sh
# ~/.zshrc: prompt markers, directory tracking and inline suggestions
source /path/to/nora/shell/nora-zsh.zsh

# ~/.bashrc: prompt markers and directory tracking
source /path/to/nora/shell/nora-bash.sh
```

| Capability | zsh | bash |
| --- | :---: | :---: |
| Previous prompt, select last output (OSC 133) | ✓ | ✓ |
| Working directory (OSC 7), used to rank suggestions | ✓ | ✓ |
| Inline suggestions | ✓ | — |

Plain bash has no hook that runs on every keystroke, so it gets no
suggestions. Other terminals ignore the sequences, so the scripts are safe
to keep in a shared dotfile.

<details>
<summary><strong>How suggestions work</strong></summary>
<br>

On every redraw of the command line, the zsh script sends nora the line
and the cursor position in an escape sequence (`OSC 6973`). When a command
runs, it also sends the command line and later its exit status. The
encoding runs inside zsh, with no process started per keystroke.

nora suggests from the commands you have run. Each one is scored by
frequency and recency. The score is tripled for commands run in the
current directory and lowered for commands that failed. With no history
match, nora completes the last word as a path or command name, but only
when the completion is unambiguous and contains no blank or shell-special
character (such as `;`, `|`, `$` or a quote). Path lookups run off the
I/O thread, so a slow network mount never stalls typing.

A suggestion appears only with the cursor at the end of the line, never
in full-screen programs or during a paste.

The design and its alternatives are in
[ADR-013](docs/adr/ADR-013-autocomplete.md).

</details>

<details>
<summary><strong>Privacy</strong></summary>
<br>

Everything stays on your machine. The history lives in
`~/.local/state/nora/history` (or `$XDG_STATE_HOME/nora/history`),
readable only by you and capped at 5,000 commands.

These commands are never recorded:

- commands starting with a space, the same convention as zsh's
  `HIST_IGNORE_SPACE`;
- multi-line commands;
- commands that zsh keeps out of your history file: those matching
  `HISTORY_IGNORE`, and those your `zshaddhistory` hooks reject with any
  non-zero status (with a `zshaddhistory` function, this needs zsh 5.8 or
  later).

Set `enabled = false` under `[autocomplete]` to turn suggestions off; nora
then neither reads nor writes the file.

</details>

## Performance

Measured on one machine (x86_64, AMD RX 6750 XT with RADV, GNOME Wayland)
unless noted. These are evidence, not guarantees.

| Measure | Result |
| --- | --- |
| Start to first frame | about 180 ms (`nora -- true`: 204–214 ms wall time) |
| Key press to frame presented | about 1 ms median (Xvfb, software GPU) |
| Key press to suggestion on screen | 1.4 ms median, 3.5 ms max (Xvfb, software GPU) |
| Output throughput, 32 MiB with colors | 70–80 MiB/s at 80×24 with 10,000 lines of scrollback (Xvfb, software GPU) |
| Allocations per frame | about 180 for a full 120×35 screen, 170 for a typed character |
| Idle | no frames drawn; the I/O thread wakes twice a second |

The method and raw numbers are in [docs/performance/](docs/performance/).

## Project status

nora is **early (0.1, pre-release)**. Linux is the supported platform and
the one it is used on every day. Expect rough edges.

Not supported yet:

- font ligatures;
- kitty graphics animation and relative placements (still images and
  video work);
- macOS and Windows. Their builds pass CI, but the windowed app has not
  been used on them, and work on them is deferred
  ([docs/15-platforms.md](docs/15-platforms.md)).

Not yet checked by a test:

- **Screen readers:** checked over AT-SPI2 (text, caret, live updates),
  but Orca's speech has not been checked in a real session.
- **Input methods:** checked end to end with ibus on X11. fcitx5, the
  Wayland text-input path and the candidate window's drawing are not.
- **Wayland clipboard:** native, text only, with no PRIMARY selection.
  Pasting another program's selection is tested on a headless compositor.
  Copying from nora on Wayland needs a real key press, so no test covers
  it yet.

Design decisions are recorded as ADRs in [docs/adr/](docs/adr/), and the
compatibility matrix is in
[docs/06-compatibility.md](docs/06-compatibility.md).

## Contributing

Issues and pull requests are welcome. For a larger change, please open an
issue first so we can agree on the approach. [CONTRIBUTING.md](CONTRIBUTING.md)
covers the checks to run, the window tests and how the code is organized.

## Security

Please report security problems privately through
[GitHub security advisories](https://github.com/Yejashi/nora/security/advisories/new),
not in a public issue. See [SECURITY.md](SECURITY.md) for what nora treats
as untrusted.

## Acknowledgements

- [Alacritty](https://github.com/alacritty/alacritty)'s `alacritty_terminal`
  (Apache-2.0) is nora's terminal engine: escape-sequence parsing, the
  screen model and the PTY layer.
- [wgpu](https://wgpu.rs), [winit](https://github.com/rust-windowing/winit),
  [glyphon](https://github.com/grovesNL/glyphon),
  [cosmic-text](https://github.com/pop-os/cosmic-text) and
  [AccessKit](https://accesskit.dev) provide rendering, windowing, text
  shaping and accessibility.
- The suggestion design draws on
  [zsh-autosuggestions](https://github.com/zsh-users/zsh-autosuggestions),
  fish's autosuggestions and kitty's
  [shell integration](https://sw.kovidgoyal.net/kitty/shell-integration/).

## License

nora is released under the [MIT License](LICENSE), © 2026 Yejashi.

The vendored `alacritty_terminal` in
[`vendor/alacritty_terminal`](vendor/alacritty_terminal) remains under the
Apache License 2.0 ([its license](vendor/alacritty_terminal/LICENSE-APACHE)).

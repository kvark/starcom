# Desktop client

The first desktop increment is **read-only**. It attaches to an existing session,
reconstructs independent terminal models, and displays live output. It does not
send keystrokes, paste into remote applications, or resize remote panes. This is
part of M2, not completion of interactive-terminal acceptance.

## Start

```sh
cargo run --release --locked
cargo run --release --locked -- --demo
```

Without arguments Starcom opens the connection form. Enter a real hostname, user,
and existing tmux session. Choose your SSH agent or an unencrypted private-key
file. Encrypted keys can be unlocked in an agent before connecting. Under **SSH
options**, set the known-hosts file, port, non-default remote tmux socket, and
history budget. `~/` in local file paths expands to the local home directory.

Settings are explicit and are not persisted yet. SSH config aliases, jump hosts,
interactive password/MFA prompts, and a host-key acceptance dialog remain future
work. Unknown or changed host keys fail instead of being accepted automatically.
See [SSH.md](SSH.md) for the transport's exact trust-policy support.

The demo never connects to a host. Its text and apparent command output are
synthetic fixtures for exploring the interface and testing rendering.

## Use

- Select a window tab to see its panes. Drag a divider to allocate **local viewing
  space only**. Remote rows/columns and other clients' geometry stay unchanged;
  use horizontal scrolling when the existing terminal is wider than its view.
- Scroll with the wheel, drag to select text, double-click to select a word, or
  triple-click to select a line. Copy with the toolbar, right-click menu, or
  the platform's Copy shortcut (Ctrl+C / Command+C). Copy is limited to 1 MiB.
- Use the font-size controls for terminal text. Disconnect leaves the last view
  readable and copyable, explicitly marked stale. **Connect** retries manually;
  no automatic SSH reconnection is implied by a retained screen.

Selection is kept in Alacritty's model so its anchors follow incoming scrolls;
copying handles combining characters, wide cells, and soft wraps. The initial
renderer uses egui's bundled fonts, not platform-wide font discovery. Missing
font glyphs, complex shaping, bidi text, accessibility, and native IME acceptance
need further work. Since remote input is disabled, there is no application-mouse
capture or terminal-input IME path in this increment.

Only visible rows are painted. The worker performs SSH and snapshot operations
outside the UI/model lock, and wakes the window only when state/output changes.
The event loop waits between requested redraws; there is no permanent frame loop.
These are implementation properties, **not measured performance comparisons**.

## Limits

The UI uses the same experimental restoration API as `starcom-inspect --watch`.
It rebuilds models after layout invalidation. It is not a universal tmux
checkpoint: unexported parser/pen state and version-specific behavior still have
the limits described in [SYNCHRONIZATION.md](SYNCHRONIZATION.md). Remote process
survival alone does not establish perfect terminal reconstruction.

A full-screen application's conversation history is not necessarily terminal
scrollback. Retained output is bounded on both the server and client; Starcom
cannot reconstruct discarded content. Opening several hosts simultaneously and
saving connection profiles are not part of this increment.

## Build and test

The default features include the desktop and embedded SSH. The original tools
remain usable without a graphical build:

```sh
cargo test --locked --no-default-features --all-targets
cargo run --locked --no-default-features -- --replay tests/data/two-panes.tmux
cargo run --locked --no-default-features --features ssh --bin starcom-inspect -- --help
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

The GUI follows FileMan's matching Blade git revision `f1fbf2a`, egui/egui-winit
0.34, and winit 0.30. It does not use eframe, GPUI, a web view, or a local PTY.
Linux's default Blade backend requires Vulkan; macOS uses Metal. Windows needs
an appropriate Blade graphics driver. A normal platform Rust linker is required,
but there is no OpenSSL/Perl/native crypto build setup. Linux window-system
link discovery may use `pkg-config`. Software-rendered Linux CI additionally
installs Mesa Vulkan drivers, Xvfb, and `python3-xlib`.

Generate an image of the real desktop UI, without a display server or SSH:

```sh
cargo run --locked -- --demo --snapshot target/test-artifacts/desktop.png
```

The snapshot renders the same widgets through Blade into an sRGB target; it is
not an artist's mock-up. Snapshot export requires `--demo` so it cannot silently
capture a private session. The README image is a palette-compressed copy of this
output. This is a visual aid, not a claim of completed native OS acceptance.

UI replay tests exercise actual drag selection, clipboard output, viewport-only
resizing, visible-row bounds, and rejection of terminal input. Linux's isolated
SSH fixture also tests desktop-worker publication, cancellation,
and preservation of remote processes. `scripts/test-desktop.py` exercises a
real Linux/X11 window and system clipboard under Xvfb. Native macOS/Windows,
Wayland, IME, and varied-DPI manual smoke tests remain outstanding.

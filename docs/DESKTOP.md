# Desktop client

Starcom has an experimental **interactive** desktop client. It attaches to an
existing tmux session, reconstructs each pane into an Alacritty terminal model,
and presents tmux windows and panes through Blade/egui.

The implementation is usable enough for focused testing, but it is not yet a
finished terminal: automatic reconnect, application mouse reporting, broad TUI
compatibility, and native macOS/Windows interaction acceptance remain open.

## Start

```sh
cargo run --release --locked
cargo run --release --locked -- --demo
```

The demo is local synthetic data. It does not open SSH, read credentials, or
attach to a tmux server.

## Connection tabs

A Starcom tab owns one connection form, one SSH/tmux client, one terminal view,
and its pending input tokens. Use **+** or Ctrl-Shift-T (Cmd-T on macOS) to create
a connection tab. A new tab displays its connection form—not panes from another
session and not a sidebar beside existing panes.

After a successful connection, the terminal workspace replaces the form in that
tab. **Connection settings** returns to the form without moving it into another
tab. Closing a tab detaches that Starcom client; the tmux server and remote jobs
continue running. Ctrl-Shift-W/Cmd-W closes the active tab.

Each tab currently opens its own SSH connection. Reusing a host connection across
multiple session tabs is deferred until it can be done without coupling failures
or authentication state between tabs.

## SSH configuration

The host field suggests literal `Host` aliases discovered from `~/.ssh/config`.
Typing filters the suggestions; selecting one resolves the supported profile
settings into the form.

Currently supported:

- `Host` patterns and negation, with OpenSSH-style first-value-wins ordering;
- `HostName`, `User`, and `Port`;
- one `IdentityFile` and `IdentitiesOnly`;
- one `UserKnownHostsFile`;
- bounded `Include` files and `*`/`?` include globs.

Wildcard `Host` entries apply defaults but are not shown as literal suggestions.
The parser never executes configuration commands. `Match`, `ProxyJump`,
`ProxyCommand`, certificates, custom agents, multiple identity files, algorithm
overrides, and other routing/security policy are displayed as blockers. Starcom
does not silently bypass them by connecting directly or choosing another key.

Use **Reload config** after editing the file. Settings are not persisted yet.
Unknown or changed host keys fail instead of being accepted automatically. See
[SSH.md](SSH.md) for the exact trust and authentication policy.

## Terminal input

Enable **Allow terminal input** before connecting for an interactive attachment.
A read-only attachment remains available for inspection.

Click a pane to focus it. Text and committed IME input are encoded as UTF-8.
Enter, arrows, Home/End, Page Up/Down, Insert/Delete, Tab, Escape, Backspace, and
F1-F20 are sent through tmux's key handling. Plain terminal control combinations
such as Ctrl-C, Ctrl-X, Ctrl-V, and Ctrl-Z remain application input.

Local clipboard shortcuts are:

- Ctrl-Shift-C / Ctrl-Shift-V on Linux and Windows;
- Cmd-C / Cmd-V on macOS;
- the **Copy selection** and **Paste** buttons;
- the pane context menu for copying.

Single-line clipboard data can be submitted immediately. Multiline paste opens a
confirmation dialog with a bounded preview. Paste rejects escape/C1 and other
control characters except tabs and line endings; deliberate control input must
come from key handling, not clipboard text.

Input is bound to the exact connection epoch, reconstructed-view generation, and
pane identity in which it originated. Layout changes, reconnects, tab changes, or
cancellation invalidate stale actions. Starcom does not queue input while offline
and never automatically retries a request whose delivery is uncertain.

`send-keys` can broadcast when a tmux window has `synchronize-panes` enabled.
Starcom detects that state in the server-side action guard and blocks the action
rather than changing the user's option or risking broadcast.

## Scrolling, selection, and copying

Wheel scrolling examines local terminal history. Drag to select, double-click for
a word, and triple-click for a line. Selection anchors live in the terminal model,
so they follow incoming scrolls. Copying handles wide cells, combining characters,
and soft wraps, with a 1 MiB output limit.

The renderer paints only visible history rows. Font-size controls affect local
presentation and do not resize the remote terminal.

Application mouse reporting is not implemented yet. Local selection and scrolling
therefore remain the only mouse behavior; a future implementation must provide an
obvious override rather than trapping the user in application mouse mode.

## Pane dividers

Dragging a divider always changes the local allocation while the pointer moves.
By default, this does **not** change tmux geometry.

For an interactive, synchronized session, enable **Resize remote panes** to opt in.
On release, Starcom submits a guarded one-axis `resize-pane`, invalidates the old
models, and reconstructs from the server's resulting layout. Other attached tmux
clients see this shared change. Nested or unusual layouts that cannot be mapped to
a safe tmux boundary remain local-only.

Starcom never changes tmux's global sizing options. Zoomed windows, stale geometry,
panes in modes, dead/input-disabled panes, and changed session/window identities
block the resize transaction.

## Disconnect and exit behavior

Disconnecting preserves the last received view for reading and copying, marks it
stale, and disables input. Manual reconnect currently starts a new attachment;
automatic retry/backoff is M3.

Window closure disables repaint callbacks, invalidates and wakes all workers, and
releases Blade surface/painter/encoder resources while the winit event loop and
native display handles are still alive. The Linux/X11 smoke test closes through
`WM_DELETE_WINDOW` and requires a zero exit status. This verifies the tested X11
path; native Wayland, macOS, and Windows close-path acceptance is still needed.

## Rendering and terminal limits

The GUI uses Blade, blade-egui, egui 0.34, egui-winit 0.34, and winit 0.30. It does
not use eframe, GPUI, a web view, Alacritty's renderer, or a local PTY.

Initial rendering uses egui's bundled fonts. Platform font discovery, complex
shaping, bidi text, accessibility, and broad IME testing remain future work.
Starcom's snapshot is also not a complete tmux terminal checkpoint; see
[SYNCHRONIZATION.md](SYNCHRONIZATION.md) for unexported parser/pen state and
history limitations.

A full-screen application's conversation history is not necessarily terminal
scrollback. Both tmux and Starcom retain bounded history and cannot recover data
that the server discarded.

## Build and validation

The default features include desktop and embedded SSH:

```sh
cargo fmt --check
python scripts/check-dependencies.py
cargo build --locked --all-features --all-targets
cargo test --locked --all-features --all-targets
cargo clippy --locked --all-features --all-targets -- -D warnings
```

The headless paths remain available:

```sh
cargo test --locked --no-default-features --all-targets
cargo run --locked --no-default-features -- --replay tests/data/two-panes.tmux
cargo run --locked --no-default-features --features ssh --bin starcom-inspect -- --help
```

Linux CI runs a disposable loopback sshd and isolated tmux server. It exercises
real interactive input, paste-once behavior, pane resizing/resynchronization,
`synchronize-panes` blocking, process preservation, snapshot continuity, and
timeouts. A native X11/Xvfb test covers rendering, selection, the system clipboard,
window resize, and normal-window clean close. Windows CI tests its OpenSSH-agent
named pipe. macOS and Windows compile/test the full desktop but do not yet have
native GUI automation.

Generate the real application-rendered demo image without SSH or a display server:

```sh
cargo run --locked -- --demo --snapshot target/test-artifacts/desktop.png
```

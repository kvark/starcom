# Desktop client

Starcom has an experimental **interactive** desktop client. It attaches to an
existing tmux session, reconstructs each pane into an Alacritty terminal model,
and presents tmux windows and panes through Blade/egui.

The implementation is usable enough for focused testing, but it is not yet a
finished terminal: full application mouse (beyond wheel), broad TUI
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
tab. A failed first attach stays on the form and shows why — the toolbar phrase
"Connection failed" is not the whole message. **Exit** always returns to the form
in the same tab, including after a failed attach. Ctrl-Shift-W/Cmd-W closes the
tab and detaches that Starcom client; the tmux server and remote jobs continue
running.

A Starcom tab is one tmux session and shows one window of that session. A
window picker is not in this increment.

Each tab currently opens its own SSH connection. Reusing a host connection across
multiple session tabs is deferred until it can be done without coupling failures
or authentication state between tabs.

## SSH configuration

The form's main choice is the host. Literal `Host` aliases from `~/.ssh/config`
are listed for one-click selection; the field after those buttons accepts a
hostname, address, or alias that is not in that list. Selecting a known host
resolves the supported
profile and lists that host's tmux sessions, selecting the first so **Connect**
is available immediately. Restored tabs do not list or authenticate on their
own.

Currently supported:

- `Host` patterns and negation, with OpenSSH-style first-value-wins ordering;
- `HostName`, `User`, and `Port`;
- one `IdentityFile` and `IdentitiesOnly`, or else `~/.ssh/id_ed25519` /
  `id_ecdsa` / `id_rsa` when those default files exist;
- one `UserKnownHostsFile`;
- `ProxyJump`, including a hop written as `user@host:port`;
- bounded `Include` files and `*`/`?` include globs.

Wildcard `Host` entries apply defaults but are not shown as literal suggestions.
The parser never executes configuration commands. `Match`, `ProxyCommand`,
certificates, custom agents, multiple identity files, algorithm overrides, and
other routing/security policy are displayed as blockers. Starcom does not
silently bypass them by connecting directly or choosing another key.

A profile with a `ProxyJump` shows its route under the resolved endpoint, so a
bastion is never invisible on the screen that starts the connection. A restored
tab re-reads the route and the blockers from the config rather than restoring
them: a saved tab records where you chose to connect, and must not become a way
to skip a bastion that has since been added, or to reach a host on terms the
config no longer allows. See [SSH.md](SSH.md) for how each hop is trusted.

Use **Reload config** after editing the file. Unknown or changed host keys fail
instead of being accepted automatically. See [SSH.md](SSH.md) for the exact trust
and authentication policy.

## Saved tabs

Open tabs are saved to `~/.config/starcom/workspace.conf` (`%APPDATA%\starcom\`
on Windows, or `$XDG_CONFIG_HOME` where it is set) and reopened next time.

What is saved is where a tab points and how it should connect: destination alias,
host, user, port, session name, tmux socket, history depth, whether it is
interactive, whether it reconnects, whether it uses the agent or an identity
file, and the global redraw cap (`fps`). Nothing that would let a reader of that file connect is written: no keys,
no passphrases, no host-key material, and no terminal contents. An identity entry
is the path you already chose, never the key behind it.

Restored tabs open on their connection form with the fields filled in. Starcom
does not connect or authenticate at startup — you press **Connect**, exactly as
on a cold start. A saved file that cannot be read is reported and left untouched
rather than replaced with a guess.

The demo neither reads nor writes this file.

## Finding and creating sessions

Selecting a known host lists its sessions automatically. **Refresh** asks again.
The query runs `tmux -N`, so it can never bring a tmux server into existence: a
host with no tmux running says so. The first session is selected; choosing
another only fills the field, and double-clicking attaches.

Starcom also asks on its own when a connection fails because that session does
not exist. You have already asked to connect and already authenticated, and the
list is exactly the missing information. No other failure triggers it —
authentication and host-key failures could not list anyway, and the others
already say what happened.

**Create session** makes the named session on the host, after a confirmation that
says plainly that this starts a tmux server if none is running. It is the only
path in Starcom that may start one, it is reachable only from that button, and it
leaves the new session detached — you still press **Connect** to attach. No
failure anywhere else falls back to it: an attach that cannot find its session
still fails, exactly as before.

Both run on the tab's worker over their own short-lived connection, so neither
blocks the window or disturbs a live attachment.

## Terminal input

Enable **Allow terminal input** before connecting for an interactive attachment.
A read-only attachment remains available for inspection.

Selecting a Host alias that has no `IdentityFile` uses **SSH agent** when one is
reachable. If none is, Starcom fills **Key file** from the first existing default
identity (`~/.ssh/id_ed25519`, then `id_ecdsa`, then `id_rsa`) — the same files
`ssh` would offer without an agent. If **SSH agent** stays selected and no agent
is reachable, the form says so before you connect. A desktop session often does
not inherit `SSH_AUTH_SOCK` from a shell, so an agent that works in your terminal
may be invisible here; start an agent in the session that launches Starcom, or
choose a key file. The warning clears on its own once an agent appears; there is
no need to restart Starcom.

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

Wheel handling follows the pane, not a global shortcut. If the application
enabled mouse reporting (DEC 1000/1002/1003), the wheel is sent as a mouse
report at the hovered cell. If it is on the alternate screen without mouse
reporting, the wheel becomes Up/Down, matching xterm alternate-scroll. Otherwise
the wheel examines local terminal history.

Drag to select, double-click for a word, and triple-click for a line. Selection
anchors live in the terminal model, so they follow incoming scrolls. Copying
handles wide cells, combining characters, and soft wraps, with a 1 MiB output
limit. Full application mouse (clicks and drags forwarded to the program) is
not implemented yet; local selection still wins those.

The renderer paints only visible history rows. Font-size controls change the
cell metrics used both to paint and to tell tmux this client's size, so the
remote grid matches the glyphs on screen.

The status bar's bottom-left spinner ticks on each workspace repaint, the same
liveness mark FileMan uses. Next to it, when a recent small tmux command has
finished, its round-trip time is shown. That is not a probe: nothing extra is
sent to measure it.

Remote pane output is redrawn at most 5 times per second by default (the **fps**
control on the tab bar, saved in `workspace.conf`). Buttons, hover, typing, and
other local UI stay immediate.

## Pane controls

Each interactive pane has window-style buttons in its top-right corner:

- **│** split right (`split-window -h`)
- **─** split below (`split-window -v`)
- **□** maximize / restore (`resize-pane -Z`)
- **×** close the pane (`kill-pane`), hidden when it is the last pane in the
  window

Click a pane to focus it (`select-pane`). These are the same tmux operations an
ordinary client would use, so other attached clients see them.

## Pane dividers

Dragging a divider previews the split locally. On release, an interactive
connection sends `resize-pane` to tmux — the same shared layout change a normal
tmux client would make — then reconstructs from the server's geometry. Cells
stay at the real font size; they are not scaled. Uncheck **Resize remote panes**
if this attachment must not change the layout others are using.

Nested or unusual layouts that cannot be mapped to a safe tmux boundary stay
local-only.

Starcom never changes tmux's global sizing options. Zoomed windows, stale geometry,
panes in modes, dead/input-disabled panes, and changed session/window identities
block the resize transaction.

## Disconnect and exit behavior

**Exit** preserves the last received view for reading and copying, marks it
stale, and disables input. It is available in every connection phase, including
**Connection failed**.

**Reconnect automatically after connection loss** is on by default in the
connection form. Only transport loss is retried. Authentication failures, host-key
failures, a missing or destroyed session, a tmux server that exited, and an
explicit detach all stop and wait for you, because retrying any of them would
either loop on a security decision or quietly attach somewhere else.

Each retry waits a little longer, up to 30 seconds, with jitter so several tabs
that drop together do not reconnect in lockstep. The status bar shows the attempt
number and the remaining wait, and **Stop reconnecting** ends the schedule at any
time. A successful attachment resets the schedule.

While disconnected, the last view stays readable and copyable, but no input token
is issued: nothing typed during an outage is queued for later delivery, and a
write whose delivery was uncertain is never repeated. Each attempt reattaches
under a new connection epoch and rebuilds every pane model from a fresh snapshot.

Because attaching resolves the session by name, a restarted tmux server can hand
back a different session that merely shares that name. Starcom compares the
session identity across the reconnect and says so when it changed. It also
reports when tmux could not supply the full scrollback, so a shorter view after
reconnecting reads as a known limit rather than as lost output.

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

## Linux desktop integration

`make install` builds a release binary and installs it with `etc/starcom.desktop`
and `etc/starcom.svg` under `PREFIX` (`~/.local` by default, or `/usr` for a
system package). `StartupWMClass=starcom` matches the X11/Wayland class the
window sets. The same files are packed into the `.deb` and `.rpm` release
artifacts.

Window chrome uses `etc/macos/icon.png`; the Windows PE resource uses
`etc/windows/icon.ico`. Regenerate both from `scripts/generate-icons.py` if the
SVG geometry changes. macOS `.app` / `.dmg` and the Linux AppImage come from
`cargo-bundle` metadata in `Cargo.toml`. GitHub Release jobs run on `v*` tags;
those builds are not notarized or Authenticode-signed.

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
`synchronize-panes` blocking, process preservation, snapshot continuity,
automatic reconnection after an abrupt transport loss, refusal to reattach to a
destroyed session, and timeouts. The fixture asserts how many tests actually ran,
so a renamed or cfg-excluded test fails instead of passing silently. A native X11/Xvfb test covers rendering, selection, the system clipboard,
window resize, and normal-window clean close. Windows CI tests its OpenSSH-agent
named pipe. macOS and Windows compile/test the full desktop but do not yet have
native GUI automation.

Generate the real application-rendered demo image without SSH or a display server:

```sh
cargo run --locked -- --demo --snapshot target/test-artifacts/desktop.png
```

# Starcom

**Session Terminal And Remote COMmander.**

A small native client for persistent remote tmux sessions. Linux, macOS, and
Windows clients; Linux hosts with stock SSH and tmux. No remote Starcom service.
Built with Rust, Blade, egui, and Alacritty's terminal core.

![Starcom desktop attached to a remote tmux session](etc/screenshot.png)

## Install

```sh
cargo install --locked --git https://github.com/kvark/starcom
```

That builds the desktop client. Run `starcom` or `starcom --demo`. A recent stable
Rust is required (`rust-version` is 1.96).

## Run

From a checkout:

```sh
cargo run --release --locked
cargo run --release --locked -- --demo
```

Use **+** to open a connection tab. Pick a `Host` from `~/.ssh/config` or type
another destination; Starcom resolves supported user/host/port/key settings and
reports unsupported routing or authentication policy instead of bypassing it.
Choosing a known host lists its tmux sessions and selects the first one so
**Connect** is the next click. Each tab is one session and one window of that
session: its form or its panes, not both side by side.

Tabs are saved and reopened on their forms; Starcom never authenticates at
startup, and the saved file holds destinations, never credentials.
**Create session** is the one action that may start a tmux server, and it asks
first.

The desktop currently supports local scrollback, selection and copying, pane
split/zoom/close controls, and opt-in shared tmux pane resizing. Wheel events go
to the application when it asked for mouse reports or is on the alternate
screen; otherwise they scroll local history. Multiline paste requires
confirmation. Transport loss reconnects automatically with visible, cancellable
backoff; authentication, trust, and missing-session failures stop and wait for
you.

SSH and cryptography use Rust libraries; OpenSSL is not a build dependency.
Host keys must already be trusted.

[Desktop usage](docs/DESKTOP.md) · [SSH details](docs/SSH.md) ·
[Synchronization limits](docs/SYNCHRONIZATION.md) · [Roadmap](PLAN.md)

# Starcom

**Session Terminal And Remote COMmander.**

A small native client for persistent remote tmux sessions. Linux, macOS, and
Windows clients; Linux hosts with stock SSH and tmux. No remote Starcom service.
Built with Rust, Blade, egui, and Alacritty's terminal core.

![Starcom desktop showing two terminal panes](etc/screenshot.png)

*Application-rendered demo data, not a remote session.*

## Run

```sh
cargo run --release --locked
cargo run --release --locked -- --demo
```

Use **+** to open a connection tab. Starcom suggests literal `Host` entries from
`~/.ssh/config`, resolves supported user/host/port/key settings, and reports
unsupported routing or authentication policy instead of bypassing it. Each tab
shows either its connection form or its tmux windows—not both side by side.

Tabs are saved and reopened on their forms; Starcom never authenticates at
startup, and the saved file holds destinations, never credentials. **List
sessions** asks the host what exists without being able to start a tmux server;
**Create session** is the one action that may start one, and it asks first.

The desktop currently supports local scrollback, selection and copying, plus the
experimental interactive path and opt-in shared tmux pane resizing. Multiline
paste requires confirmation. Transport loss reconnects automatically with visible,
cancellable backoff; authentication, trust, and missing-session failures stop and
wait for you.

SSH and cryptography use Rust libraries; OpenSSL is not a build dependency.
Host keys must already be trusted.

[Desktop usage](docs/DESKTOP.md) · [SSH details](docs/SSH.md) ·
[Synchronization limits](docs/SYNCHRONIZATION.md) · [Roadmap](PLAN.md)

# Starcom

**Session Terminal And Remote COMmander.**

A small native client for persistent remote tmux sessions. Linux, macOS, and
Windows clients; Linux hosts with stock SSH and tmux. No remote Starcom service.
A sister project to [FileMan](https://github.com/kvark/fileman), built with Rust,
Blade, egui, and Alacritty's terminal core.

![Starcom desktop showing two terminal panes](etc/screenshot.png)

*Application-rendered demo data, not a remote session.*

## Run

```sh
cargo run --release --locked
# Explore without connecting:
cargo run --release --locked -- --demo
```

Requires Rust 1.96+, the platform Rust linker, and a supported graphics driver.
SSH and cryptography use Rust libraries; OpenSSL is not a build dependency.

Use **+** to open a connection tab. Choose an alias from your `~/.ssh/config`
and an existing tmux session. Each tab owns its connection; the connection form
replaces the terminal area rather than sitting beside it. Host keys must already
be trusted. Unsupported proxy/authentication options are reported, never bypassed.

Type into a focused pane; drag to select and scroll normally. Use the Copy/Paste
buttons, Ctrl-Shift-C/V, or Cmd-C/V on macOS. Multiline paste needs confirmation.
Enable **Resize remote panes** before dragging shared tmux dividers. Read-only
connections remain available. Automatic reconnect is not implemented yet.

[Desktop usage](docs/DESKTOP.md) · [SSH inspection](docs/SSH.md) ·
[Synchronization limits](docs/SYNCHRONIZATION.md) · [Roadmap](PLAN.md) ·
[Contributing](CONTRIBUTING.md)

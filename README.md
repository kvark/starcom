# Starcom

**Session Terminal And Remote COMmander.**

A small native client for persistent remote tmux sessions. Linux, macOS, and
Windows clients; Linux hosts with stock SSH and tmux. No remote Starcom service.
A sister project to [FileMan](https://github.com/kvark/fileman), built with Rust,
Blade, egui, and Alacritty's terminal core.

![Starcom desktop showing two terminal panes](etc/screenshot.png)

*Built-in demo data, rendered by the application; not a remote session.*

## Run

```sh
cargo run --release --locked
# Explore the interface without connecting:
cargo run --release --locked -- --demo
```

Requires Rust 1.96+, the platform Rust linker, and a supported graphics driver.
SSH and cryptography use Rust libraries; OpenSSL is not a build dependency.
Enter explicit SSH settings and an existing tmux session in the connection panel;
the host key must already be trusted.

**Early read-only desktop:** live panes, window tabs, local scrollback, selection,
copying, and draggable local views. Remote input, remote pane resizing, SSH config
aliases, and automatic reconnect are not implemented yet.

[Desktop usage](docs/DESKTOP.md) · [SSH inspection](docs/SSH.md) ·
[Synchronization limits](docs/SYNCHRONIZATION.md) · [Roadmap](PLAN.md) ·
[Contributing](CONTRIBUTING.md)

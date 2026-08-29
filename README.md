# Starcom

**Session Terminal And Remote COMmander.**

A small graphical client for persistent remote tmux sessions, targeting Linux,
macOS, and Windows. Remote hosts need only stock SSH and tmux on Linux—no
Starcom server.

A sister project to [FileMan](https://github.com/kvark/fileman): Rust,
Alacritty's terminal core, and a planned Blade + egui interface.

**In development:** embedded SSH inspection works; live terminal synchronization,
the GUI, and automatic reconnect are next.

## Try

Requires Rust 1.96+, an OpenSSL/C build setup, and an existing remote tmux session.

```sh
cargo run --locked --bin starcom-inspect -- \
  --host server.example.com --user your-user --session work \
  --known-hosts "$HOME/.ssh/known_hosts" --agent
```

Connection settings are explicit; SSH config aliases are not supported yet.
Host keys must already be trusted. See `--help` for other options.

[SSH usage](docs/SSH.md) · [Roadmap](PLAN.md) · [Contributing](CONTRIBUTING.md)

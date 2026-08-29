# Starcom

**Session Terminal And Remote COMmander.**

A small graphical client for persistent remote tmux sessions. Linux, macOS, and Windows
clients; Linux hosts with stock SSH and tmux. No remote Starcom service.

A sister project to [FileMan](https://github.com/kvark/fileman), using Rust and
Alacritty's terminal core, with Blade + egui planned for the interface.

**In development:** embedded SSH inspection and experimental snapshot-to-live
synchronization. The GUI, interactive input, and automatic reconnect are next.

## Try

Requires Rust 1.96+, an OpenSSL/C build setup, and an existing remote tmux session.

```sh
cargo run --locked --bin starcom-inspect -- \
  --host server.example.com --user your-user --session work \
  --known-hosts "$HOME/.ssh/known_hosts" --agent
```

Add `--watch 5` to collect live output into reconstructed pane models for five
seconds and print their final screens. Connection settings are explicit; SSH
config aliases are not supported yet. Host keys must already be trusted.

See [SSH usage](docs/SSH.md), [synchronization limits](docs/SYNCHRONIZATION.md),
[the roadmap](PLAN.md), and [contributing](CONTRIBUTING.md).

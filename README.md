# Starcom

**Session Terminal And Remote COMmander.**

A lightweight graphical client for persistent remote tmux sessions, built with
Rust, Blade, and egui. Linux, macOS, and Windows clients; Linux hosts with stock
OpenSSH and tmux. No custom remote server.

Starcom follows its sister project [FileMan](https://github.com/kvark/fileman):
simple UI, small dependencies, and speed over features.

## Status

Early development. Embedded SSH inspection and headless terminal replay work;
the GUI and interactive terminal restoration are next.

## Try it

Requires Rust 1.96+ and an existing remote tmux session.

```sh
cargo run --locked --bin starcom-inspect -- \
  --host server.example.com --user your-user --session work \
  --known-hosts "$HOME/.ssh/known_hosts" --agent
```

Use `--identity FILE` instead of `--agent` for an unencrypted key. Host keys must
already be trusted. SSH config aliases and jump hosts are not supported yet.
See [SSH setup](docs/SSH.md) for options and limitations.

For a network-free terminal replay:

```sh
cargo run --locked -- --replay tests/data/two-panes.tmux
```

## Development

```sh
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

[Roadmap](PLAN.md) · [Contributing](CONTRIBUTING.md)

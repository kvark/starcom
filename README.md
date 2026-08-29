# Starcom

**Session Terminal And Remote COMmander.**

A small graphical client for persistent remote tmux sessions. The client targets
Linux, macOS, and Windows; remote hosts initially target Linux with stock SSH
and tmux. No Starcom server, patched tmux, or additional listening port.

A sister project to [FileMan](https://github.com/kvark/fileman): Rust, simple
modules, few dependencies, and eventually Blade + egui for the desktop interface.

## Status

**M0 is implemented; M1 has a working read-only SSH inspection path. There is no
GUI, interactive remote terminal, or automatic reconnect loop yet.**

The foundation provides bounded tmux control framing, typed commands, connection
epochs, input gating, Alacritty terminal models, and synthetic replay. The new
embedded `ssh2` backend verifies host keys before authentication and attaches to
existing sessions through a non-PTY SSH channel. `starcom-inspect` discovers panes
and prints bounded, escaped text captures without sending pane input or resizing.

These captures are observations made at different times, **not atomic snapshots
or restored emulator state**. Correct snapshot/live-output synchronization is
still outstanding. See [PLAN.md](PLAN.md) for the overall roadmap and
[docs/SSH.md](docs/SSH.md) for current M1 scope, limitations, and next steps.

## Try live inspection

Requires Rust 1.96+, a C toolchain/OpenSSL development setup, and a Linux host
with an existing tmux session. Replace the example host, user, and session:

```sh
cargo run --locked --bin starcom-inspect -- \
  --host server.example.com --user your-user --session work \
  --known-hosts "$HOME/.ssh/known_hosts" --agent
```

Use `--identity /path/to/key` instead of `--agent` for an unencrypted private key.
For an encrypted key, load it into your SSH agent first. Trust must already be
established in the supplied known-hosts file; unknown or changed keys are refused.

This first transport does not read SSH config aliases, Include, or ProxyJump.
Pass settings explicitly. `--port`, `--socket`, `--history`, and `--timeout` are
available; run `--help` for details. No local `ssh` executable is used.

## Replay and tests

```sh
cargo run --locked -- --replay tests/data/two-panes.tmux
cargo test --locked --all-targets
cargo test --locked --no-default-features --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

Default features include SSH. `--no-default-features` retains the network-free
core. Linux live integration tests run through `scripts/test-ssh.sh`, which
creates and removes its own loopback sshd, keys, agent, and isolated tmux socket.
It requires OpenSSH, tmux, Python, and sudo. It never uses your default tmux server.

CI checks Windows, macOS, and Linux builds/tests, plus real Linux SSH/tmux tests.
Native desktop input, clipboard, IME, and real application workflows are not yet
implemented or validated. No performance claims are made.

## Development

Read [CONTRIBUTING.md](CONTRIBUTING.md) for FileMan-style organization and checks.
The project is one Rust 2024 package with reusable library modules. The GUI will
use FileMan's matching Blade/egui/winit dependency set, not GPUI or eframe.

## License

MIT. See [LICENSE](LICENSE).

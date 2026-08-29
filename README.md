# Starcom

**Session Terminal And Remote COMmander.**

A small graphical client for persistent remote tmux sessions. The client targets
Linux, macOS, and Windows; remote hosts initially target Linux with ordinary
OpenSSH and tmux. No Starcom server, patched tmux, or additional listening port.

Starcom is a sister project to [FileMan](https://github.com/kvark/fileman): Rust,
simple modules, few dependencies, and Blade + egui rather than a large
application framework.

## Status

**The headless foundation is implemented; the desktop application is not yet
usable.** There is no live SSH connection, GUI, or automatic reconnect loop yet.
See [PLAN.md](PLAN.md) for the architecture, decisions, acceptance criteria, and
ordered roadmap.

Implemented modules provide bounded tmux control-mode framing and command/reply
handling, typed command construction, connection epochs and input gating,
Alacritty terminal models, and a synthetic multi-pane replay CLI. Twenty-five
unit/integration tests run without tmux, SSH, a display, or a GPU. CI runs the
build, tests, Clippy, and formatting checks on Linux, macOS, and Windows.

Only three direct dependencies are currently needed: `alacritty_terminal`,
`tmuxctl` (without its runtime drivers), and `anyhow`. The embedded SSH backend
and matching FileMan GUI dependencies will be introduced with their respective
implementations, not as unused scaffolding.

## Try the foundation

Requires Rust 1.96 or newer (tmuxctl's minimum supported version).

```sh
cargo run --locked -- --replay tests/data/two-panes.tmux
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

The replay command prints escaped snapshots of two independent terminal models.
It uses fixed 80x24 panes unless `--size COLSxROWS` is specified. It is a
correctness harness, not a general capture importer: live topology changes,
initial screen/history restoration, and network transport are not implemented.
Use `--help` for the small CLI surface.

## Architecture

```text
Blade + egui GUI                  Linux host
        |                            |
terminal models                 ordinary sshd
        |                            |
tmux control protocol <--- SSH ---> tmux -C
                                     |
                              existing tmux server
                                     |
                              shells / applications
```

Use tmux's documented control mode, not its private Unix-domain IPC. The SSH
transport is independent of the protocol engine. Embedded SSH is the intended
primary transport; a system-SSH adapter may be provided for complex environments.

Reconnect must reconstruct the pane state before enabling input. A successful
SSH handshake is not a restored session, and uncertain keystrokes must never be
silently replayed. Those rules are already represented in the state model;
actual transport/recovery integration remains the next stage of work.

## Development

Read [CONTRIBUTING.md](CONTRIBUTING.md) for organization, style, and validation.
The project starts as one Rust 2024 package with reusable library modules and a
small executable. Keep network and GUI dependencies outside the protocol/state
modules, and preserve wire fixture bytes across platforms.

The next milestone is attaching to an existing tmux session over embedded SSH,
with verified host keys and correct initial topology/screen synchronization.
Blade/egui views, selection, and divider resizing follow that foundation.

## License

MIT. See [LICENSE](LICENSE).

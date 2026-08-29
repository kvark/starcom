# Starcom

**Session Terminal And Remote COMmander.**

A small graphical client for persistent remote tmux sessions. The client targets
Linux, macOS, and Windows; remote hosts initially target Linux with ordinary
OpenSSH and tmux. No Starcom server, patched tmux, or additional listening port.

Starcom is a sister project to [FileMan](https://github.com/kvark/fileman): Rust,
simple modules, few dependencies, and eventually Blade + egui rather than a large
application framework.

## Status

Early implementation. See [PLAN.md](PLAN.md) for the architecture, decisions,
acceptance criteria, and ordered roadmap. The initial code targets the headless
protocol/replay foundation; a usable GUI and live SSH attachment are subsequent
milestones, not existing features.

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

## Development

Read [CONTRIBUTING.md](CONTRIBUTING.md) for organization, style, and validation.
Do not treat roadmap checkboxes as shipped features or compile checks as runtime
validation on other operating systems.

## License

MIT. See [LICENSE](LICENSE).

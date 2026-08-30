# Starcom plan and roadmap

Updated: 2026-08-30.

**Current status:** M0 and M1 are implemented for the tested configuration. M2's
functional implementation is present: connection tabs, SSH-config discovery,
interactive terminal input, guarded paste, opt-in tmux pane resizing, and native
Blade/egui rendering. The M2 acceptance gate remains open until the real Codex/TUI
workflow and native macOS/Windows interaction are exercised. M3, trustworthy
automatic reconnection, is the next development milestone.

Development targets `main` directly. CI runs for pull requests and updates to
`main`. Routine milestone branches, generated recovery trees, and shadow copies
of the source are not part of the workflow; the application source lives in the
normal repository root.

## Product goal

Starcom is a small, fast, cross-platform GUI for persistent remote terminal
sessions. It replaces tmux's terminal UI—not tmux itself—with normal tabs, mouse
selection, scrolling, copying, paste confirmation, and draggable pane dividers.

The client targets Linux, macOS, and Windows. The remote side initially targets
Linux with stock OpenSSH and stock tmux. A user must be able to close Starcom or
fall back to an ordinary tmux client without affecting remote jobs.

Priorities, in order:

1. Never lose, duplicate, or silently redirect user input.
2. Recover a truthful terminal view after connection loss.
3. Require no Starcom daemon, patched tmux, extra port, or privileged install.
4. Keep startup, idle CPU, memory, dependencies, and UI complexity modest.
5. Add features only after the core shell/Codex workflow is dependable.

## Architecture

```text
Starcom client (Linux / macOS / Windows)
  winit event loop + Blade renderer
    egui connection tabs, forms, window tabs, dividers and dialogs
      one Alacritty terminal model per tmux pane
        snapshot/live coordinator and guarded input queue
          bounded tmux control-mode adapter (tmuxctl)
            embedded Sunset/RustCrypto SSH transport
                       |
                    encrypted SSH
                       |
Linux host: stock sshd -> stock tmux -C -> existing tmux server -> PTYs/apps
```

One Starcom connection tab owns one client worker and one tmux-session view.
Creating a tab opens a connection form; after attachment, the terminal workspace
replaces that form in the same tab. Panes are never mixed with another tab's
connection form or state.

The SSH channel is opened without a PTY and executes `tmux -N -C attach-session`
against an existing session. `-N` prevents accidental server creation. Control
mode provides pane output, topology notifications, and commands. A single tmux
session attachment carries all of its panes; there is no SSH connection per pane.

### Why not tmux's native socket protocol?

The native tmux client/server protocol is local Unix IPC and transfers operating-
system file descriptors. It is not a remote, cross-platform protocol. Bridging it
would require a custom Linux-side component and would recreate the deployment and
compatibility burden Starcom is intended to avoid. Documented `tmux -C` control
mode is the external frontend interface and works with the host's installed tmux.

## Decisions and invariants

| Area | Decision |
| --- | --- |
| Remote lifetime | Existing tmux server owns jobs and PTYs. |
| Remote deployment | Stock sshd and tmux only; no Starcom service. |
| GUI | Blade + blade-egui + egui + winit, following FileMan conventions. |
| Terminal model | `alacritty_terminal`; do not implement a VT emulator. |
| Control protocol | `tmuxctl` with runtime/spawn defaults disabled. |
| SSH | Embedded Sunset + RustCrypto; no libssh2/OpenSSL/ring/AWS-LC backend. |
| Advanced SSH escape hatch | Optional system-SSH transport remains a future option. |
| Repository | One Rust 2024 package; normal root sources on `main`. |

The following are correctness rules, not optional polish:

- Attach to an existing session. Never silently create a replacement session or
  tmux server after a failure.
- Verify the host key before loading a private key or contacting an agent. Unknown
  and changed keys block the connection.
- Bound config files, protocol lines, command replies, terminal allocations,
  stderr, pending actions, paste size, and network work per iteration.
- Every connection and reconstructed view has an epoch/generation. Late worker
  results and stale UI actions cannot target a newer session.
- Input is enabled only after a coherent snapshot is published. Nothing typed
  while disconnected is queued for later replay.
- A write with uncertain delivery is never retried automatically.
- Multiline paste requires explicit confirmation. Paste content is data, never
  concatenated into tmux command syntax.
- Targeted input is blocked when `synchronize-panes` could broadcast it. Starcom
  does not change that user's tmux option.
- Remote resizing is opt-in because it changes the shared tmux layout. The server's
  resulting geometry is authoritative and triggers reconstruction.
- Unsupported SSH routing, trust, or authentication directives are shown as
  blockers; Starcom does not quietly connect with different semantics.
- Closing a tab or the application detaches its client while leaving remote jobs
  alive. Graphics resources are released while the native event loop/display is
  still valid.

## Current implementation

### Session and terminal core

- Bounded tmux-control parsing and typed command construction.
- Fresh Alacritty models reconstructed from observable primary/alternate screens,
  bounded history, cursor, exported modes, tabs, margins, and pending bytes.
- A tested snapshot/live ordering boundary; live bytes in the same SSH packet as
  the final reply are retained.
- Model invalidation and full reconstruction after layout/session changes.
- Last disconnected view remains readable and copyable.

Stock tmux does not expose all terminal parser and drawing-pen state. These limits
remain documented in `docs/SYNCHRONIZATION.md`; Starcom must not claim a universal
terminal checkpoint.

### SSH and configuration

- Strict known-host verification with exact, port-qualified, and hashed entries.
- Ed25519, RSA/SHA-256, and ECDSA P-256 identity files; Unix and Windows OpenSSH
  agents; no password or MFA fallback yet.
- GUI discovery of literal `Host` aliases from `~/.ssh/config`, including bounded
  `Include` expansion and wildcard defaults.
- Supported resolution: `Host`, `HostName`, `User`, `Port`, one `IdentityFile`,
  `IdentitiesOnly`, and one `UserKnownHostsFile`.
- `Match`, jump/proxy routing, certificates, custom agents, and security-algorithm
  overrides are reported as unsupported rather than ignored.

### Desktop

- Independent connection tabs; `+` opens a new connection form.
- Per-session tmux-window tabs and pane layouts.
- Local scrollback, selection, word/line selection, copying, and font sizing.
- Interactive UTF-8/IME commits, conventional special keys, terminal Ctrl keys,
  clipboard paste, and multiline-paste confirmation.
- Local divider dragging and explicit opt-in shared tmux pane resizing.
- Event-driven redraw and a worker thread that does no network I/O under the UI
  model lock.
- Ordered shutdown and a native Linux/X11 close-path test.

## Milestones

### M0 — Reusable foundation: complete

- [x] One-package Rust layout, FileMan-style organization, and cross-platform CI.
- [x] Validated IDs/dimensions and safe tmux command construction.
- [x] I/O-independent `tmuxctl` adapter with bounded framing and reply lifecycle.
- [x] Connection epochs and no-offline-input state machine.
- [x] Per-pane Alacritty models and deterministic replay fixtures.

### M1 — Existing-session attachment: implemented, compatibility work remains

- [x] Rust-native embedded SSH, host-key verification, identity and agent auth.
- [x] Non-PTY control attachment to an existing tmux session only.
- [x] Topology discovery and observable snapshot-to-live reconstruction.
- [x] Isolated Linux sshd/tmux integration tests with operation deadlines.
- [ ] Test additional tmux versions and distributions.
- [ ] Exercise editors, monitoring TUIs, and Codex; catalog fidelity gaps.
- [ ] Decide which remaining unexported parser states require mitigation.

### M2 — Minimal interactive desktop: implemented, acceptance gate still open

- [x] Blade/egui/winit desktop with event-driven redraw.
- [x] Connection tabs whose form and terminal workspace replace one another.
- [x] SSH-config alias suggestions and fail-closed profile resolution.
- [x] Local scrollback, selection, copy, window tabs, and pane layouts.
- [x] Guarded keyboard input and paste; no stale/offline replay.
- [x] Opt-in remote divider resizing followed by server-authoritative resync.
- [x] Native Linux/X11 render, clipboard, resize, and clean-close smoke test.
- [x] Real localhost SSH/tmux test for input, paste-once, resize, and broadcast guard.
- [ ] Native macOS and Windows GUI interaction/close tests.
- [ ] Wayland, IME, high-DPI, application mouse, and real Codex acceptance.

Gate: normal shell and Codex work should be comfortable without tmux keybindings,
and closing/reopening the client must not affect jobs or misdirect input.

### M3 — Trustworthy automatic reconnection: next

- [ ] Classify transient transport loss separately from auth, trust, missing-session,
  tmux-restart, and explicit user detach.
- [ ] Cancellable exponential backoff with jitter and visible retry state.
- [ ] Reattach and reconstruct fresh models for every new epoch.
- [ ] Preserve the last view but never queue offline input or replay uncertain writes.
- [ ] Test connection loss during output, input, paste, and remote layout changes.
- [ ] Expose history truncation and server/session replacement clearly.

Gate: repeated network loss produces no duplicate command, hidden gap, stale-pane
write, endless security retry, or phantom successful reconnection.

### M4 — Multiple-machine operational fit

- [ ] Persist non-secret tabs/profiles and restore them without auto-authentication.
- [ ] Reuse one SSH connection per host where the SSH backend supports it safely.
- [ ] Add ProxyJump/bastion support or a system-SSH adapter for advanced configs.
- [ ] Improve certificate, hardware-key, custom-agent, and MFA workflows.
- [ ] Add explicit session discovery/creation UI without changing attach semantics.

### M5 — Performance and release hardening

- [ ] Measure startup, clean-build time, binary size, idle CPU, RAM per pane/history,
  sustained-output throughput, and input latency.
- [ ] Stress many panes, long lines, slow readers, reconnects, and repeated resizes.
- [ ] Fuzz hostile control/config transcripts and audit terminal side effects.
- [ ] Publish a tested tmux/platform compatibility matrix.
- [ ] Package signed Linux, macOS, and Windows builds.

## Immediate work order

1. Implement M3 reconnect classification, scheduler, and UI states.
2. Run the original Codex workflow and representative shell/editor/TUI sessions.
3. Add native macOS/Windows close, clipboard, and input acceptance.
4. Extend SSH configuration only where semantics can be tested end to end.
5. Measure before optimizing the renderer or replacing dependencies.

## Validation policy

Every update must keep these paths green:

```sh
cargo fmt --check
python scripts/check-dependencies.py
cargo build --locked --all-features --all-targets
cargo test --locked --all-features --all-targets
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo test --locked --no-default-features --all-targets
cargo test --locked --no-default-features --features ssh --all-targets
```

Linux CI additionally creates a disposable loopback sshd and isolated tmux socket.
It must never touch the user's default tmux server. Native Linux/X11 automation
covers rendering, system clipboard, window resize, and normal WM close. Windows
CI exercises the native OpenSSH-agent named pipe. Compilation is not considered a
substitute for native interaction testing on platforms where that test is absent.

## Non-goals for the first useful release

SFTP/file management, Git UI, agent orchestration, plugins, collaboration,
terminal graphics protocols, a custom multiplexer, and a custom remote daemon are
out of scope. A full-screen program's conversation history is not necessarily
terminal scrollback, and Starcom cannot reconstruct output tmux has discarded.

## References

- [tmux control mode](https://github.com/tmux/tmux/wiki/Control-Mode)
- [tmux native client](https://github.com/tmux/tmux/blob/master/client.c)
- [tmux control implementation](https://github.com/tmux/tmux/blob/master/control.c)
- [`tmuxctl`](https://docs.rs/tmuxctl/latest/tmuxctl/)
- [`alacritty_terminal`](https://docs.rs/alacritty_terminal/latest/alacritty_terminal/)
- [Sunset](https://docs.rs/sunset/0.6.0/sunset/)
- [OpenSSH configuration](https://man.openbsd.org/ssh_config)
- [FileMan](https://github.com/kvark/fileman)

# Starcom: architecture and implementation roadmap

Date: 2026-08-29. Status: M0 headless foundation implemented; M1 is next.

This document records the decisions behind **Session Terminal And Remote
COMmander**. It is the working plan, not a claim that the listed capabilities
already exist. Keep acceptance checkboxes and implementation notes current.

## 1. Product goal

A small, fast GUI for managing persistent terminal sessions across several
machines. Normal mouse selection, scrolling, copying, and draggable splits should
replace the need to learn tmux's terminal UI. Connections recover without losing
remote jobs or silently repeating input.

The client targets **Linux, macOS, and Windows**. The remote side initially
targets **Linux with stock OpenSSH and tmux**. No Starcom server, custom remote
agent, patched tmux, root access, UDP requirement, or extra listening port.
Existing sessions must remain attachable with an ordinary tmux client.

Speed, low idle CPU, modest memory, a small dependency surface, and simple
implementation take priority over visual effects and workbench features.
Persistence covers client exits and network failures while the tmux server and
host remain alive; it does not promise survival of a host reboot or recovery of
output that tmux has discarded.

## 2. Decisions we are keeping

| Area | Decision | Reason |
| --- | --- | --- |
| Remote process ownership | Existing tmux server | Deployment and interoperability are more valuable than owning another daemon. |
| External tmux interface | Documented control mode, `tmux -C` | Intended for external frontends; carries commands, per-pane output, and topology notifications. |
| Transport | Embedded SSH is the intended primary backend | Cross-platform packaging and connection/authentication UI under our control. |
| Escape hatch | Optional system-SSH backend | Reuse complicated enterprise configurations without changing the protocol core. |
| GUI | Blade + blade-egui + egui + winit | Reuse FileMan experience; no GPUI, web frontend, or eframe renderer. |
| Terminal state | `alacritty_terminal` | Reuse emulation, grid, modes, history, and selection; do not write a VT emulator. |
| Control parser | Audit and use `tmuxctl` with default features disabled | Its I/O-independent engine fits both embedded and external SSH. Keep a narrow adapter. |
| Repository | One Rust 2024 package initially | Follow FileMan; separate library modules, not a speculative framework. |

### Why not the native tmux socket protocol?

The private native protocol is local Unix IPC. It transfers real file descriptors
and has a versioned internal message protocol. A Windows/macOS client cannot
transfer one of its descriptors into a remote Linux kernel. Forwarding socket
bytes does not forward the required descriptor semantics. A custom bridge would
reintroduce remote deployment and compatibility work.

The installed `tmux -C` client already establishes the required local attachment.
The server's control implementation uses the passed input/output descriptors;
this is not a new tmux invocation for every key or a required extra userspace
relay for each output byte. Control-mode escaping/framing still has overhead and
must be measured. This decision is independent of whether SSH is embedded or
launched as a subprocess. See references [1]-[3].

## 3. Scope and non-goals

The first useful release attaches to existing sessions, displays windows and
panes as GUI elements, supports local scrollback/selection/copy, resizes panes,
and restores a truthful usable view after a transient connection failure.

Explicitly deferred: SFTP/file browsing, Git UI, agent orchestration, shell
completion, plugins, synchronized collaboration, terminal graphics protocols,
and a custom remote multiplexer. Do not wrap Codex or another application just
to make the terminal work. A full-screen program's historical conversation is
not necessarily present in terminal scrollback.

## 4. Architecture and ownership

```text
Local client (Linux / macOS / Windows)
  winit event loop + Blade renderer
    egui host/session sidebar, tabs, dividers, dialogs
    terminal view: visible cells, selection, local history
      per-pane Alacritty terminal model
      session/topology state and restoration coordinator
        bounded tmux control adapter (tmuxctl)
          SSH channel / transport adapter
                    |
                 encrypted SSH
                    |
Linux host: stock sshd -> stock tmux -C -> existing tmux server
                                             |
                                       PTYs and applications
```

Start with one host connection and one attached session. Later multiplex one SSH
connection per host, with an exec channel per attached session; not one network
connection per pane. Open an SSH exec channel **without requesting a PTY** and
execute an explicitly quoted command such as:

```sh
exec tmux -C attach-session -t '=work'
```

Use `-C`, not terminal-oriented `-CC`. Session names are data and require both
correct shell quoting and exact tmux targeting. Keep SSH stderr separate from
the control stdout stream. Attach to an existing session; creation is a separate
explicit user action. Keep `-L`/`-S` socket selection available for non-default
servers, without changing their permissions.

### Module boundaries

Create these as the corresponding feature lands:

- `src/core.rs`: validated identities, dimensions, and small shared types.
- `src/control.rs`: bounded byte framing, tmuxctl adapter, reply lifecycle.
- `src/command.rs`: typed command construction and safe argument encoding.
- `src/connection.rs`: connection epochs, recovery states, retry policy.
- `src/terminal.rs`: pane-local Alacritty model; no window, SSH, or local PTY.
- `src/app_state.rs`: hosts, sessions, pane topology, restoration transactions.
- `src/ssh.rs`: embedded transport, authentication, host keys, config subset.
- `src/input.rs`: conventional GUI keys and terminal input/mouse encoding.
- `src/ui/`: terminal views, sidebar, tabs, divider interactions, dialogs.
- `src/main.rs`: CLI/bootstrap and eventually winit/Blade lifecycle.
- `tests/data/`: small, synthetic, deterministic protocol fixtures.

The first implementation exposes a headless replay command in `src/replay.rs`.
That is a correctness harness, not a replacement product direction or a working
SSH GUI. Only implemented modules exist; later modules are not empty scaffolding.

## 5. Dependencies and FileMan conventions

Reference: FileMan commit `26499ebdb3d983190c61b9016a0ea31b2711aacf`.
Its current GUI set is egui/egui-winit 0.34, winit 0.30.5, and Blade git revision
`f1fbf2a`. Introduce this set together when the GUI milestone starts and verify
that the resolved versions still agree. Reuse its thin-LTO/stripped release
profile, `AGENTS.md` pointers, contributor style, and tests/fixtures organization.

The foundation uses `alacritty_terminal` 0.26, `tmuxctl` 0.1 with its runtime/spawn
defaults disabled, and `anyhow`. Cargo.lock is generated and committed. The
package declares Rust 1.96+, as required by tmuxctl 0.1. Audit the actual resolved
source, not just README claims. Do not introduce Alacritty's window/renderer or
a local PTY dependency of our own.

SSH candidate: `russh`, using an explicitly selected crypto backend and only
needed features. Before committing to it, compare dependency/build cost and
agent/config support with FileMan's existing `ssh2` approach. Embedded SSH is the
architectural decision; a particular SSH library is not yet irrevocable. Do not
ship both libraries by default. Configuration parsing is acceptable; incomplete
semantics must be explicit.

Add only dependencies used by implemented code. Measure dependency count,
clean-build time, binary size, startup, idle CPU, and memory separately; one is
not a proxy for all the others.

## 6. Protocol and terminal correctness

### Control framing and commands

Keep the wire as bytes. Decode tmux octal escapes once, then feed pane output to
the appropriate terminal parser. Handle partial lines, arbitrary network chunk
boundaries, blank lines inside replies, unsolicited notifications, and EOF.
Bound line length, reply accumulation, pending commands, and outgoing queues.
A framing failure must tear down that stream, not continue with guessed state.

`tmuxctl` has a FIFO command-correlation engine. Register commands in write order;
initial attachment replies and server-internal blocks must not consume the wrong
request. Initially send one simple command per request; command lists, aliases,
hooks, and delayed commands need explicit compatibility tests before adoption.
Every outstanding command receives a failure/uncertain outcome on disconnect.

Use typed pane/window/session IDs for actions. Hex-encode arbitrary key bytes
for `send-keys -H`; do not concatenate terminal input into command syntax.
Separate terminal key input from bracketed paste and application mouse events.
Limit/batch large input rather than generating unbounded control lines.

### Terminal ownership

One emulator per pane, initialized from the server's actual dimensions. The
Alacritty parser/model is reused independently of its renderer and PTY loop.
Control-mode output is a terminal byte stream, not a ready-made grid.

Do not automatically send emulator-generated device-query responses back to the
application: tmux already terminates the application's terminal protocol. Audit
which terminal events are meaningful locally. Clipboard reads/writes, URL
activation, notifications, and other side effects are disabled or explicitly
gated. Replaying old output must not replay these side effects.

### Reattachment is a reconstruction transaction

A captured screen is not a complete serialized terminal. Implement and test a
bootstrap that establishes topology, history/current screen, dimensions, cursor,
mode state, and any pending escape-sequence fragment while live output arrives.
Do not append a capture to an old emulator and call that recovery.

Start attachment in a deliberately controlled output state where supported,
query the server, build replacement pane models, establish the ordering between
snapshot and live bytes, then atomically publish a coherent view. Document the
ordering guarantees demonstrated by tmux transcripts; do not assume a general
atomic snapshot API exists. If exact reconstruction cannot be established for a
mode/version, expose a resynchronizing/limited state instead of inventing
history or enabling unsafe input.

Keep the last disconnected view readable and copyable. Gaps due to tmux's
retention limits must be visible. Resynchronize on pause/continue or output loss;
never silently drop bytes and pretend the emulator remains accurate.

### Geometry and history

The server owns pane geometry, and another client may be attached. Agree an
explicit client/window sizing policy before resizing. Do not mutate global tmux
options or unexpectedly resize other users' sessions. Coalesce divider drags;
apply the server's resulting layout as authoritative. Query history limits;
local scrollback and retained remote history have different lifetimes.

## 7. Connection and security invariants

Model at least:

```text
Disconnected -> Connecting -> Restoring -> Live
                     |             |       |
                     +---------- Backoff <-+
                     |
                 NeedsAttention (authentication, changed host key, missing session)
```

Each attempt has a monotonically changing epoch. Late data or UI actions from a
previous channel must not mutate a newly attached session. Input is accepted
only for a live, synchronized epoch. Do not queue input while offline and do not
replay writes whose delivery is uncertain. Acknowledgment of `send-keys` is not
proof that an application completed the requested operation.

Network failures use bounded exponential backoff with jitter, cancellation, and
a visible Retry action. Authentication failures, changed host keys, and a missing
session require user attention rather than endless retries or automatic session
creation. Distinguish transient transport loss, deliberate detach, tmux restart,
and host reboot. Reset retry counters only after a sufficiently healthy period.

SSH host-key checks are mandatory. Persist trust only after explicit confirmation;
a changed key blocks reconnection. Reuse agents/keys where possible; never store
plaintext passwords. Keep credential material out of logs and replay fixtures.

For the first embedded configuration subset support Host/HostName, User, Port,
IdentityFile, and known-hosts paths with documented first-value/wildcard behavior.
Add Include, ProxyJump, agent integration, and platform-specific agents in tested
increments. Unsupported Match/ProxyCommand or security-affecting directives must
be reported; do not silently connect with a different meaning. Optional `ssh -G`
resolution/system-SSH fallback can cover advanced configurations, but is not a
requirement for the embedded path.

## 8. GUI behavior

Small host/session sidebar; tabs for windows; pane layout mirrors tmux. Click to
focus, drag dividers, use visible split/close actions, and use conventional
copy/paste shortcuts appropriate to each OS. No mandatory tmux prefix key.

Wheel scrolling should normally inspect local history. Application mouse
reporting needs a discoverable override for local selection/scrolling. Preserve
Unicode width, combining characters, wide-cell selection, IME, wrapped-line
copy, bracketed paste, application cursor keys, and high-DPI coordinates.

Start by rendering visible terminal rows with cached egui drawing. Do not use a
widget per cell or a TextEdit containing the entire history. Add a dedicated
Blade glyph-atlas pass only if profiling justifies it. Repaint on data/input/
resize, not an unconditional high-rate timer. Keep network/parsing work off the
UI thread and coalesce repaint requests, not terminal bytes.

## 9. Ordered milestones and acceptance gates

### M0 — Reusable foundation (implemented)

- [x] Establish one-package Rust layout, docs, style, and cross-platform CI.
- [x] Add validated IDs/dimensions and safe tmux command construction.
- [x] Wrap tmuxctl without coupling it to an SSH runtime or GUI.
- [x] Bound incoming framing and pending-command state; handle EOF explicitly.
- [x] Add connection epochs and a tested no-offline-input state machine.
- [x] Feed synthetic pane output into Alacritty; provide headless replay.
- [x] Test fragmented input, multiple panes, control characters, and teardown.

Gate: deterministic tests pass; README clearly says this is not yet a live GUI.
The foundation has 25 unit/integration tests. The state machine is policy only:
there is no SSH channel, reconnect timer, or snapshot-restoration transaction.
The replay path uses a fixed pane size and rejects layout changes. Unit tests
are not evidence of compatibility with a real tmux server; that is M1's gate.

### M1 — Existing-session attachment over embedded SSH

- [ ] Choose and pin the SSH library/features after a small dependency audit.
- [ ] Host-key verification and a minimal explicit authentication/config path.
- [ ] Open a non-PTY exec channel; attach only to an existing tmux session.
- [ ] Discover topology and actual dimensions before interpreting pane state.
- [ ] Establish initial history/screen/mode synchronization ordering.
- [ ] Add isolated Linux tmux + localhost-SSH integration tests and timeouts.

Gate: inspect a pre-existing shell and a full-screen TUI from the client; an
ordinary tmux client can still attach; no custom remote executable is installed.

### M2 — Minimal FileMan-style desktop client

- [ ] Bring in the matching Blade/egui/winit set, with event-driven redraw.
- [ ] Display independent pane grids, basic host/session selection, and errors.
- [ ] Focus/input, local scrollback, selection, copy, and bracketed paste.
- [ ] Draggable dividers with a tested shared-client sizing policy.
- [ ] Manual smoke tests on Linux, macOS, and Windows; IME/high-DPI checks.

Gate: normal shell work and the user's Codex workflow are comfortable without
learning tmux keybindings. Distinguish application history from terminal history.

### M3 — Reconnection that is actually trustworthy

- [ ] Network detection, cancellable retry/backoff/jitter, and explicit UI states.
- [ ] Rebuild terminal models and topology on each attachment epoch.
- [ ] Drop stale callbacks/actions; fail pending writes without replay.
- [ ] Exercise shell/TUI output and layout changes during a network interruption.
- [ ] Verify missing sessions, server restarts, exhausted history, and slow readers.

Gate: disconnect during output/input, continue work remotely, reconnect, and
compare layout/screen/cursor/modes with the server. No duplicate command, hidden
history gap, endless auth retries, or phantom successful reconnection.

### M4 — Multiple machines and operational fit

- [ ] Several hosts/sessions with visible status and one host connection where practical.
- [ ] SSH Include/ProxyJump/agent workflows and optional system-SSH adapter.
- [ ] Persist non-secret connection/session preferences using a small documented format.
- [ ] Replay/failure fixtures and protocol modules remain usable without GUI dependencies.

Gate: representative personal machines and cluster/bastion access work without
remote Starcom deployment or changing global tmux configuration.

### M5 — Performance and release hardening

- [ ] Record startup, idle CPU, RAM per pane/history size, throughput, and input latency.
- [ ] Stress many panes, long lines, sustained output, reconnects, and window resizing.
- [ ] Fuzz/hostile transcripts; audit clipboard/URL/title/escape-sequence behavior.
- [ ] Pin/test supported tmux versions, including at least one older Linux-distro version.
- [ ] Package Windows/macOS/Linux builds following FileMan's release organization.
- [ ] Optimize rendering/allocations only against reproducible measurements.

Gate: publish measured budgets and a compatibility matrix, not unverified
"lighter/faster than X" claims.

## 10. Validation matrix

Unit/replay tests must run without tmux, SSH, a GPU, or a display. Include arbitrary
chunk splits, non-UTF-8 output, blank reply lines, unknown notifications, bounded
long lines/replies, startup versus command replies, disconnect with pending
commands, stale epochs, and command-injection attempts in session names/input.
Terminal tests cover carriage-return redraw, ANSI attributes, alternate-screen
entry/exit, cursor addressing, wrapping, and independent panes.

Linux integration tests create their own tmux socket and temporary SSH fixture.
Never use `kill-server` against the default socket. Put deadlines on every wait.
Later GUI replay tests use software rendering/snapshots as FileMan does. CI
compilation is not a replacement for native input/clipboard/IME smoke tests.

Before calling the client usable, test a shell, an editor, a monitoring TUI, and
Codex. Include two attached clients of different sizes, connection loss during a
paste, remote resize while detached, changed host key, missing session, and
server restart. Record unsupported terminal features explicitly.

## 11. Open decisions, not excuses to block M0

1. Embedded SSH backend: russh versus reusing ssh2; include Windows agent support
   and build/dependency costs in the comparison.
2. Exact snapshot/live-output ordering and mode reconstruction for supported tmux
   versions. This is the primary correctness investigation in M1/M3.
3. Native text rasterization/fallback/IME and eventual accessibility needs;
   establish a basic view before adding a custom glyph system.
4. Per-client geometry policy when other tmux clients are connected.
5. Concrete performance budgets, after a reproducible baseline exists.

## References

[1]: https://github.com/tmux/tmux/wiki/Control-Mode
[2]: https://github.com/tmux/tmux/blob/master/client.c
[3]: https://github.com/tmux/tmux/blob/master/control.c
[4]: https://docs.rs/tmuxctl/latest/tmuxctl/
[5]: https://docs.rs/alacritty_terminal/latest/alacritty_terminal/
[6]: https://docs.rs/russh/latest/russh/
[7]: https://github.com/kvark/fileman/tree/26499ebdb3d983190c61b9016a0ea31b2711aacf
[8]: https://man.openbsd.org/ssh_config

Primary technical references: [tmux control mode][1], [native client][2],
[server control implementation][3], [tmuxctl][4], [Alacritty terminal core][5],
[russh][6], [FileMan reference][7], and [OpenSSH configuration][8]. Library versions
and source links should be rechecked when a milestone introduces that dependency.

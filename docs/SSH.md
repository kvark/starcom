# Embedded SSH: first live milestone

Status: 2026-08-29. **M1 is partially implemented, not complete.** This is the
implementation-status companion to [PLAN.md](../PLAN.md).

## Implemented

- Embedded `ssh2` transport, following FileMan rather than introducing a second
  SSH ecosystem. `polling` supplies readiness waits; `base64` formats SHA256
  fingerprints. No asynchronous runtime or local SSH subprocess is required.
- Explicit host/user/port/known-hosts/authentication options. Plain and hashed
  known-host entries are checked before attempting identity or agent auth.
- A non-PTY exec channel running stock `tmux -N -C`, attached to an existing
  session. `-N` prevents server startup and `-E` prevents session environment
  updates. `read-only,ignore-size,no-output` flags are verified after attachment.
- Bounded pane discovery and textual screen/history inspection. Geometry, cursor,
  alternate-screen status, and history counts are reported. Captures are escaped
  when printed. SSH stderr never enters the control protocol parser.
- Unit/CLI tests and seven Linux integration tests using disposable SSH/tmux
  fixtures. Tests exercise trust failures before auth, hashed host keys, agent
  auth, separate stderr, deadlines, missing sessions, and preservation of remote
  process IDs, dimensions, and environment.

The Windows dependency enables vendored OpenSSL for Ed25519 support; this has a
native build cost. Linux/macOS normally use the system OpenSSL development
installation. `vendored-openssl` is available explicitly on those platforms too.
The `ssh` feature is enabled by default; disable defaults to test just the core.
This selection prioritizes reuse and proven integration, not an unmeasured claim
that libssh2 is smaller or faster than russh.

## Deliberate boundaries

The inspector never enables interactive input and never marks the connection
state `Live`. A capture is not a serialized terminal. Queries happen at different
times; an unchanged metadata comparison does not prove an atomic snapshot.
Captures must not be appended to an old Alacritty model as a reconnect strategy.
The tmux read-only flag is not an authorization sandbox for control commands;
the inspector itself only issues its small, private set of read-only queries.
User-configured tmux hooks can still run when a client attaches.

SSH configuration parsing, jump hosts, proxy commands, MFA/password prompts,
trust-on-first-use dialogs, and reconnect scheduling are not implemented. An
explicit identity file must be unencrypted; encrypted keys can be loaded into an
agent. Agent authentication was live-tested on Linux, not against every platform's
agent implementation. No password or private-key contents are logged or stored.

Known-host markers such as `@revoked` and `@cert-authority`, wildcards, and negation
patterns are rejected rather than ignored. This conservative check applies to
the whole supplied file, including unrelated entries. Do not remove revocations
to bypass this limitation. Supporting these semantics correctly, or using a
system-SSH fallback, is subsequent work. Unknown/changed keys are not accepted
and trust files are never rewritten automatically.

Network operations have deadlines; OS DNS resolution and local agent IPC are
not covered by those network timeouts. Connection/authentication are blocking
and must run on a worker when a GUI is added. One connection currently owns one
channel; multi-session connection sharing remains later work.

Resource budgets are explicit: 128 panes, 1000 requested history lines, bounded
protocol lines/replies, 64 KiB stderr, and 8 MiB capture/transfer budgets. Large
sessions can fail with an explicit limit error. A captured text line resembling
a tmux reply guard can currently cause the strict control parser to fail closed;
framing-safe snapshot extraction needs attention before general terminal use.

## Remaining M1 acceptance work

1. Establish and test snapshot/live-output ordering, including cursor, modes,
   alternate/primary screens, wrapping, and pending escape-sequence fragments.
2. Feed a coherent restored state into pane emulators before enabling input.
   Test output and topology changes while the initial snapshot is being built.
3. Wire connection epochs and teardown to the live transport; keep uncertain
   writes failed, never automatically replayed.
4. Add the documented SSH configuration subset and capability/version matrix.
   The current inspector validates required client flags but does not establish
   compatibility with every tmux release.

Then proceed to M2's Blade/egui desktop interface. Live TUI/Codex acceptance and
reconnection tests remain outstanding; the integration fixture only demonstrates
transport, inspection, and preservation of controlled primary/alternate screens.

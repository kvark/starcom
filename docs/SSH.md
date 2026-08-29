# Embedded SSH: first live milestone

Status: 2026-08-29. Embedded SSH inspection and read-only snapshot-to-live models
are implemented. General interactive terminal fidelity and desktop acceptance
remain open. See [PLAN.md](../PLAN.md) and [synchronization details](SYNCHRONIZATION.md).

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
- `--watch SECONDS` reconstructs fresh pane models, then feeds live output into
  them. Geometry changes invalidate the view and trigger a new snapshot. Screens
  remain readable on disconnect; input and automatic reconnect are not enabled.
- Unit/CLI tests and isolated Linux integration tests cover transport/trust,
  snapshot ordering, pending sequences, continuous output, resize, hook safety,
  and preservation of remote jobs. See synchronization details for tests actually
  executed and remaining platform coverage.

The Windows dependency enables vendored OpenSSL for Ed25519 support; this has a
native build cost. Linux/macOS normally use the system OpenSSL development
installation. `vendored-openssl` is available explicitly on those platforms too.
The `ssh` feature is enabled by default; disable defaults to test just the core.
This selection prioritizes reuse and proven integration, not an unmeasured claim
that libssh2 is smaller or faster than russh.

## Deliberate boundaries

The inspector never enables interactive input and never marks the connection
state `Live`. Default inspection queries happen at different times; an unchanged
metadata comparison does not prove an atomic snapshot. `--watch` uses a separate
synchronous command batch and a fresh set of models, not captures appended to old
models. A capture is still not a complete serialized terminal parser; fidelity
limits and source-supported ordering assumptions are documented separately.
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

Resource budgets are explicit: 128 panes for basic inspection (32 for live
reconstruction), 1000 requested history lines, bounded
protocol lines/replies, 64 KiB stderr, and 8 MiB capture/transfer budgets. Large
sessions can fail with an explicit limit error. A captured text line resembling
a tmux reply guard can currently cause the strict control parser to fail closed;
framing-safe snapshot extraction needs attention before general terminal use.

## Remaining acceptance work

The snapshot-to-live path has passed controlled Linux tmux 3.4 tests, including
primary/alternate buffers and output arriving during attachment. The next desktop
step can use these models, while keeping their fidelity limits explicit.

Still outstanding: full TUI/Codex acceptance, broader tmux and client-platform
coverage, missing parser-state fidelity, SSH configuration semantics, interactive
input gating, and transport reconnect/epoch integration. M2 adds Blade/egui views,
selection, scrolling, and resizing; M3 integrates automatic connection recovery.

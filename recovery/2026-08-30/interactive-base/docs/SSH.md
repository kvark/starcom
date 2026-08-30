# Embedded SSH

Starcom uses **Sunset 0.6** for the SSH protocol and **RustCrypto / `ssh-key`**
for identity signing. `polling` supplies socket readiness without an asynchronous
runtime. There is no local SSH subprocess, libssh2, OpenSSL, ring, or AWS-LC.
The remote side remains ordinary Linux sshd and stock tmux.

This replaces the first milestone's C-backed `ssh2` implementation. FileMan's
GUI conventions are retained; its older SSH dependency is not copied. Current
russh backends require native crypto, so disabling its default features alone
would not meet Starcom's requirement. Sunset's lower-level interface lets the
application own bounded buffers, deadlines, and agent requests.

## Connect

```sh
cargo run --locked --bin starcom-inspect -- \
  --host server.example.com --user your-user --session work \
  --known-hosts "$HOME/.ssh/known_hosts" --agent
```

Use `--identity FILE` for an **unencrypted OpenSSH-format** private key instead
of `--agent`. Ed25519, RSA with SHA-256 signatures, and ECDSA P-256 identities are
supported. Unlock encrypted keys in your SSH agent; Starcom does not ask for or
store passphrases. On Unix, the agent is selected by `SSH_AUTH_SOCK`. On Windows,
use the local OpenSSH-agent named pipe (or a local pipe in `SSH_AUTH_SOCK`);
Pageant and Unix-socket compatibility bridges are not implemented.

Connection options are explicit. SSH config aliases, Include/Match, bastions,
ProxyJump/ProxyCommand, certificates, hardware security-key identities, and MFA
prompts remain future work. No password fallback or agent forwarding occurs.

Add `--watch 5` to reconstruct pane models, consume five seconds of live output,
and print escaped final screens. The GUI uses the same transport and snapshot
logic. It remains read-only: keystrokes and clipboard paste are not sent remotely.

## Trust and authentication

Known-host verification occurs **before loading a private key or contacting the
agent**, and is repeated on rekey. Exact hostnames, comma-separated exact entries,
and hashed `|1|...` entries work. Non-default ports use the exact `[host]:port`
identity, not a fallback to the port-22 entry. SHA-256 fingerprints are displayed.
Unknown or changed keys fail; trust files are never changed automatically.

Markers such as `@revoked` and `@cert-authority`, wildcard patterns, and negation
are rejected rather than silently ignored. This conservative restriction applies
to the entire supplied file. Do not delete revocations to bypass it; full policy
support or an explicit system-SSH adapter requires separate implementation.

Private-key file bytes use zeroizing storage. Signing credentials and the local
agent connection are released after authentication. Agent requests and replies
are size-bounded, with explicit signature-algorithm checks. Credentials and pane
contents are not logged by default. The dependency policy is not a claim that
this application or its cryptographic dependencies have received a security audit.

## Transport and tmux behavior

One worker owns one nonblocking SSH connection and one exec channel. It does not
hold the GUI model lock while doing network I/O. Partial socket writes consume
only accepted bytes; reconnect never retries uncertain application input. Receive
queues are bounded (1 MiB stdout, 64 KiB stderr); backpressure stops reads instead
of discarding terminal output. Stderr is always separate from the control stream.

Network and agent I/O use deadlines. OS hostname resolution, local filesystem
access, and agent connection establishment can still block outside those timers;
they run on the worker, never the window thread. Worker cancellation invalidates
publication immediately, but does not forcibly interrupt those OS calls.

The non-PTY channel executes `tmux -N -C ... attach-session -E ...`.
`-N` forbids starting a replacement server, `-E` leaves session environment alone,
and the attachment starts with `read-only,ignore-size,no-output`. The client
verifies those flags before proceeding. Sunset's exec request does not request
an acknowledgment; only valid tmux replies establish readiness. Denied execs
produce channel closure or an operation timeout, not a false successful session.

The tmux read-only flag is not an authorization sandbox. Starcom issues its own
restricted query set, does not change global options, and avoids remote input and
geometry changes. Existing attach hooks may still run. Captures with yielding
hooks are refused before capture because they invalidate snapshot ordering.

Default inspection consists of separate observations. The `--watch` path uses
an ordered snapshot-to-live transaction and fresh models; see
[SYNCHRONIZATION.md](SYNCHRONIZATION.md) for remaining parser-state limitations.
Output discarded by tmux cannot be reconstructed. Budget violations fail visibly.

## Build policy and validation

`python3 scripts/check-dependencies.py` checks the resolved host dependency graph
for forbidden native SSH/crypto packages. CI also builds with unusable `CC` and
`CXX` values to catch accidental C/C++ compilation. Normal platform linking,
graphics drivers, and operating-system FFI are still required. Linux window-system
crates can include the Rust `cc` or `pkg-config` helper crates without invoking a
C compiler under our selected features; absence of their names is not the policy.

The isolated Linux fixture requires test-server executables `sshd`, `ssh-keygen`,
`ssh-agent`, `ssh-add`, and tmux. These are **test/runtime programs**, not native
libraries compiled into Starcom. It generates and deletes its own keys, uses a
loopback-only listener and a unique tmux socket, and never touches default sessions.

Tests cover known-host failures before authentication, hashed entries, agent
signing, Ed25519/RSA/P-256 identity authentication, 128 KiB streams across forced
16 KiB rekeys, separated stderr, deadlines, snapshot continuity, capture-hook
safety, and preservation of processes, dimensions, and session environment.
A separate native Windows-agent test uses one disposable identity and removes it.

Sunset currently has a small fixed receive window and a limited algorithm set.
High-latency throughput, broader servers, rekey stress, and native platform agents
need continued measurement and compatibility work. Pure Rust is not by itself
proof of better latency, lower RAM, or mature SSH interoperability. The backend
remains an experimental client integration, not a replacement for OpenSSH's full
configuration/authentication surface.

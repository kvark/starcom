# Changelog

## v0.1.0

First release. Starcom attaches to existing remote tmux sessions over its own
embedded SSH client and draws them in a native window.

- **Attach, don't replace.** Uses tmux control mode (`tmux -N -C`) against a
  stock remote tmux. Starcom never starts a tmux server implicitly, never
  modifies global tmux settings, and leaves the ordinary `tmux` client usable
  as a fallback. Closing a tab or the window detaches; remote jobs keep running.
- **Embedded SSH.** Sunset plus RustCrypto — no OpenSSL, libssh2, ring, AWS-LC,
  or `ssh` subprocess. Strict known-hosts verification with exact,
  port-qualified, and hashed entries; unknown or changed keys always fail.
  Ed25519, RSA/SHA-256, and ECDSA P-256 keys, and Unix and Windows OpenSSH
  agents.
- **`~/.ssh/config` reading, without executing anything.** `Host` patterns,
  `HostName`, `User`, `Port`, `IdentityFile`, `IdentitiesOnly`,
  `UserKnownHostsFile`, `ProxyJump`, and bounded `Include`. Directives that
  cannot be honoured exactly are shown as blockers rather than bypassed.
- **ProxyJump.** Bastion chains of up to four hops. Every hop verifies its own
  host key and authenticates on its own; a jump host never vouches for the one
  behind it.
- **Automatic reconnection, for transport loss only.** Authentication,
  host-key, missing-session, server-exit, and detach failures stop and wait.
  Nothing typed while offline is queued, and no uncertain input is replayed.
- **Session discovery and creation.** Listing uses `tmux -N` and cannot bring a
  server into existence. Creating a session is only reachable from an explicit
  confirmation.
- **Saved tabs.** Destinations and preferences persist; credentials, host-key
  material, and terminal contents never do. Restoring a workspace opens forms,
  not connections.
- **Desktop client.** Blade/egui rendering of Alacritty's terminal model, panes
  and tabs, mouse selection, scrollback, paste confirmation, and draggable pane
  dividers. Remote resizing is opt-in.

Prebuilt Linux, macOS, and Windows artifacts are attached below. Linux and
Windows builds are unsigned; macOS builds are ad-hoc codesigned, not notarized.

Known limitations: MFA, host and user certificates, `ProxyCommand`, and reusing
one SSH connection across tabs are not supported. Remote hosts are Linux with
stock OpenSSH and tmux. No performance baselines are published yet.

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
  Ed25519, RSA/SHA-256, ECDSA P-256, and agent-held `sk-ssh-ed25519` keys,
  and Unix and Windows OpenSSH agents.
- **`~/.ssh/config` reading, without executing anything.** `Host` patterns,
  `HostName`, `User`, `Port`, `HostKeyAlias`, every `IdentityFile` in order,
  `IdentitiesOnly`, `UserKnownHostsFile`, and bounded `Include`. Directives that cannot be
  honoured exactly are shown as blockers rather than bypassed.
- **Identities, then the agent.** Files from the profile (or OpenSSH's default
  `id_ed25519` / `id_ecdsa` / `id_rsa` when none are set) are offered in order,
  then the local agent unless `IdentitiesOnly` is set. There is no
  agent-versus-key radio. Encrypted files and unsupported agent algorithms are
  named in the error (`the SSH agent holds 3 keys, 1 unsupported (sk-ecdsa-…)`).
- **Automatic reconnection, for transport loss only.** Authentication,
  host-key, missing-session, server-exit, and detach failures stop and wait.
  Nothing typed while offline is queued, and no uncertain input is replayed.
  A sleeping laptop or a black-holed control stream is treated as transport
  loss, not as a protocol fault.
- **Session discovery and creation.** Listing uses `tmux -N` and cannot bring a
  server into existence. Creating a session is only reachable from an explicit
  confirmation.
- **Saved tabs.** Destinations and preferences persist; credentials, host-key
  material, and terminal contents never do. Restoring a workspace opens forms,
  not connections, and re-reads identity policy and blockers from the config
  so a saved tab cannot skip a new `IdentityFile`. An unreadable file is a
  system dialog before the window opens: clear it, or exit and leave it.
- **Desktop client.** Blade/egui rendering of Alacritty's terminal model, panes
  and tabs, mouse selection, scrollback, paste confirmation, and draggable pane
  dividers. Maximize is tmux zoom and keeps focus. Exit returns to the form and
  retitles the tab from those fields. Remote resizing is opt-in.

Prebuilt Linux, macOS, and Windows artifacts are attached below. Linux and
Windows builds are unsigned; macOS builds are ad-hoc codesigned, not notarized.

Known limitations: MFA, host and user certificates, `ProxyJump`, `ProxyCommand`,
`sk-ecdsa` and file-based SK keys, and reusing one SSH connection across tabs
are not supported. Remote hosts are Linux with stock OpenSSH and tmux. No
performance baselines are published yet.

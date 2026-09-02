# Changelog

## v0.1.2

Terminal viewport and tab chrome after `v0.1.1`.

- **History stays where you left it.** Scrolling up no longer follows new
  output. The live tip is the last screen of the buffer, not past it, so a
  new attach is not a blank pane. A dropped connection keeps that last view
  on screen (dimmed) while it retries.
- **Tabs.** Hover lightens a chip without changing the strip height. Color
  still tracks connected, connecting, and failed.
- **Status.** The focused pane's size sits next to Exit. A copy notice
  replaces the hint for a moment. Idle polls do not wake the GUI unless the
  screen changed.

Prebuilt Linux, macOS, and Windows artifacts are attached below. Linux and
Windows builds are unsigned; macOS builds are ad-hoc codesigned, not notarized.

Known limitations: MFA, host and user certificates, `ProxyJump`, `ProxyCommand`,
`sk-ecdsa` and file-based SK keys, and reusing one SSH connection across tabs
are not supported. Remote hosts are Linux with stock OpenSSH and tmux. No
performance baselines are published yet.

## v0.1.1

Identities, hardware keys, install, and the terminal workspace after `v0.1.0`.

- **Identities, then the agent.** Files from the profile (or OpenSSH's default
  `id_ed25519` / `id_ecdsa` / `id_rsa` when none are set) are offered in order,
  then the local agent unless `IdentitiesOnly` is set. There is no
  agent-versus-key radio. A missing `User` is the local account. `HostKeyAlias`
  is the known-hosts lookup name; TCP still uses `HostName`. Encrypted files and
  unsupported agent algorithms are named in the error (`the SSH agent holds 3
  keys, 1 unsupported (sk-ecdsa-…)`).
- **Agent-held `sk-ssh-ed25519`.** Hardware keys in the local agent are offered;
  signing stays in the authenticator. `sk-ecdsa` and file-based SK keys are not.
- **Saved workspace.** Restored tabs re-read identity policy and blockers from
  `~/.ssh/config`. An unreadable `workspace.conf` is a system dialog before the
  window opens: clear the file, or exit and leave it. Tmux session names are
  not persisted; the host is listed when you pick it.
- **Desktop.** Maximize is tmux zoom and keeps focus. Exit, or typing `exit` in
  the last shell, returns to the form and names the tab **New connection**.
  Tabs are smaller and color-coded: green connected, yellow connecting or
  reconnecting, red failed. Drag a tab to reorder without dropping the pane's
  keyboard focus; switching tabs restores it. Connect shows **Connecting…**
  with a spinner. A reconnecting session paints the last view slightly dimmed.
  Create a session in a field beside the session list, not a dialog. Right-click
  copies the selection or the whole pane. Divider drags always resize tmux.
  After keys or wheel, remote echo runs at 20 fps for a short time. The idle
  paint rate is `fps` in `workspace.conf` (`etc/workspace.conf.example`);
  polls that do not change the screen do not wake the GUI. Hovering a tab
  lightens it without changing the strip height. Scrolling up in history stays
  on those lines while new output arrives; the live tip still follows. The
  status line shows the focused pane's size, and a copy notice replaces the
  hint for a moment. The selected pane is the widget with keyboard focus;
  arrows, Tab, and Escape stay with that field or pane instead of walking to
  other buttons.
- **Install.** `make install` does not assume GNU `install -D`. On macOS it
  writes `~/Applications/Starcom.app`. macOS/Windows icons use a full PNG
  family for `cargo-bundle --bin`.

Prebuilt Linux, macOS, and Windows artifacts are attached below. Linux and
Windows builds are unsigned; macOS builds are ad-hoc codesigned, not notarized.

Known limitations: MFA, host and user certificates, `ProxyJump`, `ProxyCommand`,
`sk-ecdsa` and file-based SK keys, and reusing one SSH connection across tabs
are not supported. Remote hosts are Linux with stock OpenSSH and tmux. No
performance baselines are published yet.

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
  agents. Hardware-backed `sk-*` keys are not offered.
- **`~/.ssh/config` reading, without executing anything.** `Host` patterns,
  `HostName`, `User`, `Port`, `IdentityFile`, `IdentitiesOnly`,
  `UserKnownHostsFile`, and bounded `Include`. Directives that cannot be
  honoured exactly are shown as blockers rather than bypassed. Multiple
  `IdentityFile` entries are a blocker in this release.
- **Authentication.** The form chooses an SSH agent or a key file. Encrypted
  files need `ssh-add`.
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
  not connections.
- **Desktop client.** Blade/egui rendering of Alacritty's terminal model, panes
  and tabs, mouse selection, scrollback, paste confirmation, and draggable pane
  dividers. Remote resizing is opt-in.

Prebuilt Linux, macOS, and Windows artifacts were attached to the GitHub
Release. Linux and Windows builds are unsigned; macOS builds are ad-hoc
codesigned, not notarized.

# Sunset patches: client `direct-tcpip` and agent-held SK-Ed25519

`0001-Add-a-client-direct-tcpip-channel-open.patch` applies to Sunset at tag
`sunset-0.6.0` (upstream `https://github.com/mkj/sunset`). It is 62 lines across
four files and adds nothing to the wire format Sunset does not already encode.
`0002-Offer-agent-held-sk-ssh-ed25519-keys.patch` applies on top of that: it
parses `sk-ssh-ed25519@openssh.com` public keys and agent signatures so a
FIDO key in the local agent can be offered. Signing still happens in the
agent; there is no libfido2.

Starcom builds against them. `Cargo.toml` carries a `[patch.crates-io]` entry
pinning `kvark/sunset` at the revision holding exactly these patches, the same
way the GUI stack pins `kvark/blade`. The files here are the changes as they
should reach upstream; the fork is how they are carried until they do.

## Why

`ProxyJump` needs a `direct-tcpip` channel: the bastion connects onward on the
client's behalf, and a second SSH session runs inside that channel. Sunset 0.6
already encodes and decodes the channel type, and its open-confirmation path is
type agnostic — but a client cannot open one. `open_client_session` is the only
opener, and inbound `direct-tcpip` is rejected with a literal `// TODO implement
it`. That is the whole of what blocks `ProxyJump` on the embedded transport.

## What it changes

- `Runner::open_client_tcpip(address, port, origin, origin_port)`, mirroring
  `open_client_session`. Nothing else is needed to carry the channel's data.
- `Runner::is_channel_finished(&ChanHandle)`, so a caller waiting on an open can
  tell a refusal from a slow open. A refused open leaves the channel in
  `PendingDone` rather than removing it, and `PendingDone` is reported neither as
  `eof` nor as `closed`. Without this a rejection is indistinguishable from a
  slow open until the caller's own deadline expires — which is what Starcom hit
  first: a refused forward hung for the full ten seconds instead of reporting.
- A `many-channels` feature raising `config::MAX_CHANNELS` from 4 to 16. The
  channel array is fixed size, so the default stays small for `no_std` targets.
  Starcom needs two channels per hop; the cap matters for sharing one connection
  across tabs, which is a separate question.
- `PubKey::SkEd25519` and `SignKey::AgentSkEd25519`, so an agent-held
  `sk-ssh-ed25519@openssh.com` key can be offered and the authenticator
  signature (flags + counter) can be sent. `sk-ecdsa-*` is still unknown.

## What was measured

Against OpenSSH 9.x on Linux, in the normal fixture run:

- A forward to a live listener carries data both ways.
- An unreachable destination is reported as a refusal, promptly. OpenSSH sends
  `SSH_MSG_CHANNEL_OPEN_FAILURE` rather than confirming and then closing, which
  is worth knowing: the failure arrives before any data path exists.
- A server with `AllowTcpForwarding no` — how a hardened bastion is configured —
  is also reported as a refusal rather than as a timeout.
- Sunset's own 31 tests pass with both patches applied, and it builds both with
  and without `many-channels`.

## Applying it

```sh
git clone https://github.com/mkj/sunset
git -C sunset checkout sunset-0.6.0
git -C sunset am /path/to/starcom/etc/sunset/*.patch
```

To test a change against a local checkout instead of the pinned fork:

```sh
cargo test --config 'patch.crates-io.sunset.path="/path/to/sunset"' --all-features
```

## Status

The patches are carried, not landed. `ssh::Connection::open_forward` exists and is
covered against a real server, but `ProxyJump` is still reported as unsupported:
running a second SSH session inside the channel needs `ssh::Connection` to accept
a transport other than a `TcpStream`, which is not written.

Remove the `[patch.crates-io]` entry, this directory, and the `allow-git` entry
in `deny.toml` once the changes reach a published Sunset release.

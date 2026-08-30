# Recovery snapshot: interactive desktop work

This directory preserves the exact unfinished sources from the interrupted 2026-08-30 implementation so they remain reachable in Git history and can be resumed in another session.

It is **not part of the Starcom build** and does not claim to compile as a standalone nested checkout. The root project remains at the last validated read-only desktop revision.

Contents:

- `interactive-base/`: the partially assembled repository tree containing guarded tmux input, paste, resize, and snapshot changes.
- `desktop.rs`, `ssh.rs`, `ui-input.rs`, and `ui-layout.rs`: newer source blobs that were uploaded but omitted from that tree.
- `workspace.rs`, `window.rs`, and `ssh_config.rs`: connection tabs, event-loop teardown changes, and bounded OpenSSH-config discovery.

Known missing integration:

- the matching interactive/config-aware `src/ui/mod.rs` was never uploaded;
- module wiring, CLI/demo snapshot wiring, docs, and tests must be reconciled;
- the complete tree must pass formatting, build, tests, Clippy, real SSH/tmux tests, exit smoke tests, and cross-platform CI before replacing root sources.

Once integration lands, this working-tree directory may be deleted. Its commit will remain in history as the durable handoff.

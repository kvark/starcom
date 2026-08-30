#!/usr/bin/env bash
# Build against a Sunset carrying etc/sunset/*.patch and run the forwarding
# coverage. Not part of the normal validation path: the published crate cannot
# open a client direct-tcpip channel, so this needs the patch to exist.
#
# Requires network access to clone Sunset. Everything it writes lives under
# target/, and the fixture it uses is the same disposable sshd as test-ssh.sh.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
checkout="$root/target/sunset"
tag=sunset-0.6.0

# Repointing Sunset changes dependency resolution, so Cargo rewrites Cargo.lock.
# That lockfile describes the published crate and every other check depends on
# it, so put it back however this exits.
lock_backup=$(mktemp "$root/target/Cargo.lock.backup.XXXXXX" 2>/dev/null \
    || mktemp "${TMPDIR:-/tmp}/Cargo.lock.backup.XXXXXX")
cp "$root/Cargo.lock" "$lock_backup"
restore_lock() {
    cp "$lock_backup" "$root/Cargo.lock"
    rm -f "$lock_backup"
}
trap restore_lock EXIT

if [[ ! -d "$checkout/.git" ]]; then
    git clone --quiet https://github.com/mkj/sunset "$checkout"
fi
git -C "$checkout" fetch --quiet --tags origin
git -C "$checkout" checkout --quiet "$tag"
git -C "$checkout" reset --hard --quiet "$tag"
# `am` needs an identity, and this checkout is disposable.
git -C "$checkout" -c user.email=starcom@invalid -c user.name=Starcom \
    am --quiet "$root"/etc/sunset/*.patch

echo "Sunset $tag + $(ls "$root"/etc/sunset/*.patch | wc -l) patch(es) at $checkout"
export RUSTFLAGS="${RUSTFLAGS:-} --cfg sunset_forward"
export STARCOM_FIXTURE_EXTRA_TESTS=1
"$root/scripts/test-ssh.sh" \
    --config "patch.crates-io.sunset.path=\"$checkout\""

#!/bin/sh
# Assemble Starcom.app from a release binary and etc/macos icons.
# Usage: macos-app.sh <starcom-binary> <Starcom.app>
set -eu

if [ $# -ne 2 ]; then
    echo "usage: $0 <starcom-binary> <Starcom.app>" >&2
    exit 2
fi

binary=$1
dest=$2
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | head -n1)
if [ -z "$version" ]; then
    echo "$0: could not read version from Cargo.toml" >&2
    exit 1
fi
if [ ! -f "$binary" ]; then
    echo "$0: missing binary: $binary" >&2
    exit 1
fi

stage=$(mktemp -d "${TMPDIR:-/tmp}/starcom-app.XXXXXX")
trap 'rm -rf "$stage"' EXIT
app=$stage/Starcom.app
macos=$app/Contents/MacOS
resources=$app/Contents/Resources
iconset=$stage/Starcom.iconset

mkdir -p "$macos" "$resources" "$iconset"
install -m 755 "$binary" "$macos/starcom"
sed "s/@VERSION@/$version/g" "$root/etc/macos/Info.plist" >"$app/Contents/Info.plist"
printf 'APPL????' >"$app/Contents/PkgInfo"

# iconutil's names are pixel size and density, not our on-disk filenames.
cp "$root/etc/macos/icon_16.png" "$iconset/icon_16x16.png"
cp "$root/etc/macos/icon_32.png" "$iconset/icon_16x16@2x.png"
cp "$root/etc/macos/icon_32.png" "$iconset/icon_32x32.png"
cp "$root/etc/macos/icon_64.png" "$iconset/icon_32x32@2x.png"
cp "$root/etc/macos/icon_128.png" "$iconset/icon_128x128.png"
cp "$root/etc/macos/icon_256.png" "$iconset/icon_128x128@2x.png"
cp "$root/etc/macos/icon_256.png" "$iconset/icon_256x256.png"
cp "$root/etc/macos/icon_512.png" "$iconset/icon_256x256@2x.png"
cp "$root/etc/macos/icon_512.png" "$iconset/icon_512x512.png"
cp "$root/etc/macos/icon_512@2x.png" "$iconset/icon_512x512@2x.png"
iconutil -c icns "$iconset" -o "$resources/AppIcon.icns"
rm -rf "$iconset"

codesign --sign - --force --deep "$app"

mkdir -p "$(dirname -- "$dest")"
rm -rf "$dest"
ditto "$app" "$dest"

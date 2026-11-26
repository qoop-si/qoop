#!/bin/sh
# Build qoop.app and make a notarized, stapled Developer ID dmg of it, on the owner's Mac:
#   ASC_ISSUER_ID=<issuer> crates/qoop-desktop/release-mac.sh [out dir]
# Writes qoop-<version>-mac-<arch>.dmg and its .sha256 into the out dir (default
# ~/.local/share/qoop/releases/<version>). Nothing is published.
#
# CI cannot do this: Xcode cloud-signs the Developer ID export with the Apple ID signed in to Xcode
# (no certificate file on disk), so the release workflow keeps its own signing path and this is the
# local one. devid-release.sh (~/.local/share/signing) does the export, notarytool, stapling and the
# spctl check; it needs ASC_ISSUER_ID in the environment.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
recipe=${DEVID_RELEASE:-$HOME/.local/share/signing/devid-release.sh}
[ -x "$recipe" ] || {
	echo "$0: $recipe is missing or not executable" >&2
	exit 1
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$root"
cargo build -p qoop-desktop --release --locked
version=$(sh "$here/bundle.sh" --binary "${CARGO_TARGET_DIR:-target}/release/qoop-desktop" "$work" | awk '{print $2}')
out=${1:-$HOME/.local/share/qoop/releases/$version}
"$recipe" "$work/qoop.app" "$out"

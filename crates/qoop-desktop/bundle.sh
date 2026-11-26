#!/bin/sh
# Make qoop.app from a built qoop-desktop binary, signed ad hoc:
#   crates/qoop-desktop/bundle.sh --binary <path to qoop-desktop> <out dir>
# writes <out dir>/qoop.app (Info.plist, MacOS/qoop-desktop, Resources/AppIcon.icns). It builds
# nothing: CI and the release hand it the binary they built. The release signs it again with the
# Developer ID; ad hoc is what a build without that certificate can carry. A Developer ID dmg made on the
# owner's Mac (Xcode cloud signing, which CI cannot do) comes from release-mac.sh.
set -eu

usage() {
	echo "usage: $0 --binary <qoop-desktop> <out dir>" >&2
	exit 2
}

[ "${1:-}" = "--binary" ] || usage
binary=${2:-}
out=${3:-}
[ -n "$binary" ] && [ -n "$out" ] || usage
[ -x "$binary" ] || {
	echo "$0: $binary is not an executable file" >&2
	exit 1
}

here=$(cd "$(dirname "$0")" && pwd)
version=$("$binary" --version | awk '{print $2}')
[ -n "$version" ] || {
	echo "$0: $binary did not say its version" >&2
	exit 1
}

app="$out/qoop.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
sed "s/@VERSION@/$version/g" "$here/bundle/Info.plist" >"$app/Contents/Info.plist"
cp "$binary" "$app/Contents/MacOS/qoop-desktop"
cp "$here/bundle/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"
codesign --force --sign - "$app"
echo "$app $version"

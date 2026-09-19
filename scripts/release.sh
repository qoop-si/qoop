#!/bin/sh
# Local release: build on this machine, publish the files to GitHub Releases.
#
# GitHub never compiles anything here. It only hosts the tarball, which costs
# no Actions minutes: the build runs on this Mac, and `gh release create`
# uploads the result. macOS arm64 only, which is what this machine is.
#
# Usage: ./scripts/release.sh 0.1.0
#
# What it does, in order: refuse a dirty tree, stamp the version into every
# workspace member, build the release binary, smoke it, pack it with its
# checksum, commit the bump, tag it, push, and create the GitHub release with
# notes from the commits since the previous tag.

set -eu

VERSION="${1:?usage: ./scripts/release.sh <version>, e.g. ./scripts/release.sh 0.1.0}"
VERSION="${VERSION#v}"

case "$VERSION" in
    *[!0-9.]* | "" | *..* | .* | *.)
        echo "release.sh: refusing version '$VERSION': want digits and dots, e.g. 0.1.0" >&2
        exit 1
        ;;
esac

if [ "$(uname -sm)" != "Darwin arm64" ]; then
    echo "release.sh: refusing: this ships a macOS arm64 binary, and this is $(uname -sm)" >&2
    exit 1
fi

command -v gh >/dev/null || { echo "release.sh: gh is not installed" >&2; exit 1; }
command -v shasum >/dev/null || { echo "release.sh: shasum is not installed" >&2; exit 1; }

if [ -n "$(git status --porcelain)" ]; then
    echo "release.sh: refusing: the tree is dirty, commit or stash first" >&2
    exit 1
fi

if gh release view "v$VERSION" >/dev/null 2>&1; then
    echo "release.sh: refusing: v$VERSION already exists" >&2
    exit 1
fi

# The version a person sees is CARGO_PKG_VERSION of the binary, so every
# member moves together rather than the binary drifting from its libraries.
for manifest in crates/*/Cargo.toml; do
    if grep -q '^version = "' "$manifest"; then
        sed -i '' "s/^version = \".*\"/version = \"$VERSION\"/" "$manifest"
    fi
done

cargo build --release -p qoro-cli

BIN=target/release/qoro
REPORTED="$("$BIN" --version)"
echo "built: $REPORTED"
case "$REPORTED" in
    "qoro $VERSION") ;;
    *)
        echo "release.sh: refusing: the binary reports '$REPORTED', want 'qoro $VERSION'" >&2
        exit 1
        ;;
esac

rm -rf dist
mkdir -p dist
cp "$BIN" dist/qoro
tar -czf "dist/qoro-aarch64-apple-darwin.tar.gz" -C dist qoro
rm dist/qoro
(cd dist && shasum -a 256 qoro-aarch64-apple-darwin.tar.gz > qoro-aarch64-apple-darwin.tar.gz.sha256)
cat "dist/qoro-aarch64-apple-darwin.tar.gz.sha256"

PREVIOUS="$(git describe --tags --abbrev=0 2>/dev/null || true)"
if [ -n "$PREVIOUS" ]; then
    NOTES="$(git log "$PREVIOUS..HEAD" --format='- %s')"
else
    NOTES="$(git log --format='- %s' -20)"
fi

git add crates/*/Cargo.toml Cargo.lock
git commit -m "chore: release v$VERSION"
git tag "v$VERSION"
git push origin HEAD
git push origin "v$VERSION"

gh release create "v$VERSION" \
    "dist/qoro-aarch64-apple-darwin.tar.gz" \
    "dist/qoro-aarch64-apple-darwin.tar.gz.sha256" \
    --title "v$VERSION" \
    --notes "$NOTES"

echo "released v$VERSION"

#!/bin/sh
# Local release: build on this machine, publish the files to GitHub Releases.
#
# GitHub never compiles anything here. It only hosts the tarballs, which costs
# no Actions minutes: every build runs on this Mac, and `gh release create`
# uploads the result. Four binaries: macOS arm64 and Intel with cargo's own
# targets, Linux x86_64 and arm64 with cargo-zigbuild against glibc 2.17, so one
# binary starts on every glibc distribution from the last decade.
#
# One-time setup on this machine:
#   rustup target add x86_64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
#   cargo install cargo-zigbuild   (zig itself: brew install zig)
#   softwareupdate --install-rosetta --agree-to-license
# Docker must be running: the Linux binaries are started in a container before
# anything is published, the same way the Mac ones are started here.
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
    echo "release.sh: refusing: releases are built on macOS arm64, and this is $(uname -sm)" >&2
    exit 1
fi

command -v gh >/dev/null || { echo "release.sh: gh is not installed" >&2; exit 1; }
command -v shasum >/dev/null || { echo "release.sh: shasum is not installed" >&2; exit 1; }
command -v cargo-zigbuild >/dev/null || { echo "release.sh: cargo-zigbuild is not installed" >&2; exit 1; }
docker info >/dev/null 2>&1 || { echo "release.sh: docker is not running; the Linux binaries are smoked in it" >&2; exit 1; }
# The Intel Mac binary is started under Rosetta before it is published, and without
# Rosetta that start fails after every Cargo.toml has been stamped. Asked here, first.
[ -e /Library/Apple/usr/libexec/oah/libRosettaRuntime ] || {
    echo "release.sh: Rosetta is not installed: softwareupdate --install-rosetta --agree-to-license" >&2
    exit 1
}

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

TARGETS="aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu"

# Refuse unless the binary itself says the version that was stamped. Every platform is
# started for real: the Intel Mac binary under Rosetta, the Linux ones in CentOS 7, whose
# glibc 2.17 is the oldest the build promises, in a container of their own architecture.
smoke() {
    triple="$1"
    bin="$2"
    case "$triple" in
        aarch64-apple-darwin) "$bin" --version ;;
        x86_64-apple-darwin) arch -x86_64 "$bin" --version ;;
        x86_64-unknown-linux-gnu)
            docker run --rm --platform linux/amd64 -v "$(cd "$(dirname "$bin")" && pwd):/q:ro" \
                centos:7 /q/qoro --version ;;
        aarch64-unknown-linux-gnu)
            docker run --rm --platform linux/arm64 -v "$(cd "$(dirname "$bin")" && pwd):/q:ro" \
                centos:7 /q/qoro --version ;;
    esac
}

rm -rf dist
mkdir -p dist
for triple in $TARGETS; do
    case "$triple" in
        # Linux carries its debug info inside the binary (76 MB against 12 MB on a Mac,
        # which keeps it beside the binary), so it is dropped there; the symbol names
        # stay, which is what a backtrace needs.
        *-linux-gnu) CARGO_PROFILE_RELEASE_STRIP=debuginfo \
            cargo zigbuild --release -p qoro-cli --target "$triple.2.17" ;;
        *) cargo build --release -p qoro-cli --target "$triple" ;;
    esac
    BIN="${CARGO_TARGET_DIR:-target}/$triple/release/qoro"
    REPORTED="$(smoke "$triple" "$BIN")"
    echo "built $triple: $REPORTED"
    case "$REPORTED" in
        "qoro $VERSION") ;;
        *)
            echo "release.sh: refusing: $triple reports '$REPORTED', want 'qoro $VERSION'" >&2
            exit 1
            ;;
    esac
    ASSET="qoro-$triple.tar.gz"
    cp "$BIN" dist/qoro
    tar -czf "dist/$ASSET" -C dist qoro
    rm dist/qoro
    (cd dist && shasum -a 256 "$ASSET" > "$ASSET.sha256")
    cat "dist/$ASSET.sha256"
done

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

gh release create "v$VERSION" dist/*.tar.gz dist/*.tar.gz.sha256 \
    --title "v$VERSION" \
    --notes "$NOTES"

echo "released v$VERSION"

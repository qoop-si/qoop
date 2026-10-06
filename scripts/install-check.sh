#!/bin/sh
# install.sh end to end against a fake release: no network, no real HOME.
#
# QORO_RELEASE_BASE points install.sh at a local directory laid out like the
# API: a `latest` file in its shape, and each asset under `assets/<n>`, the
# path install.sh takes an asset url by. The stand-in `qoro` answers
# --version like the real binary and logs hook/skill delegation.

set -eu

fail() {
    echo "install-check FAIL: $1" >&2
    exit 1
}

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT INT TERM
export T

# A stand-in release packed the way release.sh packs it, under every triple
# install.sh may pick, so the check runs on whichever machine this is.
TRIPLES="aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu"
REL="$T/releases"
mkdir -p "$T/fake" "$REL/assets"
release() {
    # shellcheck disable=SC2016 # expanded by the stand-in when it runs, not here
    printf '#!/bin/sh\nif [ "$1" = "--version" ]; then echo "qoro %s"; else echo "$@" >> "$T/calls"; fi\n' "$1" > "$T/fake/qoro"
    chmod 0755 "$T/fake/qoro"
    n=0
    assets=""
    for triple in $TRIPLES; do
        asset="qoro-$triple.tar.gz"
        tar -czf "$T/fake/$asset" -C "$T/fake" qoro
        (cd "$T/fake" && shasum -a 256 "$asset" > "$asset.sha256")
        for file in "$asset" "$asset.sha256"; do
            n=$((n + 1))
            cp "$T/fake/$file" "$REL/assets/$n"
            assets="$assets${assets:+,}{\"url\": \"file://$REL/assets/$n\", \"name\": \"$file\"}"
        done
    done
    printf '{"tag_name": "v9.9.9", "assets": [%s]}\n' "$assets" > "$REL/latest"
}
release 9.9.9

export QORO_RELEASE_BASE="file://$REL"
export PATH="/usr/bin:/bin:/usr/sbin:/sbin"
export QORO_DEST="$T/dest/qoro"
export HOME="$T/home"
mkdir -p "$T/home"

# Happy path: installs, gates the version, delegates hooks + skills.
sh scripts/install.sh >/dev/null || fail "happy path exited nonzero"
[ -x "$QORO_DEST" ] || fail "binary missing at QORO_DEST"
[ "$("$QORO_DEST" --version)" = "qoro 9.9.9" ] || fail "installed binary reports wrong version"
grep -q "hooks install" "$T/calls" || fail "hooks install was not delegated"
grep -q "skills install" "$T/calls" || fail "skills install was not delegated"
grep -q "bridge reload" "$T/calls" || fail "bridge reload was not delegated"
# The notch on a Mac and nowhere else.
if [ "$(uname -s)" = "Darwin" ]; then
    grep -q "notch install" "$T/calls" || fail "notch install was not delegated on macOS"
else
    grep -q "notch install" "$T/calls" && fail "notch install was delegated off macOS"
fi

# No QORO_DEST: into the home, never a root-owned system directory, with a
# PATH hint since that directory is not on PATH here.
out="$(env -u QORO_DEST sh scripts/install.sh)" || fail "default destination exited nonzero"
[ -x "$HOME/.local/bin/qoro" ] || fail "default install did not land in ~/.local/bin/qoro"
case "$out" in *"add $HOME/.local/bin to PATH"*) ;; *) fail "no PATH hint: $out" ;; esac

# Already at the destination, a link to a dev build: left exactly as it is.
rm "$HOME/.local/bin/qoro"
ln -s "$T/fake/old-dev-build" "$HOME/.local/bin/qoro"
env -u QORO_DEST sh scripts/install.sh >/dev/null 2>&1 || fail "existing destination exited nonzero"
[ "$(readlink "$HOME/.local/bin/qoro")" = "$T/fake/old-dev-build" ] || fail "an existing destination was replaced"

# Existing binary: untouched, pointed at update.
printf '#!/bin/sh\necho "qoro 0.0.1"\n' > "$T/fake/old-qoro"
chmod 0755 "$T/fake/old-qoro"
mkdir -p "$T/oldbin"
cp "$T/fake/old-qoro" "$T/oldbin/qoro"
PATH="$T/oldbin:$PATH" QORO_DEST="$T/dest2/qoro" sh scripts/install.sh >/dev/null || fail "existing-binary path exited nonzero"
[ ! -e "$T/dest2/qoro" ] || fail "existing binary was replaced instead of redirecting to update"

# Wrong version in tarball: refused, nothing installed.
release 1.2.3
QORO_DEST="$T/dest3/qoro" sh scripts/install.sh >/dev/null 2>&1 && fail "version mismatch was accepted"
[ ! -e "$T/dest3/qoro" ] || fail "mismatched binary was installed"

echo "install-check ok"

#!/bin/sh
# install.sh end to end against a fake release: no network, no real HOME.
#
# QOOP_RELEASE_BASE points install.sh at a local directory laid out like the
# API: a `latest` file in its shape, and each asset under `assets/<n>` (GitHub's
# shape) or `v<version>/<file>` (qoop.si's, with the keys in the other order), the
# paths install.sh takes an asset url by. The stand-in `qoop` answers
# --version like the real binary and logs hook/skill delegation.

set -eu

fail() {
    echo "install-check FAIL: $1" >&2
    exit 1
}

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT INT TERM
export T
# install.sh makes its temp dir here, so what it leaves behind can be seen.
mkdir -p "$T/tmp"
export TMPDIR="$T/tmp"

# A stand-in release packed the way the release script packs it, under every triple
# install.sh may pick, so the check runs on whichever machine this is.
TRIPLES="aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu"
REL="$T/releases"
mkdir -p "$T/fake" "$REL/assets"
release() {
    # shellcheck disable=SC2016 # expanded by the stand-in when it runs, not here
    printf '#!/bin/sh\nif [ "$1" = "--version" ]; then echo "qoop %s"; else echo "$@" >> "$T/calls"; fi\n' "$1" > "$T/fake/qoop"
    chmod 0755 "$T/fake/qoop"
    echo extra > "$T/fake/extra"
    rm -rf "$REL/assets" "$REL/v9.9.9"
    mkdir -p "$REL/assets" "$REL/v9.9.9"
    n=0
    assets=""
    hub=""
    for triple in $TRIPLES; do
        asset="qoop-$triple.tar.gz"
        tar -czf "$T/fake/$asset" -C "$T/fake" qoop extra
        (cd "$T/fake" && shasum -a 256 "$asset" > "$asset.sha256")
        for file in "$asset" "$asset.sha256"; do
            n=$((n + 1))
            cp "$T/fake/$file" "$REL/assets/$n"
            cp "$T/fake/$file" "$REL/v9.9.9/$file"
            assets="$assets${assets:+,}{\"url\": \"file://$REL/assets/$n\", \"name\": \"$file\"}"
            hub="$hub${hub:+,}{\"name\": \"$file\", \"url\": \"file://$REL/v9.9.9/$file\"}"
        done
    done
    # The release always says 9.9.9; what the tarball's binary says is the argument, so a
    # liar can be made.
    printf '{"tag_name": "v9.9.9", "assets": [%s]}\n' "$assets" > "$REL/latest"
    printf '{"assets": [%s], "tag_name": "v9.9.9"}\n' "$hub" > "$REL/hub-latest"
}
release 9.9.9

export QOOP_RELEASE_BASE="file://$REL"
# A logging tar in front, to see which members install.sh asks for.
mkdir -p "$T/shim"
# shellcheck disable=SC2016 # expanded by the shim when it runs, not here
printf '#!/bin/sh\necho "$@" >> "$T/tar-calls"\nexec /usr/bin/tar "$@"\n' > "$T/shim/tar"
chmod 0755 "$T/shim/tar"
export PATH="$T/shim:/usr/bin:/bin:/usr/sbin:/sbin"
export QOOP_DEST="$T/dest/qoop"
export HOME="$T/home"
mkdir -p "$T/home"

# Happy path: installs, gates the version, delegates hooks + skills.
sh scripts/install.sh >/dev/null || fail "happy path exited nonzero"
[ -x "$QOOP_DEST" ] || fail "binary missing at QOOP_DEST"
[ "$("$QOOP_DEST" --version)" = "qoop 9.9.9" ] || fail "installed binary reports wrong version"
[ ! -e "$T/dest/qoro" ] && [ ! -L "$T/dest/qoro" ] || fail "a fresh install left the old name behind"
grep -q "hooks install" "$T/calls" || fail "hooks install was not delegated"
grep -q "skills install" "$T/calls" || fail "skills install was not delegated"
grep -q "bridge reload" "$T/calls" || fail "bridge reload was not delegated"
# The notch and the icon font on a Mac and nowhere else.
for what in notch icons; do
    if [ "$(uname -s)" = "Darwin" ]; then
        grep -q "$what install" "$T/calls" || fail "$what install was not delegated on macOS"
    else
        grep -q "$what install" "$T/calls" && fail "$what install was delegated off macOS"
    fi
done

# Only the binary is asked of the tarball, and the temp dir is gone afterwards.
grep -q "qoop$" "$T/tar-calls" || fail "tar was not asked for the qoop member alone"
grep -q "extra" "$T/tar-calls" && fail "tar was asked for more than the qoop member"
[ -z "$(ls -A "$T/tmp")" ] || fail "install.sh left its temp dir behind"

# No QOOP_DEST, and the script on stdin as `curl | sh` gives it: into the home, never a root-owned system directory, with a
# PATH hint since that directory is not on PATH here.
out="$(env -u QOOP_DEST sh <scripts/install.sh)" || fail "default destination exited nonzero"
[ -x "$HOME/.local/bin/qoop" ] || fail "default install did not land in ~/.local/bin/qoop"
case "$out" in *"add $HOME/.local/bin to PATH"*) ;; *) fail "no PATH hint: $out" ;; esac
# The next step, by full path since the name does not resolve yet.
case "$out" in *"next: $HOME/.local/bin/qoop login"*) ;; *) fail "no next step by full path: $out" ;; esac
# Not a terminal: no onboarding wizard asked for.
grep -q "^onboard" "$T/calls" && fail "the wizard ran with no terminal answering"
[ -z "$(ls -A "$T/tmp")" ] || fail "the default install left its temp dir behind"

# The qoop.si layout: asset urls under releases/v<version>/, keys in the other order. The
# latest file is named hub-latest, so it is laid out as the base's own `latest`.
mv "$REL/latest" "$REL/api-latest"
cp "$REL/hub-latest" "$REL/latest"
rm -rf "$T/dest5"
QOOP_DEST="$T/dest5/qoop" sh scripts/install.sh >/dev/null || fail "the qoop.si layout exited nonzero"
[ "$("$T/dest5/qoop" --version)" = "qoop 9.9.9" ] || fail "the qoop.si layout installed the wrong binary"
mv "$REL/api-latest" "$REL/latest"

# A base that is not https (and not a local directory) is refused by curl itself, not fetched.
QOOP_RELEASE_BASE="http://127.0.0.1:9/releases" QOOP_DEST="$T/dest6/qoop" sh scripts/install.sh >"$T/http-out" 2>&1 &&
    fail "an http base was accepted"
grep -qE "not supported|disabled" "$T/http-out" || fail "an http base failed for another reason: $(cat "$T/http-out")"
[ ! -e "$T/dest6/qoop" ] || fail "an http base installed something"

# The profile line is the shell's own: fish has no `export`, and a Mac's bash reads .bash_profile.
out="$(SHELL=/usr/bin/fish QOOP_DEST="$T/fish/qoop" sh scripts/install.sh)" || fail "fish install exited nonzero"
case "$out" in *'fish_add_path'*) ;; *) fail "no fish_add_path hint: $out" ;; esac
case "$out" in *'export PATH'*) fail "a bash line was given to fish: $out" ;; esac
out="$(SHELL=/bin/bash QOOP_DEST="$T/bash/qoop" sh scripts/install.sh)" || fail "bash install exited nonzero"
if [ "$(uname -s)" = "Darwin" ]; then rc=.bash_profile; else rc=.bashrc; fi
case "$out" in *">> ~/$rc"*) ;; *) fail "wrong profile for bash: $out" ;; esac

# A file of the person's beside the destination is never staged over or removed, and nothing
# but the binary is left there.
mkdir -p "$T/dest7"
echo keep > "$T/dest7/qoop.qoop-new"
QOOP_DEST="$T/dest7/qoop" sh scripts/install.sh >/dev/null || fail "install beside a stray file exited nonzero"
[ "$(cat "$T/dest7/qoop.qoop-new")" = keep ] || fail "a file beside the destination was touched"
[ "$(find "$T/dest7" -type f -o -type l | wc -l | tr -d ' ')" = 2 ] || fail "install left stray files in dest7"

# A manifest that points outside its own base is refused before anything is downloaded.
mkdir -p "$T/elsewhere"
sed "s|file://$REL/|file://$T/elsewhere/releases/|g" "$REL/latest" > "$T/latest.out"
cp "$REL/latest" "$T/latest.keep"
cp "$T/latest.out" "$REL/latest"
QOOP_DEST="$T/dest8/qoop" sh scripts/install.sh >"$T/out8" 2>&1 && fail "a manifest pointing elsewhere was accepted"
grep -q "points outside" "$T/out8" || fail "no reason given for the refused manifest: $(cat "$T/out8")"
[ ! -e "$T/dest8/qoop" ] || fail "a manifest pointing elsewhere installed something"
cp "$T/latest.keep" "$REL/latest"

# Already at the destination, a link to a dev build: left exactly as it is.
rm "$HOME/.local/bin/qoop"
ln -s "$T/fake/old-dev-build" "$HOME/.local/bin/qoop"
env -u QOOP_DEST sh scripts/install.sh >/dev/null 2>&1 || fail "existing destination exited nonzero"
[ "$(readlink "$HOME/.local/bin/qoop")" = "$T/fake/old-dev-build" ] || fail "an existing destination was replaced"

# Existing binary: untouched, pointed at update.
printf '#!/bin/sh\necho "qoop 0.0.1"\n' > "$T/fake/old-qoop"
chmod 0755 "$T/fake/old-qoop"
mkdir -p "$T/oldbin"
cp "$T/fake/old-qoop" "$T/oldbin/qoop"
PATH="$T/oldbin:$PATH" QOOP_DEST="$T/dest2/qoop" sh scripts/install.sh >/dev/null || fail "existing-binary path exited nonzero"
[ ! -e "$T/dest2/qoop" ] || fail "existing binary was replaced instead of redirecting to update"

# An install that still has the old name counts as an install.
printf '#!/bin/sh\necho "qoro 0.0.1"\n' > "$T/fake/old-qoro"
chmod 0755 "$T/fake/old-qoro"
mkdir -p "$T/oldname"
cp "$T/fake/old-qoro" "$T/oldname/qoro"
PATH="$T/oldname:$PATH" QOOP_DEST="$T/dest4/qoop" sh scripts/install.sh >/dev/null || fail "old-name path exited nonzero"
[ ! -e "$T/dest4/qoop" ] || fail "an install under the old name was not left to update"

# Wrong version in tarball: refused, nothing installed.
release 1.2.3
QOOP_DEST="$T/dest3/qoop" sh scripts/install.sh >/dev/null 2>&1 && fail "version mismatch was accepted"
[ ! -e "$T/dest3/qoop" ] || fail "mismatched binary was installed"
[ -z "$(ls -A "$T/tmp")" ] || fail "a refused install left its temp dir behind"

echo "install-check ok"

#!/bin/sh
# install.sh end to end against a fake release: no network, no real HOME.
#
# A `curl` shim serves the release.json fixture; asset downloads go over
# file:// to a packed stand-in tarball. The stand-in `qoro` answers
# --version like the real binary and logs hook/skill delegation.

set -eu

fail() {
    echo "install-check FAIL: $1" >&2
    exit 1
}

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT INT TERM
export T

# A stand-in release: version 9.9.9, packed the way release.sh packs it.
mkdir -p "$T/fake"
printf '#!/bin/sh\nif [ "$1" = "--version" ]; then echo "qoro 9.9.9"; else echo "$@" >> "$T/calls"; fi\n' > "$T/fake/qoro"
chmod 0755 "$T/fake/qoro"
tar -czf "$T/fake/qoro-aarch64-apple-darwin.tar.gz" -C "$T/fake" qoro
(cd "$T/fake" && shasum -a 256 -c qoro-aarch64-apple-darwin.tar.gz.sha256 >/dev/null) 2>/dev/null || {
    (cd "$T/fake" && shasum -a 256 qoro-aarch64-apple-darwin.tar.gz > qoro-aarch64-apple-darwin.tar.gz.sha256)
}
cat > "$T/fake/release.json" <<EOF
{"tag_name": "v9.9.9", "assets": [
  {"name": "qoro-aarch64-apple-darwin.tar.gz", "browser_download_url": "file://$T/fake/qoro-aarch64-apple-darwin.tar.gz"},
  {"name": "qoro-aarch64-apple-darwin.tar.gz.sha256", "browser_download_url": "file://$T/fake/qoro-aarch64-apple-darwin.tar.gz.sha256"}
]}
EOF

# curl shim: the API call gets the fixture, everything else is real curl.
mkdir -p "$T/bin"
cat > "$T/bin/curl" <<EOF
#!/bin/sh
for arg in "\$@"; do
    case "\$arg" in
        *api.github.com*) out=""; prev=""; for a in "\$@"; do
            [ "\$prev" = "-o" ] && out="\$a"; prev="\$a"; done
            cp "$T/fake/release.json" "\$out"; exit 0;;
    esac
done
exec /usr/bin/curl "\$@"
EOF
chmod 0755 "$T/bin/curl"

export PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export QORO_DEST="$T/dest/qoro"
export HOME="$T/home"
mkdir -p "$T/home"

# Happy path: installs, gates the version, delegates hooks + skills.
sh scripts/install.sh >/dev/null || fail "happy path exited nonzero"
[ -x "$QORO_DEST" ] || fail "binary missing at QORO_DEST"
[ "$("$QORO_DEST" --version)" = "qoro 9.9.9" ] || fail "installed binary reports wrong version"
grep -q "hooks install" "$T/calls" || fail "hooks install was not delegated"
grep -q "skills install" "$T/calls" || fail "skills install was not delegated"

# Existing binary: untouched, pointed at update.
printf '#!/bin/sh\necho "qoro 0.0.1"\n' > "$T/fake/old-qoro"
chmod 0755 "$T/fake/old-qoro"
mkdir -p "$T/oldbin"
cp "$T/fake/old-qoro" "$T/oldbin/qoro"
PATH="$T/oldbin:$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" QORO_DEST="$T/dest2/qoro" sh scripts/install.sh >/dev/null || fail "existing-binary path exited nonzero"
[ ! -e "$T/dest2/qoro" ] || fail "existing binary was replaced instead of redirecting to update"

# Wrong version in tarball: refused, nothing installed.
printf '#!/bin/sh\necho "qoro 1.2.3"\n' > "$T/fake/qoro"
chmod 0755 "$T/fake/qoro"
tar -czf "$T/fake/qoro-aarch64-apple-darwin.tar.gz" -C "$T/fake" qoro
(cd "$T/fake" && shasum -a 256 qoro-aarch64-apple-darwin.tar.gz > qoro-aarch64-apple-darwin.tar.gz.sha256)
QORO_DEST="$T/dest3/qoro" sh scripts/install.sh >/dev/null 2>&1 && fail "version mismatch was accepted"
[ ! -e "$T/dest3/qoro" ] || fail "mismatched binary was installed"

echo "install-check ok"

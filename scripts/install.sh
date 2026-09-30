#!/bin/sh
# Fresh-machine install: binary, hooks, skills. One command, no questions.
#
# Complementary to `qoro update`, not a copy of it: update replaces a live
# binary in place and refuses while sessions run; this lays a machine out
# from nothing. An existing binary that is older is not touched here;
# `qoro update` owns that path.
#
# Fail closed: no network, no install. Rerunning changes nothing.
#
# Into the person's home, never as root: `/usr/local/bin` needs sudo on a stock
# Mac and on Linux, and under sudo the hooks and skill land root-owned where the
# person's agents cannot run them, and every later `qoro update` needs sudo too.

set -eu

# Only under sudo: root on its own box (a container, a server) installing into its own
# home is the person, and gets what anyone else gets.
[ -z "${SUDO_USER:-}" ] || [ "$(id -u)" -ne 0 ] || {
    echo "qoro install: run it as yourself, not with sudo: the binary, hooks and skill all go into your home" >&2
    exit 1
}

REPO="pat/qoro"
DEST="${QORO_DEST:-$HOME/.local/bin/qoro}"
# A mirror, or a local directory as file:///path holding a `latest` file in the API's own
# shape: how the platform pick is tested with no network. `qoro update` reads the same.
BASE="${QORO_RELEASE_BASE:-https://api.github.com/repos/$REPO/releases}"

# The tarball scripts/release.sh packs for this machine, by target triple.
MACHINE="$(uname -m)"
# A shell running under Rosetta reports x86_64 on an Apple silicon Mac; the machine
# itself is arm64, and that is the binary that runs natively.
if [ "$(uname -s)" = "Darwin" ] && [ "$(sysctl -in sysctl.proc_translated 2>/dev/null)" = "1" ]; then
    MACHINE="arm64"
fi
case "$(uname -s) $MACHINE" in
    "Darwin arm64") TRIPLE="aarch64-apple-darwin" ;;
    "Darwin x86_64") TRIPLE="x86_64-apple-darwin" ;;
    "Linux x86_64") TRIPLE="x86_64-unknown-linux-gnu" ;;
    "Linux aarch64" | "Linux arm64") TRIPLE="aarch64-unknown-linux-gnu" ;;
    *)
        echo "qoro install: no prebuilt binary for $(uname -s) $(uname -m);" \
            "build from source: cargo build --release -p qoro-cli" >&2
        exit 1
        ;;
esac
# The Linux binaries link glibc. On musl (Alpine) they cannot start at all, so say that
# here rather than after a download that could never run.
if [ "$(uname -s)" = "Linux" ] && ldd --version 2>&1 | grep -qi musl; then
    echo "qoro install: this is a musl system; the prebuilt Linux binary needs glibc." \
        "Build from source: cargo build --release -p qoro-cli" >&2
    exit 1
fi
ASSET="qoro-$TRIPLE.tar.gz"

if command -v qoro >/dev/null 2>&1; then
    echo "qoro is already on PATH: $(command -v qoro)" >&2
    echo "for a newer release run: qoro update" >&2
    exit 0
fi
# Not on PATH but already there, a link to a dev build for one: left exactly as it is.
if [ -e "$DEST" ] || [ -L "$DEST" ]; then
    echo "qoro is already at $DEST, not on PATH; nothing replaced" >&2
    echo "for a newer release run: $DEST update" >&2
    exit 0
fi

for tool in curl tar awk sed; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "qoro install: missing required tool: $tool" >&2
        exit 1
    }
done
# Every Mac has shasum; slim Linux images have sha256sum and no perl.
if command -v shasum >/dev/null 2>&1; then
    SHA256="shasum -a 256"
elif command -v sha256sum >/dev/null 2>&1; then
    SHA256="sha256sum"
else
    echo "qoro install: missing required tool: shasum or sha256sum" >&2
    exit 1
fi

# The repository is private, so without a token nothing is found at all. The
# environment first, then the token `gh` logged in with.
TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
if [ -z "$TOKEN" ] && command -v gh >/dev/null 2>&1; then
    TOKEN="$(gh auth token 2>/dev/null || true)"
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK" "$DEST.qoro-new"' EXIT INT TERM

# GET a URL into a file: accept type, max seconds, file, URL.
# The token goes only to api.github.com, never to a mirror named by QORO_RELEASE_BASE, and
# only on stdin as a curl config line (`-K -`, printf is a shell builtin), never in an argv
# any process listing shows; `-K` is understood by every curl, unlike `-H @-` (7.55). A
# redirect to the storage host is followed by hand and without the token, because curl
# older than 7.58 sent a custom Authorization header on to the redirected host.
api_get() {
    case "$4" in
        https://api.github.com/*) with_token="$TOKEN" ;;
        *) with_token="" ;;
    esac
    if [ -n "$with_token" ]; then
        next="$(printf 'header = "Authorization: Bearer %s"\n' "$with_token" |
            curl -fsS --max-time "$2" -H "Accept: $1" -K - -o "$3" -w '%{redirect_url}' "$4")" ||
            return 1
        [ -z "$next" ] || curl -fsSL --max-time "$2" -o "$3" "$next"
    else
        curl -fsSL --max-time "$2" -H "Accept: $1" -o "$3" "$4"
    fi
}

release_json="$WORK/release.json"
api_get application/vnd.github+json 25 "$release_json" "$BASE/latest" || {
    echo "qoro install: cannot reach GitHub Releases" >&2
    exit 1
}

# Read with sed and awk rather than a JSON library, because a fresh Linux box has no
# python; plain `[ \t]` rather than character classes, which mawk 1.3.3 (Debian 10,
# Ubuntu 18.04) does not know. The API's own `url` of an asset, found by name: not `browser_download_url`,
# which 404s for a private repository even with a token. In the API's answer an asset's
# `url` comes before its `name`, so the last asset url seen is the one a matching name
# belongs to.
version="$(tr -d '\n' <"$release_json" |
    sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p')"
asset_url_of() {
    tr ',{}' '\n\n\n' <"$release_json" | awk -v want="$1" '
        /"url"[ \t]*:/ && /\/releases\/assets\/[0-9]+"/ {
            url = $0
            sub(/.*"url"[ \t]*:[ \t]*"/, "", url)
            sub(/".*/, "", url)
        }
        /"name"[ \t]*:/ {
            name = $0
            sub(/.*"name"[ \t]*:[ \t]*"/, "", name)
            sub(/".*/, "", name)
            if (name == want && url != "") { print url; exit }
        }'
}
[ -n "$version" ] || {
    echo "qoro install: the newest release names no version" >&2
    exit 1
}
asset_url="$(asset_url_of "$ASSET")"
checksum_url="$(asset_url_of "$ASSET.sha256")"
[ -n "$asset_url" ] || {
    echo "qoro install: the newest release carries no $ASSET" >&2
    exit 1
}
[ -n "$checksum_url" ] || {
    echo "qoro install: the newest release carries no $ASSET.sha256" >&2
    exit 1
}

api_get application/octet-stream 120 "$WORK/$ASSET" "$asset_url" || {
    echo "qoro install: cannot download $ASSET" >&2
    exit 1
}
api_get application/octet-stream 120 "$WORK/$ASSET.sha256" "$checksum_url" || {
    echo "qoro install: cannot download $ASSET.sha256" >&2
    exit 1
}

(cd "$WORK" && $SHA256 -c "$ASSET.sha256") || {
    echo "qoro install: checksum mismatch" >&2
    exit 1
}
tar -xzf "$WORK/$ASSET" -C "$WORK"
got="$("$WORK/qoro" --version 2>/dev/null)" || {
    echo "qoro install: the downloaded binary does not run" >&2
    exit 1
}
[ "$got" = "qoro $version" ] || {
    echo "qoro install: refusing: the download reports $got, want qoro $version" >&2
    exit 1
}

# Atomic into place, executable.
tmp="$DEST.qoro-new"
mkdir -p "$(dirname "$DEST")" || {
    echo "qoro install: cannot create $(dirname "$DEST")" >&2
    exit 1
}
cp "$WORK/qoro" "$tmp"
chmod 0755 "$tmp"
mv "$tmp" "$DEST"
echo "installed $($DEST --version) to $DEST"

# The rest is the CLI's own verbs, never a copy of their logic. Each one on its own: a
# hooks directory the CLI refuses is no reason to leave the skill and the PATH hint out.
"$DEST" hooks install || echo "qoro install: hooks were not installed; see the message above" >&2
"$DEST" skills install || echo "qoro install: the skill was not installed; see the message above" >&2
# On a Mac, the agents in the notch, started at login. Never the reason an install
# fails: a Mac older than 14 says so, and everything above stays installed.
if [ "$(uname -s)" = "Darwin" ]; then
    "$DEST" notch install || echo "the notch was not turned on; the rest of qoro is installed"
fi

# The questions the install asks, and only when there is a person to answer them:
# pick the terminal pet, and whether a finished or waiting agent plays a sound. Piped
# runs and CI stay non-interactive by construction, and quitting keeps the install
# just done. The sound is asked once: a config.toml that already says is kept.
if [ -t 0 ] && [ -z "${QORO_NONINTERACTIVE:-}" ]; then
    "$DEST" avatar pick || true
    cfg=$("$DEST" config | head -n 1 | sed 's/  (no file.*//')
    if ! grep -Eqs '(^|[.{,[:space:]])sound[[:space:]]*=' "$cfg"; then
        printf 'play a sound when an agent finishes or needs you? [Y/n] '
        read -r answer || answer=n
        case "$answer" in
            [nN]*) sound=false ;;
            *) sound=true ;;
        esac
        "$DEST" config set notify.sound "$sound" || true
    fi
else
    echo "pick an avatar when ready: qoro avatar pick"
    echo "sounds when an agent finishes or needs you: qoro config set notify.sound true"
fi

# The hooks call `qoro` by name, so it has to be on PATH. Said, not done: an rc
# file is the person's own.
if ! command -v qoro >/dev/null 2>&1; then
    case "${SHELL:-}" in
        */zsh) rc=.zshrc ;;
        */bash) rc=.bashrc ;;
        *) rc=.profile ;;
    esac
    echo "add $(dirname "$DEST") to PATH, e.g. echo 'export PATH=\"$(dirname "$DEST"):\$PATH\"' >> ~/$rc"
fi

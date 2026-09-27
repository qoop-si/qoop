#!/bin/sh
# Fresh-machine install: binary, hooks, skills. One command, no questions.
#
# Complementary to `qoro update`, not a copy of it: update replaces a live
# binary in place and refuses while sessions run; this lays a machine out
# from nothing. An existing binary that is older is not touched here;
# `qoro update` owns that path.
#
# Fail closed: no network, no install. Rerunning changes nothing.

set -eu

REPO="pat/qoro"
ASSET="qoro-aarch64-apple-darwin.tar.gz"
DEST="${QORO_DEST:-/usr/local/bin/qoro}"

if [ "$(uname -s)" != "Darwin" ] || [ "$(uname -m)" != "arm64" ]; then
    echo "qoro install: macOS arm64 only ($(uname -s) $(uname -m))" >&2
    exit 1
fi

if command -v qoro >/dev/null 2>&1; then
    echo "qoro is already on PATH: $(command -v qoro)" >&2
    echo "for a newer release run: qoro update" >&2
    exit 0
fi

for tool in curl python3 shasum tar; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "qoro install: missing required tool: $tool" >&2
        exit 1
    }
done

# The repository is private, so without a token nothing is found at all. The
# environment first, then the token `gh` logged in with.
TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
if [ -z "$TOKEN" ] && command -v gh >/dev/null 2>&1; then
    TOKEN="$(gh auth token 2>/dev/null || true)"
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK" "$DEST.qoro-new"' EXIT INT TERM

# GET an api.github.com URL into a file: accept type, max seconds, file, URL.
# The token goes to curl on stdin (`-H @-`, printf is a shell builtin), never in
# an argv any process listing shows. `-L` follows an asset's redirect to the
# storage host and curl drops the header there, so the token stays with the API.
api_get() {
    if [ -n "$TOKEN" ]; then
        printf 'Authorization: Bearer %s\n' "$TOKEN" |
            curl -fsSL --max-time "$2" -H "Accept: $1" -H @- -o "$3" "$4"
    else
        curl -fsSL --max-time "$2" -H "Accept: $1" -o "$3" "$4"
    fi
}

release_json="$WORK/release.json"
api_get application/vnd.github+json 25 "$release_json" \
    "https://api.github.com/repos/$REPO/releases/latest" || {
    echo "qoro install: cannot reach GitHub Releases" >&2
    exit 1
}

version="$(python3 -c "
import json
tag = json.load(open('$release_json'))['tag_name']
print(tag[1:] if tag.startswith('v') else tag)
")"
# The API `url` of an asset, found by name. Not `browser_download_url`, which
# 404s for a private repository even with a token.
asset_url_of() {
    python3 -c "
import json
for a in json.load(open('$release_json'))['assets']:
    if a['name'] == '$1':
        print(a['url'])
        break
"
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

(cd "$WORK" && shasum -a 256 -c "$ASSET.sha256") || {
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

# The rest is the CLI's own verbs, never a copy of their logic.
"$DEST" hooks install
"$DEST" skills install

# The one question the install asks, and only when there is a person to answer
# it: pick the terminal pet. Piped runs and CI stay non-interactive by
# construction, and quitting keeps the install just done.
if [ -t 0 ] && [ -z "${QORO_NONINTERACTIVE:-}" ]; then
    "$DEST" avatar pick || true
else
    echo "pick an avatar when ready: qoro avatar pick"
fi

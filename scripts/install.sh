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

AUTH_HEADER=""
if [ -n "${GH_TOKEN:-${GITHUB_TOKEN:-}}" ]; then
    TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
    AUTH_HEADER="Authorization: Bearer $TOKEN"
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK" "$DEST.qoro-new"' EXIT INT TERM

release_json="$WORK/release.json"
if [ -n "$AUTH_HEADER" ]; then
    curl -fsSL --max-time 25 -H "$AUTH_HEADER" -o "$release_json" \
        "https://api.github.com/repos/$REPO/releases/latest" || {
        echo "qoro install: cannot reach GitHub Releases" >&2
        exit 1
    }
else
    curl -fsSL --max-time 25 -o "$release_json" \
        "https://api.github.com/repos/$REPO/releases/latest" || {
        echo "qoro install: cannot reach GitHub Releases" >&2
        exit 1
    }
fi

version="$(python3 -c "
import json
tag = json.load(open('$release_json'))['tag_name']
print(tag[1:] if tag.startswith('v') else tag)
")"
asset_url="$(python3 -c "
import json,sys
for a in json.load(open('$release_json'))['assets']:
    if a['name'] == '$ASSET':
        print(a['browser_download_url'])
        break
")"
checksum_url="${asset_url}.sha256"
[ -n "$asset_url" ] || {
    echo "qoro install: the newest release carries no $ASSET" >&2
    exit 1
}

if [ -n "$AUTH_HEADER" ]; then
    curl -fsSL --max-time 120 -H "$AUTH_HEADER" -o "$WORK/$ASSET" "$asset_url" || {
        echo "qoro install: cannot download $ASSET" >&2
        exit 1
    }
    curl -fsSL --max-time 120 -H "$AUTH_HEADER" -o "$WORK/$ASSET.sha256" "$checksum_url" || {
        echo "qoro install: cannot download $ASSET.sha256" >&2
        exit 1
    }
else
    curl -fsSL --max-time 120 -o "$WORK/$ASSET" "$asset_url" || {
        echo "qoro install: cannot download $ASSET" >&2
        exit 1
    }
    curl -fsSL --max-time 120 -o "$WORK/$ASSET.sha256" "$checksum_url" || {
        echo "qoro install: cannot download $ASSET.sha256" >&2
        exit 1
    }
fi

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

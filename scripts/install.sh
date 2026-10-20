#!/bin/sh
# Fresh-machine install: binary, hooks, skills, icon font. One command, no questions.
#
# Complementary to `qoop update`, not a copy of it: update replaces a live
# binary in place and refuses while sessions run; this lays a machine out
# from nothing. An existing binary that is older is not touched here;
# `qoop update` owns that path.
#
# Fail closed: no network, no install. Rerunning changes nothing.
#
# Written to be piped (`curl -fsSL https://qoop.si/install.sh | sh`, or the same file from a
# qoop.si/i/ link): everything runs from
# `main`, called on the last line, so a connection that drops mid-download runs nothing.
#
# Into the person's home, never as root: `/usr/local/bin` needs sudo on a stock
# Mac and on Linux, and under sudo the hooks and skill land root-owned where the
# person's agents cannot run them, and every later `qoop update` needs sudo too.

set -eu

main() {
    # Only under sudo: root on its own box (a container, a server) installing into its own
    # home is the person, and gets what anyone else gets.
    [ -z "${SUDO_USER:-}" ] || [ "$(id -u)" -ne 0 ] || {
        echo "qoop install: run it as yourself, not with sudo: the binary, hooks and skill all go into your home" >&2
        exit 1
    }

    DEST="${QOOP_DEST:-$HOME/.local/bin/qoop}"
    # A mirror, or a local directory as file:///path holding a `latest` file in the API's own
    # shape: how the platform pick is tested with no network. `qoop update` reads the same.
    # GitHub's own API works too, when named here.
    BASE="${QOOP_RELEASE_BASE:-https://qoop.si/releases}"
    BASE="${BASE%/}"

    # The tarball the release script packs for this machine, by target triple.
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
            echo "qoop install: no prebuilt binary for $(uname -s) $(uname -m);" \
                "build from source: cargo build --release -p qoop-cli" >&2
            exit 1
            ;;
    esac
    # The Linux binaries link glibc. On musl (Alpine) they cannot start at all, so say that
    # here rather than after a download that could never run.
    if [ "$(uname -s)" = "Linux" ] && ldd --version 2>&1 | grep -qi musl; then
        echo "qoop install: this is a musl system; the prebuilt Linux binary needs glibc." \
            "Build from source: cargo build --release -p qoop-cli" >&2
        exit 1
    fi
    ASSET="qoop-$TRIPLE.tar.gz"

    # `qoro` is what this was called before the rename; an install under that name is one.
    for name in qoop qoro; do
        if command -v "$name" >/dev/null 2>&1; then
            echo "qoop is already on PATH: $(command -v "$name")" >&2
            echo "for a newer release run: $name update" >&2
            exit 0
        fi
    done
    # Not on PATH but already there, a link to a dev build for one: left exactly as it is.
    if [ -e "$DEST" ] || [ -L "$DEST" ]; then
        echo "qoop is already at $DEST, not on PATH; nothing replaced" >&2
        echo "for a newer release run: $DEST update" >&2
        exit 0
    fi

    for tool in curl tar awk sed; do
        command -v "$tool" >/dev/null 2>&1 || {
            echo "qoop install: missing required tool: $tool" >&2
            exit 1
        }
    done
    # Every Mac has shasum; slim Linux images have sha256sum and no perl.
    if command -v shasum >/dev/null 2>&1; then
        SHA256="shasum -a 256"
    elif command -v sha256sum >/dev/null 2>&1; then
        SHA256="sha256sum"
    else
        echo "qoop install: missing required tool: shasum or sha256sum" >&2
        exit 1
    fi

    # Only GitHub's API wants a token (its repository is private): the environment first,
    # then the one `gh` logged in with. qoop.si and a mirror are read anonymously.
    TOKEN=""
    case "$BASE" in
        https://api.github.com/*)
            TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
            if [ -z "$TOKEN" ] && command -v gh >/dev/null 2>&1; then
                TOKEN="$(gh auth token 2>/dev/null || true)"
            fi
            ;;
    esac

    WORK="$(mktemp -d)"
    # The temp dir goes on every way out. A signal has to end the script itself: a trap that
    # only cleans up would let it run on into a directory that is gone.
    TMPNEW=""
    trap 'rm -rf "$WORK"; [ -z "$TMPNEW" ] || rm -f "$TMPNEW"' EXIT
    trap 'exit 1' HUP INT TERM

    # curl, over https only, except for a local directory given as file://. A busy server
    # (the hub answers 503 with Retry-After) is waited out a few times; a 404 is not retried.
    fetch() {
        case "$BASE" in
            file://*) curl "$@" ;;
            *) curl --proto '=https' --retry 3 --retry-delay 5 "$@" ;;
        esac
    }

    # GET a URL into a file: accept type, max seconds, file, URL.
    # The token goes only to api.github.com, never to a mirror named by QOOP_RELEASE_BASE, and
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
                fetch -fsS --max-time "$2" -H "Accept: $1" -K - -o "$3" -w '%{redirect_url}' "$4")" ||
                return 1
            [ -z "$next" ] || fetch -fsSL --max-time "$2" -o "$3" "$next"
        else
            fetch -fsSL --max-time "$2" -H "Accept: $1" -o "$3" "$4"
        fi
    }

    release_json="$WORK/release.json"
    api_get application/vnd.github+json 25 "$release_json" "$BASE/latest" || {
        echo "qoop install: cannot reach the releases" >&2
        exit 1
    }

    # Read with sed and awk rather than a JSON library, because a fresh Linux box has no
    # python; plain `[ \t]` rather than character classes, which mawk 1.3.3 (Debian 10,
    # Ubuntu 18.04) does not know. Each asset object is one line, split at the braces, so the
    # order of its keys does not matter. Its `url` is the API's own, found by name: not
    # `browser_download_url`, which 404s for a private repository even with a token. The
    # shapes taken are GitHub's `.../releases/assets/<n>` and qoop.si's
    # `.../releases/v<version>/<file>`.
    version="$(tr -d '\n' <"$release_json" |
        sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p')"
    asset_url_of() {
        tr -d '\n' <"$release_json" | tr '{}' '\n' | awk -v want="$1" '
            function field(key,    m) {
                if (!match($0, "\"" key "\"[ \t]*:[ \t]*\"[^\"]*\"")) return ""
                m = substr($0, RSTART, RLENGTH)
                sub(/^[^:]*:[ \t]*"/, "", m)
                sub(/"$/, "", m)
                return m
            }
            field("name") == want {
                url = field("url")
                if (url ~ /\/releases\/(assets\/[0-9]+|v[0-9.]+\/[^\/]+)$/) { print url; exit }
            }'
    }
    [ -n "$version" ] || {
        echo "qoop install: the newest release names no version" >&2
        exit 1
    }
    asset_url="$(asset_url_of "$ASSET")"
    checksum_url="$(asset_url_of "$ASSET.sha256")"
    [ -n "$asset_url" ] || {
        echo "qoop install: the newest release carries no $ASSET" >&2
        exit 1
    }
    [ -n "$checksum_url" ] || {
        echo "qoop install: the newest release carries no $ASSET.sha256" >&2
        exit 1
    }
    # The files are the base's own: a manifest cannot send the download elsewhere.
    for url in "$asset_url" "$checksum_url"; do
        case "$url" in
            "$BASE"/*) ;;
            *)
                echo "qoop install: the release points outside $BASE: $url" >&2
                exit 1
                ;;
        esac
    done

    api_get application/octet-stream 120 "$WORK/$ASSET" "$asset_url" || {
        echo "qoop install: cannot download $ASSET" >&2
        exit 1
    }
    api_get application/octet-stream 120 "$WORK/$ASSET.sha256" "$checksum_url" || {
        echo "qoop install: cannot download $ASSET.sha256" >&2
        exit 1
    }

    (cd "$WORK" && $SHA256 -c "$ASSET.sha256") || {
        echo "qoop install: checksum mismatch" >&2
        exit 1
    }
    # Only the binary comes out of the tarball, whatever else it holds.
    tar -xzf "$WORK/$ASSET" -C "$WORK" qoop
    got="$("$WORK/qoop" --version 2>/dev/null)" || {
        echo "qoop install: the downloaded binary does not run" >&2
        exit 1
    }
    [ "$got" = "qoop $version" ] || {
        echo "qoop install: refusing: the download reports $got, want qoop $version" >&2
        exit 1
    }

    # Atomic into place, executable: staged beside the destination under a name of its own
    # (a rename needs one filesystem), never one a file of the person's could be sitting on.
    mkdir -p "$(dirname "$DEST")" || {
        echo "qoop install: cannot create $(dirname "$DEST")" >&2
        exit 1
    }
    TMPNEW="$(mktemp "$DEST.XXXXXX")" || {
        echo "qoop install: cannot write in $(dirname "$DEST")" >&2
        exit 1
    }
    cp "$WORK/qoop" "$TMPNEW"
    chmod 0755 "$TMPNEW"
    mv "$TMPNEW" "$DEST"
    TMPNEW=""
    echo "installed $("$DEST" --version) to $DEST"

    # A first install: no config.toml and no pet yet, read before the steps below, which may
    # write the config (the notch). A binary that says nothing (the install check) is not one.
    first=""
    if "$DEST" config 2>/dev/null | head -n 1 | grep -q "(no file yet" \
        && [ "$("$DEST" avatar show --json 2>/dev/null)" = "null" ]; then
        first=1
    fi

    # The rest is the CLI's own verbs, never a copy of their logic. Each one on its own: a
    # hooks directory the CLI refuses is no reason to leave the skill and the PATH hint out.
    "$DEST" hooks install || echo "qoop install: hooks were not installed; see the message above" >&2
    "$DEST" skills install || echo "qoop install: the skill was not installed; see the message above" >&2
    # On a Mac, the agents in the notch, started at login. Never the reason an install
    # fails: a Mac older than 14 says so, and everything above stays installed.
    if [ "$(uname -s)" = "Darwin" ]; then
        "$DEST" notch install || echo "the notch was not turned on; the rest of qoop is installed"
        # The icons' font, now: a terminal draws a glyph from a font that was there when it
        # first looked, and the interface draws icons only from one there when it started.
        "$DEST" icons install || echo "qoop install: the icon font was not installed; run: qoop icons install" >&2
    fi
    # A phone's bridge service, when there is one, back on the file just put in place: launchd
    # will not start an agent whose file was replaced, and a running one is the old build.
    "$DEST" bridge reload || echo "qoop install: the bridge service was not reloaded; run: qoop bridge reload" >&2

    # The hooks call `qoop` by name, so it has to be on PATH. Said, not done: an rc
    # file is the person's own. Said before the onboarding, so a ctrl-c in its sign-in, which
    # ends this script too, leaves it on the screen.
    cmd="qoop"
    if ! command -v qoop >/dev/null 2>&1; then
        bindir="$(dirname "$DEST")"
        case "${SHELL:-}" in
            */zsh) rc=.zshrc ;;
            */bash)
                # Terminal on a Mac starts a login shell, which reads .bash_profile and not .bashrc.
                if [ "$(uname -s)" = "Darwin" ]; then rc=.bash_profile; else rc=.bashrc; fi
                ;;
            */fish) rc="" ;;
            *) rc=.profile ;;
        esac
        if [ -z "$rc" ]; then
            echo "add $bindir to PATH, e.g. fish_add_path \"$bindir\""
        else
            echo "add $bindir to PATH, e.g. echo 'export PATH=\"$bindir:\$PATH\"' >> ~/$rc"
        fi
        cmd="$DEST"
    fi

    # On a first install, and only when there is a terminal to answer on, the onboarding
    # steps: the pet, the sounds, light or dark, the command key, notifications, sleep, then
    # signing in. A piped run (`curl | sh`) has no stdin to ask on but usually has the
    # terminal, so the answers are read from it; CI has neither and stays non-interactive by
    # construction. Quitting keeps the install just done.
    if [ -n "$first" ] && [ -z "${QOOP_NONINTERACTIVE:-}" ] && ( : </dev/tty ) 2>/dev/null; then
        "$DEST" onboard </dev/tty || true
    else
        echo "the pet, the sounds and every other setting: qoop settings"
        # The onboarding asks to sign in itself; without it, the one command to run next, by
        # full path while the name does not resolve yet.
        echo "next: $cmd login"
    fi
}

main "$@"

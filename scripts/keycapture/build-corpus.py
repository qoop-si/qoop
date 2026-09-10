"""Build the keyboard corpus from what each emulator publishes about itself.

Every row has a source. Two kinds exist:

  terminfo:<entry>   the emulator's own terminfo description, read on this machine with
                     `infocmp`. For Ghostty that is the entry shipped inside the app
                     bundle; for gnome-terminal it is VTE's `vte-256color`; iTerm2 ships
                     no entry of its own and sets TERM to `xterm-256color`, which is the
                     description it implements.
  ascii / xterm      control and meta encodings that no terminfo entry carries because
                     they are not per-terminal: a control letter is ANSI X3.4, and a meta
                     letter is the ESC prefix documented in xterm's ctlseqs.
  capture:<emulator> bytes recorded from a physical key press by drive.py.

Nothing here is written from memory. If a sequence cannot be traced to one of those, it
does not become a row.

The f-key numbering above kf12 and the digit suffix on an extended capability both follow
xterm's modifier parameter table: the parameter is 1 plus the sum of shift 1, alt 2,
control 4. That is the convention terminfo entries are written against, and it is what
makes kf25 mean control with F1 rather than a 25th physical key.

Usage: uv run --no-project python build-corpus.py <output.tsv>
"""

import pathlib
import re
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
GHOSTTY_TERMINFO = "/Applications/Ghostty.app/Contents/Resources/terminfo"

EMULATORS = [
    ("ghostty", "xterm-ghostty", GHOSTTY_TERMINFO),
    ("iterm2", "xterm-256color", None),
    ("gnome-terminal", "vte-256color", None),
]

# xterm's modifier parameter: 1 plus shift 1, alt 2, control 4.
MODIFIER_PARAM = {
    2: "shift",
    3: "alt",
    4: "shift+alt",
    5: "ctrl",
    6: "shift+ctrl",
    7: "ctrl+alt",
    8: "shift+ctrl+alt",
}

# Unmodified capabilities.
PLAIN = {
    "kcuu1": "Up",
    "kcud1": "Down",
    "kcuf1": "Right",
    "kcub1": "Left",
    "khome": "Home",
    "kend": "End",
    "kich1": "Insert",
    "kdch1": "Delete",
    "knp": "PageDown",
    "kpp": "PageUp",
    "kbs": "Backspace",
    "kent": "KeypadEnter",
}

# Capabilities that are shifted by definition, per terminfo(5).
SHIFTED = {
    "kUP": "Up",
    "kDN": "Down",
    "kRIT": "Right",
    "kLFT": "Left",
    "kHOM": "Home",
    "kEND": "End",
    "kIC": "Insert",
    "kDC": "Delete",
    "kNXT": "PageDown",
    "kPRV": "PageUp",
    "kcbt": "Tab",
}

# The extended capabilities carry the modifier as a digit suffix.
EXTENDED_BASE = {
    "kUP": "Up",
    "kDN": "Down",
    "kRIT": "Right",
    "kLFT": "Left",
    "kHOM": "Home",
    "kEND": "End",
    "kIC": "Insert",
    "kDC": "Delete",
    "kNXT": "PageDown",
    "kPRV": "PageUp",
}

# kf13 upward, in blocks of twelve, is the xterm convention.
FKEY_BLOCKS = ["shift", "ctrl", "shift+ctrl", "alt", "shift+alt", "ctrl+alt"]


def infocmp(entry: str, terminfo_dir: str | None) -> dict[str, str]:
    env = {"TERMINFO_DIRS": terminfo_dir} if terminfo_dir else {}
    out = subprocess.run(
        ["infocmp", "-x", "-1", entry],
        capture_output=True,
        text=True,
        env={**dict(__import__("os").environ), **env},
    )
    if out.returncode != 0:
        raise SystemExit(f"infocmp {entry} failed: {out.stderr.strip()}")
    caps: dict[str, str] = {}
    for line in out.stdout.splitlines():
        line = line.strip().rstrip(",")
        if "=" not in line or not line.startswith("k"):
            continue
        name, value = line.split("=", 1)
        caps[name] = value
    return caps


def unescape(value: str) -> bytes | None:
    """Turn a terminfo string capability into the bytes it stands for."""
    out = bytearray()
    i = 0
    while i < len(value):
        c = value[i]
        if c == "\\":
            i += 1
            nxt = value[i]
            if nxt == "E" or nxt == "e":
                out.append(0x1B)
            elif nxt == "n":
                out.append(0x0A)
            elif nxt == "r":
                out.append(0x0D)
            elif nxt == "t":
                out.append(0x09)
            elif nxt == "b":
                out.append(0x08)
            elif nxt == "s":
                out.append(0x20)
            elif nxt == "\\":
                out.append(0x5C)
            elif nxt.isdigit():
                digits = value[i : i + 3]
                out.append(int(digits, 8) & 0xFF)
                i += 2
            else:
                return None
        elif c == "^":
            i += 1
            out.append(ord(value[i].upper()) ^ 0x40)
        elif c == "%":
            # A parameterised capability is not a fixed key sequence.
            return None
        else:
            out.append(ord(c))
        i += 1
    return bytes(out)


def rows_for(emulator: str, entry: str, terminfo_dir: str | None) -> list[tuple[str, ...]]:
    caps = infocmp(entry, terminfo_dir)
    source = f"terminfo:{entry}"
    seen: set[tuple[str, str]] = set()
    rows: list[tuple[str, ...]] = []

    def add(name: str, code: str, mods: str, raw: bytes | None) -> None:
        if raw is None or not raw:
            return
        key = (raw.hex(), code)
        if key in seen:
            return
        seen.add(key)
        rows.append((emulator, name, raw.hex(), code, mods, "press", source))

    for cap, code in PLAIN.items():
        if cap not in caps:
            continue
        raw = unescape(caps[cap])
        if cap == "kbs" and raw == b"\x08":
            # xterm-256color says the Backspace key sends BS, which is also exactly what
            # control with H sends. One byte cannot carry both meanings, so asserting
            # either one would be asserting a preference rather than a fact. The row is
            # left out and the ASCII control-letter row keeps the byte.
            continue
        add(cap, code, "none", raw)
    for cap, code in SHIFTED.items():
        if cap in caps:
            add(cap, code, "shift", unescape(caps[cap]))
    for cap, value in caps.items():
        m = re.fullmatch(r"(k[A-Z]{2,3})([2-8])", cap)
        if m and m.group(1) in EXTENDED_BASE:
            add(cap, EXTENDED_BASE[m.group(1)], MODIFIER_PARAM[int(m.group(2))], unescape(value))
    for cap, value in caps.items():
        m = re.fullmatch(r"kf(\d+)", cap)
        if not m:
            continue
        n = int(m.group(1))
        if n <= 12:
            add(cap, f"F{n}", "none", unescape(value))
            continue
        block, index = divmod(n - 13, 12)
        if block < len(FKEY_BLOCKS):
            add(cap, f"F{index + 1}", FKEY_BLOCKS[block], unescape(value))
    return rows


def control_and_meta_rows() -> list[tuple[str, ...]]:
    """Encodings that are not per-terminal and therefore not in any terminfo entry.

    Three control codes are excluded from the control-letter block because the byte has a
    named key that owns it and no terminal can tell the two apart on the wire: 0x09 is Tab
    as much as it is control with I, 0x0d is Enter, and 0x1b is Escape. Emitting a
    control-letter expectation for those would demand that the decoder guess, and the
    corpus would be asserting something untrue about terminals rather than something true
    about us. They appear below as the named keys instead.
    """
    rows: list[tuple[str, ...]] = []
    ambiguous = {"i": "Tab", "m": "Enter", "[": "Escape"}
    for ch in "abcdefghjklnopqrstuvwxyz":
        rows.append(("any", f"ctrl-{ch}", bytes([ord(ch) & 0x1F]).hex(), ch, "ctrl", "press", "ascii"))
    for name, ch, code in [
        ("ctrl-space", "@", " "),
        ("ctrl-backslash", "\\", "\\"),
        ("ctrl-bracketright", "]", "]"),
        ("ctrl-caret", "^", "^"),
        ("ctrl-underscore", "_", "_"),
    ]:
        rows.append(("any", name, bytes([ord(ch) & 0x1F]).hex(), code, "ctrl", "press", "ascii"))
    for name, code, raw in [("tab", "Tab", b"\x09"), ("enter", "Enter", b"\x0d"), ("escape", "Escape", b"\x1b")]:
        rows.append(("any", name, raw.hex(), code, "none", "press", "ascii"))
    assert set(ambiguous) & set("abcdefghjklnopqrstuvwxyz") == set()
    for ch in "abcdefz.":
        rows.append(("any", f"alt-{ch}", bytes([0x1B, ord(ch)]).hex(), ch, "alt", "press", "xterm-ctlseqs"))

    # Cursor and edit keys with the cursor key mode off. terminfo's kcuu1 and friends are
    # written for application mode, because that is the mode a curses application puts the
    # terminal in, so a corpus built from terminfo alone would miss the encoding a shell
    # actually sees. A live Ghostty capture on this machine sent CSI A for Up, not SS3 A,
    # which is what forced this block to exist.
    for code, final in [("Up", "A"), ("Down", "B"), ("Right", "C"), ("Left", "D"), ("Home", "H"), ("End", "F")]:
        rows.append(("any", f"normal-{code.lower()}", bytes([0x1B, 0x5B, ord(final)]).hex(), code, "none", "press", "xterm-ctlseqs"))
    return rows


def captured_rows() -> list[tuple[str, ...]]:
    rows: list[tuple[str, ...]] = []
    for path in sorted((HERE / "captured").glob("*.tsv")) if (HERE / "captured").exists() else []:
        for line in path.read_text().splitlines()[1:]:
            if not line.strip():
                continue
            emulator, label, raw, code, mods, kind = line.split("\t")
            rows.append((emulator, label, raw, code, mods, kind, f"capture:{emulator}"))
    return rows


def main() -> int:
    out_path = pathlib.Path(sys.argv[1])
    rows: list[tuple[str, ...]] = []
    for emulator, entry, terminfo_dir in EMULATORS:
        got = rows_for(emulator, entry, terminfo_dir)
        print(f"{emulator}: {len(got)} rows from {entry}", file=sys.stderr)
        rows += got
    rows += control_and_meta_rows()
    captured = captured_rows()
    print(f"captured: {len(captured)} rows", file=sys.stderr)
    rows += captured

    with out_path.open("w", encoding="utf-8") as f:
        f.write("emulator\tlabel\traw_hex\texpected_code\texpected_mods\texpected_kind\tsource\n")
        for row in rows:
            f.write("\t".join(row) + "\n")
    print(f"wrote {len(rows)} rows to {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

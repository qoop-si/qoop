"""Drive a terminal emulator through the key list and record what it sends.

The corpus this produces is a capture, not a table transcribed from documentation. The
expected side of each row is the key that was physically pressed; the raw side is whatever
the emulator put on the pty. Nothing here decides what an emulator "should" send, which is
the point: that is exactly what the decoder has to be right about, so a hand-written
fixture would be begging the question.

Usage: uv run --no-project python drive.py <ghostty|iterm2> <output.tsv>
"""

import pathlib
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
SLOT = 0.45
SENTINEL_CHAR = "."
SENTINEL_HEX = "2e"
MOD_MAP = {"shift": "shift down", "ctrl": "control down", "alt": "option down"}
APP_NAME = {"ghostty": "Ghostty", "iterm2": "iTerm2"}
# What `System Events` calls the process, which is not always what it calls the
# application. Ghostty is "Ghostty" to `activate` and "ghostty" to a process query, and
# comparing against the wrong one silently makes the focus check always true.
PROC_NAME = {"ghostty": "ghostty", "iterm2": "iTerm2"}


def applescript_for(send: str) -> str:
    kind, value, mods = send.split(":", 2)
    parts = [MOD_MAP[m] for m in mods.split("+") if m != "none"]
    using = f" using {{{', '.join(parts)}}}" if parts else ""
    if kind == "code":
        return f"key code {value}{using}"
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'keystroke "{escaped}"{using}'


def launch(emulator: str, command: str) -> None:
    if emulator == "ghostty":
        # Start from no windows. Windows from earlier runs stay open, and `activate` gives
        # the focus to whichever of them is frontmost, which is not the one recording.
        subprocess.run(["osascript", "-e", 'tell application "Ghostty" to quit'], capture_output=True)
        time.sleep(2)
        subprocess.Popen(
            ["/Applications/Ghostty.app/Contents/MacOS/ghostty", "-e", "/bin/sh", "-c", command],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return
    if emulator == "iterm2":
        script = (
            'tell application "iTerm2"\n'
            f'  create window with default profile command "/bin/sh -c \\"{command}\\""\n'
            "end tell"
        )
        subprocess.run(["osascript", "-e", script], check=True, capture_output=True)
        return
    raise SystemExit(f"unknown emulator {emulator}")


def lines_of(path: pathlib.Path) -> list[str]:
    if not path.exists():
        return []
    return [l for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


def main() -> int:
    emulator, out_path = sys.argv[1], pathlib.Path(sys.argv[2])
    keys = [
        line.rstrip("\n").split("\t")
        for line in (HERE / "keys.tsv").read_text().splitlines()[1:]
        if line.strip()
    ]
    # A recorder left over from an earlier run writes to the same path and truncates it,
    # which looks exactly like "no keystroke arrived".
    subprocess.run(["pkill", "-f", "capture-keys.py"], capture_output=True)
    time.sleep(1)
    raw_path = HERE / f"raw-{emulator}.hex"
    ready_path = pathlib.Path(str(raw_path) + ".ready")
    raw_path.unlink(missing_ok=True)
    ready_path.unlink(missing_ok=True)
    seconds = len(keys) * (SLOT * 2 + 0.25) + 40

    launch(emulator, f"uv run --no-project python {HERE / 'capture-keys.py'} {raw_path} {seconds}")

    waited = 0.0
    while not ready_path.exists():
        time.sleep(0.2)
        waited += 0.2
        if waited > 40:
            print("the recorder never reached raw mode", file=sys.stderr)
            return 1

    activate = f'tell application "{APP_NAME[emulator]}" to activate'

    # Preflight. Two minutes of keystrokes into a window that is not listening produces a
    # file full of nothing and no clue why, so prove one keystroke lands first.
    subprocess.run(
        [
            "osascript",
            "-e",
            f'{activate}\ndelay 2\ntell application "System Events" to keystroke "{SENTINEL_CHAR}"',
        ],
        check=True,
    )
    for _ in range(25):
        if lines_of(raw_path):
            break
        time.sleep(0.2)
    if lines_of(raw_path) != [SENTINEL_HEX]:
        print(f"preflight failed: recorder saw {lines_of(raw_path)}", file=sys.stderr)
        return 1
    offset = 1

    script = [activate, "delay 0.5"]
    for _, send, *_ in keys:
        script.append(f'tell application "System Events" to {applescript_for(send)}')
        script.append(f"delay {SLOT}")
        # Some combinations belong to the window server and take the focus with them:
        # control with an arrow opens Mission Control, and every later key would go there.
        # Reactivating costs milliseconds and turns a run-ending failure into one gap.
        # Escape only when the focus actually left, because an unconditional Escape would
        # be a byte in every slot and would make every row a two-event slot.
        script.append(
            "tell application \"System Events\" to if name of first application process "
            f"whose frontmost is true is not \"{PROC_NAME[emulator]}\" then key code 53"
        )
        script.append("delay 0.35")
        script.append(activate)
        script.append("delay 0.25")
        script.append(f'tell application "System Events" to keystroke "{SENTINEL_CHAR}"')
        script.append(f"delay {SLOT}")
    subprocess.run(["osascript", "-e", "\n".join(script)], check=True)
    time.sleep(2)

    events = lines_of(raw_path)[offset:]
    slots: list[list[str]] = []
    current: list[str] = []
    for event in events:
        if event == SENTINEL_HEX:
            slots.append(current)
            current = []
        else:
            current.append(event)
    print(f"{len(keys)} keys sent, {len(slots)} slots closed", file=sys.stderr)
    if len(slots) != len(keys):
        print("MISMATCH: alignment is not trustworthy, refusing to write a corpus", file=sys.stderr)
        return 1

    written = 0
    unobserved: list[str] = []
    with out_path.open("w", encoding="utf-8") as f:
        f.write("emulator\tlabel\traw_hex\texpected_code\texpected_mods\texpected_kind\n")
        for (label, _, code, mods, kind), slot in zip(keys, slots):
            if len(slot) != 1:
                # Nothing arrived, or the key produced more than one burst. Either way the
                # row is not a clean observation and belongs in the report, not the corpus.
                unobserved.append(f"{label}({len(slot)})")
                continue
            f.write(f"{emulator}\t{label}\t{slot[0]}\t{code}\t{mods}\t{kind}\n")
            written += 1
    print(f"wrote {written} rows; not observed: {' '.join(unobserved) or 'none'}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

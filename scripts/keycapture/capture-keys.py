"""Record the raw bytes a terminal emulator sends for each key press.

Runs inside the emulator under test with its stdin in raw mode, and groups incoming bytes
into events by an idle gap. The gap, not a byte count, is what separates events: one key
press produces anywhere from one to eight bytes, and no length rule can tell "the rest of
this sequence" from "the next key" without one.

Events are written and flushed as they complete rather than at exit, so the driver can
confirm that a keystroke is actually reaching this process before it sends a hundred more.
"""

import os
import pathlib
import select
import sys
import termios
import time
import tty

IDLE_GAP = 0.12


def main() -> int:
    out_path, seconds = sys.argv[1], float(sys.argv[2])
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    out = open(out_path, "w", encoding="utf-8")
    tty.setraw(fd)
    pathlib.Path(out_path + ".ready").write_text("ready", encoding="utf-8")
    buf = b""
    last: float | None = None
    deadline = time.time() + seconds
    try:
        while time.time() < deadline:
            ready, _, _ = select.select([fd], [], [], 0.02)
            if ready:
                chunk = os.read(fd, 4096)
                if not chunk:
                    break
                buf += chunk
                last = time.time()
            elif buf and last is not None and time.time() - last > IDLE_GAP:
                out.write(buf.hex() + "\n")
                out.flush()
                buf = b""
                last = None
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        if buf:
            out.write(buf.hex() + "\n")
        out.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Gate G1.23: no `unsafe` outside qoop-pty without a SAFETY comment.

`unsafe` is not banned, it is accounted for. A block with no stated reason is one nobody can
review, and the crates that are supposed to have none say so in their own source with
`forbid(unsafe_code)`, which the compiler enforces. This checks the rest: every remaining
`unsafe` has a SAFETY comment on the lines immediately above it, or anywhere in the
comment block that ends directly above it (a long reason is still one reason).
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
LOOKBACK = 4


def main() -> int:
    problems: list[str] = []
    counted = 0
    for path in sorted(ROOT.glob("crates/*/src/**/*.rs")) + sorted(ROOT.glob("crates/*/tests/**/*.rs")):
        lines = path.read_text(encoding="utf-8").splitlines()
        for i, line in enumerate(lines):
            if not re.search(r"\bunsafe\b", line):
                continue
            # A declaration of intent, not a use of it.
            if "forbid(unsafe_code)" in line or line.strip().startswith("//"):
                continue
            counted += 1
            window = lines[max(0, i - LOOKBACK) : i]
            ok = any("SAFETY:" in w for w in window)
            j = i - 1
            while not ok and j >= 0 and lines[j].strip().startswith("//"):
                ok = "SAFETY:" in lines[j]
                j -= 1
            if not ok:
                rel = path.relative_to(ROOT)
                problems.append(f"{rel}:{i + 1}: {line.strip()}")

    if problems:
        print(f"FAIL  {len(problems)} unsafe uses with no SAFETY comment:")
        for p in problems:
            print(f"  {p}")
        return 1
    print(f"OK  {counted} unsafe uses, every one with a SAFETY comment")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

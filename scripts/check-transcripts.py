#!/usr/bin/env python3
"""Gate G0.4: the spike transcripts are generated, and they still say what they said.

The first version of this gate diffed the generated document byte for byte. Two separate
measurements showed that it can never pass: the transcripts embed pids, wall-clock
durations, throughput, byte counts and RSS figures, all of which change on every run. A
gate that is red on every green pull request gets deleted, and the hand-typed transcript
comes straight back, which is the exact failure this file exists to prevent.

So: normalise the volatile fields, diff the normalised form, and assert the structural
invariants separately. What is checked is what the transcript MEANS, which is the thing a
hand edit would change.
"""
import pathlib, re, subprocess, sys

VOLATILE = [
    (re.compile(r"\b\d+\.\d+(?:ms|s|µs|us|ns)\b"), "<duration>"),
    (re.compile(r"\b\d+(?:ms|s|µs|us|ns)\b"), "<duration>"),
    (re.compile(r"\bpid=\d+"), "pid=<pid>"),
    (re.compile(r"\bpgid=-?\d+"), "pgid=<pgid>"),
    (re.compile(r"\bsid=-?\d+"), "sid=<sid>"),
    (re.compile(r"Some\(\d+\)"), "Some(<n>)"),
    (re.compile(r"-\d{2,}\b"), "-<pgid>"),
    (re.compile(r"\bdev=\d+"), "dev=<dev>"),
    (re.compile(r"/dev/ttys\d+"), "/dev/tty<n>"),
    (re.compile(r"/dev/pts/\d+"), "/dev/pts/<n>"),
    (re.compile(r"\b\d+ MiB/s"), "<rate> MiB/s"),
    (re.compile(r"\b\d+ KiB per terminal"), "<n> KiB per terminal"),
    (re.compile(r"EAGAIN \d+"), "EAGAIN <n>"),
    (re.compile(r"\b\d+ bytes in\b"), "<n> bytes in"),
    (re.compile(r"settled after \d+ rounds / \d+ bytes"), "settled after <n> rounds / <n> bytes"),
    (re.compile(r"\bof CPU\b"), "of CPU"),
    (re.compile(r"\b\d+ members\b"), "<n> members"),
    (re.compile(r"\b\d+ distinct groups\b"), "<n> distinct groups"),
    (re.compile(r"drained \d+ prompt bytes"), "drained <n> prompt bytes"),
    (re.compile(r"history_size=\d+"), "history_size=<n>"),
    (re.compile(r"\b\d+ trailing bytes"), "<n> trailing bytes"),
]

# Structural invariants: the meaning of each transcript, independent of any number.
REQUIRED = {
    "spike-pty": [
        "[1] async read/write on PTY master: PASS",
        "[2] tcgetpgrp on master: PASS",
        "[3] resize observed by child (stty size): PASS",
        "[4] idle 3s with no output: PASS",
        "[5] child exit status propagates: PASS",
        "SPIKE-PTY: ALL PASS",
    ],
    "spike-term": [
        "[1] plain text into grid: PASS",
        "[2] SGR colour reaches cell state: PASS",
        "[3] bounded scrollback: PASS",
        "[4] damage tracking: PASS",
        "[5] resize to 132x40: PASS",
        "SPIKE-TERM: ALL PASS",
    ],
    "spike-proc": [
        "master=false, slave=false",
        "[1] fd inheritance:",
        "-> PASS",
        "[2] silent exec transition:",
        "[3] kqueue EVFILT_PROC on an unprivileged child: exec=true",
        "[4] OS-measured idle cost",
        "SPIKE-PROC: ALL PASS",
    ],
    "spike-teardown": [
        "CLAIM A CONFIRMED",
        "the controlling terminal is disassociated when the session leader dies",
        "PROPOSED FIX WORKS",
        "CLAIM B CONFIRMED",
        "CLAIM C CONFIRMED",
        "CLAIM D CONFIRMED",
        "SPIKE-TEARDOWN: COMPLETE",
    ],
    "spike-mem": [
        "one configuration per process",
        "never scrolled",
        "engine history disabled",
    ],
}


def normalise(text: str) -> str:
    for pat, repl in VOLATILE:
        text = pat.sub(repl, text)
    return text


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    fail = []
    doc = root / "docs" / "spike-results.md"

    # 0. Read and normalise the COMMITTED document before regenerating anything.
    #
    # The first version of this checker regenerated first and then normalised, so what it
    # examined was always the fresh output and never what a human had committed. A test
    # edited "CLAIM A CONFIRMED" to "CLAIM A REFUTED" in the committed file and the gate
    # still printed OK. Comparing before against after is what actually proves the committed
    # document came from a real run, and it is the property the byte-diff version had.
    if not doc.exists():
        print(f"FAIL {doc} does not exist")
        return 1
    committed = normalise(doc.read_text())

    # 1. Regenerate, so the committed document is proven to come from a real run.
    r = subprocess.run(
        ["./scripts/regen-spike-results.sh"],
        cwd=root,
        capture_output=True,
        text=True,
        env={**__import__("os").environ, "SKIP_BUILD": __import__("os").environ.get("SKIP_BUILD", "0")},
    )
    if r.returncode != 0:
        print("FAIL regen-spike-results.sh exited", r.returncode)
        print(r.stdout[-2000:], r.stderr[-2000:])
        return 1

    # 2. Structural invariants per transcript, plus a clean exit and no failures.
    for name, required in REQUIRED.items():
        path = root / "spikes" / "transcripts" / f"{name}.txt"
        if not path.exists():
            fail.append(f"{name}: transcript missing")
            continue
        text = path.read_text()
        if "exit=0" not in text:
            fail.append(f"{name}: did not exit 0")
        for line in text.splitlines():
            if line.startswith("FAILURE:"):
                fail.append(f"{name}: {line}")
        for needle in required:
            if needle not in text:
                fail.append(f"{name}: missing required line {needle!r}")

    # 3a. The committed document must match what a real run produces right now, once
    #     volatile fields are normalised. This is the load-bearing comparison.
    current = normalise(doc.read_text())
    if committed != current:
        import difflib

        diff = list(
            difflib.unified_diff(
                committed.splitlines(),
                current.splitlines(),
                "committed document (normalised)",
                "this run (normalised)",
                lineterm="",
                n=1,
            )
        )
        fail.append("the committed document does not match a real run:")
        fail.extend("  " + d for d in diff[:60])

    # 3b. A separate committed baseline catches meaning drift over time, for example a
    #     spike quietly weakened until it asserts less than it used to.
    baseline = root / "docs" / "spike-results.normalised.txt"
    if not baseline.exists():
        fail.append(
            f"{baseline.relative_to(root)} is missing. It is a committed artefact, not "
            "something this gate creates: a self-seeding baseline makes the gate green "
            "against any content. Regenerate it deliberately with "
            "QOOP_WRITE_BASELINE=1 uv run --no-project python scripts/check-transcripts.py"
        )
        if __import__("os").environ.get("QOOP_WRITE_BASELINE") == "1":
            baseline.write_text(current)
            print(f"wrote {baseline.relative_to(root)}; commit it")
            fail.pop()
    elif baseline.read_text() != current:
        import difflib

        diff = list(
            difflib.unified_diff(
                baseline.read_text().splitlines(),
                current.splitlines(),
                "committed (normalised)",
                "regenerated (normalised)",
                lineterm="",
                n=1,
            )
        )
        fail.append("normalised transcript differs from the committed baseline:")
        fail.extend("  " + d for d in diff[:60])

    if fail:
        for x in fail:
            print("FAIL " + x if not x.startswith("  ") else x)
        return 1
    print("OK  transcripts regenerate, exit clean, and match the committed baseline")
    return 0


if __name__ == "__main__":
    sys.exit(main())

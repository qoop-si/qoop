#!/usr/bin/env python3
"""Gate G0.1 and G0.2, executable.

G0.1  every ADR has Context, Decision, Consequences and Rollback sections
G0.2  every ADR that cites measured evidence names a spike file that exists and a
      Reproduce command; the command is run by CI, not by this script

These were previously asserted as "met" in the plan without anyone running them, and a
review found two ADRs missing a Consequences section and none carrying a runnable command.
"""
import os, pathlib, re, sys

adr_dir = pathlib.Path("docs/adr")
required = ["## Context", "## Decision", "## Consequences", "## Rollback"]
fail = []

files = sorted(adr_dir.glob("ADR-*.md"))
if not files:
    print("no ADRs found", file=sys.stderr)
    sys.exit(1)

# Numbers must be unique, and every number the plan cites must either exist as a file or
# be marked "not written" in the plan's ADR table. A dense-range check was rejected: the
# reserved numbers will not be written in numeric order, so it would fail the build for a
# reason that is not a defect, and the cheapest way out would be a placeholder ADR.
nums = [int(f.name[4:7]) for f in files]
dupes = {n for n in nums if nums.count(n) > 1}
if dupes:
    fail.append(f"duplicate ADR numbers: {sorted(dupes)}")

plan = pathlib.Path("plans/00_MASTER_PLAN.md")
if plan.exists():
    ptext = plan.read_text()
    for m in re.finditer(r"^\| (\d{3}) \| ([^|]+)\| ([^|]+)\|", ptext, re.M):
        num, _subject, status = int(m.group(1)), m.group(2), m.group(3).strip()
        exists = num in nums
        declared_unwritten = status.startswith("not written")
        if exists and declared_unwritten:
            fail.append(f"ADR-{num:03d} exists but the plan says it is not written")
        if not exists and not declared_unwritten:
            fail.append(
                f"ADR-{num:03d} is cited by the plan with status {status!r} but no file exists"
            )

for f in files:
    s = f.read_text()
    for sect in required:
        if sect not in s:
            fail.append(f"{f.name}: missing section {sect!r}")
    if "Evidence:" in s:
        m = re.search(r"^Reproduce: `([^`]+)`", s, re.M)
        if not m:
            fail.append(f"{f.name}: cites Evidence but names no Reproduce command")
        else:
            # The command must exist and be executable, not merely be a string.
            cmd = m.group(1).split()[0].lstrip("./")
            cmd_path = pathlib.Path(cmd)
            if not cmd_path.exists():
                fail.append(f"{f.name}: Reproduce command {cmd} does not exist")
            elif not os.access(cmd_path, os.X_OK):
                fail.append(f"{f.name}: Reproduce command {cmd} is not executable")
        for m in re.finditer(r"`(spikes/[^`]+)`", s):
            for part in re.findall(r"src/\{([^}]+)\}", m.group(1)):
                for name in part.split(","):
                    p = pathlib.Path("spikes/spike-runtime/src") / f"{name.strip()}.rs"
                    if not p.exists():
                        fail.append(f"{f.name}: cites missing spike file {p}")
            if "{" not in m.group(1):
                p = pathlib.Path(m.group(1))
                if not p.exists():
                    fail.append(f"{f.name}: cites missing spike file {p}")

# I13: the product must not claim isolation it does not enforce.
forbidden = [
    ("sandboxed from each other", "agents are not OS-isolated from each other"),
    ("agents cannot access", "same-uid agents can access anything the user can"),
    ("securely isolated", "same-uid isolation is cooperative, not enforced"),
]
# Superseded decisions that keep coming back through a document that was edited in
# isolation. Two review rounds caught the same CBOR cascade, so it is now mechanical.
# Each entry is (forbidden phrase, what replaced it). A document that needs to discuss the
# history says so in the allowed form on the right, which is why the check is on the exact
# stale phrasing rather than on the word.
superseded = [
    ("CBOR hello", "the handshake is JSON, see ADR-004"),
    ("payload is always CBOR", "the handshake is JSON, see ADR-004"),
    ("in-memory ring is bounded by **bytes**", "the byte budget was withdrawn, ADR-003"),
    ("release the PTY master", "teardown drains the master and closes it last, ADR-014"),
    ("re-enumerating before each escalation", "the containment set is a monotonic union"),
]
# A retraction is allowed to quote what it retracts, which is most of the value of these
# documents, so a line that carries a retraction marker is not a violation.
RETRACTION = (
    "was wrong",
    "is wrong",
    "earlier",
    "superseded",
    "withdrawn",
    "previous",
    "no longer",
    "retract",
)
for doc in list(pathlib.Path("docs").rglob("*.md")) + list(pathlib.Path("plans").glob("*.md")):
    for n, line in enumerate(doc.read_text().splitlines(), 1):
        for phrase, replacement in superseded:
            if phrase in line and not any(m in line for m in RETRACTION):
                fail.append(
                    f"{doc}:{n}: superseded decision {phrase!r} reappeared; {replacement}"
                )

# I13 covers product claims, which live in a README and in marketing copy as much as in an
# ADR, so the roots are wider than docs alone. `reviews/` is deliberately excluded: a review
# quotes the forbidden phrases in order to argue about them, and a checker that reddens on
# that punishes the exact discipline it exists to support.
claim_docs = []
for r in ["docs", "plans", "README.md"]:
    rp = pathlib.Path(r)
    if rp.is_file():
        claim_docs.append(rp)
    elif rp.is_dir():
        claim_docs.extend(rp.rglob("*.md"))
for doc in claim_docs:
    text = doc.read_text().lower()
    for phrase, why in forbidden:
        if phrase in text:
            fail.append(f"{doc}: forbidden isolation claim {phrase!r} ({why})")

if fail:
    for x in fail:
        print("FAIL " + x)
    sys.exit(1)
print(f"OK  {len(files)} ADRs, structure and evidence references valid")

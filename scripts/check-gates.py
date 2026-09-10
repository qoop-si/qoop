#!/usr/bin/env python3
"""Every gate the plan or an ADR references must be defined in the plan.

Round 3 renumbered the M2 gates and a naive remap cascaded, leaving several documents
pointing at a gate that tested something else. Nothing caught it because nothing checked.
This does.
"""
import glob, pathlib, re, sys

plan = pathlib.Path("plans/00_MASTER_PLAN.md").read_text()
defined = set(re.findall(r"^(G\d+\.\d+)\s", plan, re.M))
if not defined:
    print("FAIL no gate definitions found; has the plan format changed?")
    sys.exit(1)

fail = []
for name, text in [("plans/00_MASTER_PLAN.md", plan)] + [
    (f, pathlib.Path(f).read_text()) for f in sorted(glob.glob("docs/adr/*.md"))
]:
    for g in sorted(set(re.findall(r"\b(G\d+\.\d+)\b", text))):
        if g not in defined:
            fail.append(f"{name}: references {g}, which is not defined in the plan")

# Gate numbering inside a milestone must be dense and in document order, so a reader can
# find a gate by scanning and a renumber cannot silently leave a hole.
by_milestone = {}
for m in re.finditer(r"^G(\d+)\.(\d+)\s", plan, re.M):
    by_milestone.setdefault(int(m.group(1)), []).append(int(m.group(2)))
for ms, nums in sorted(by_milestone.items()):
    if nums != list(range(1, len(nums) + 1)):
        fail.append(f"milestone {ms} gates are not dense and in order: {nums}")

if fail:
    for f in fail:
        print("FAIL " + f)
    sys.exit(1)
print(f"OK  {len(defined)} gates defined, every reference resolves, numbering is dense")

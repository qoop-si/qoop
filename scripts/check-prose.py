#!/usr/bin/env python3
"""No em dash, and no double hyphen or lone hyphen used as a stylistic dash.

A hyphen inside a compound word or a CLI flag is fine. A hyphen standing in for a dash is
not. This runs over documents we ship, not over vendored reference material.
"""
import pathlib, re, sys

# Written as escapes so this checker does not trip over itself.
EM = "\u2014"
EN = "\u2013"

# `reviews/` is excluded on purpose: a review quotes a violation in order to argue about
# it, and a checker that reddens on a quoted counterexample punishes the discipline it
# exists to support. Reviews are an internal record, not a shipped artifact.
# `crates` is in the list because most of this project's prose is in doc comments, and a
# rule that is enforced over the documentation and not over the code is enforced over the
# smaller half. `skills` and `hooks` are copied into people's own agent directories, so
# they ship as surely as the binary does.
roots = ["crates", "docs", "plans", "scripts", "skills", "hooks", "site", "README.md", ".github", ".qoop"]
skip = ("ref/", "target/", "node_modules/")

fail = []
files = []
for r in roots:
    p = pathlib.Path(r)
    if p.is_file():
        files.append(p)
    elif p.is_dir():
        files.extend(x for x in p.rglob("*") if x.suffix in {".md", ".py", ".sh", ".yml", ".rs", ".js", ".ts", ".tsx"})

for f in files:
    if any(s in str(f) for s in skip):
        continue
    # Fence state, tracked across lines. Checking only whether a line starts with a fence
    # marker leaves every line inside a block looking like prose, and a shell command in a
    # block legitimately contains a double hyphen.
    in_fence = False
    for n, line in enumerate(f.read_text(errors="ignore").splitlines(), 1):
        if f.suffix == ".md" and line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if EM in line or EN in line:
            fail.append(f"{f}:{n}: em or en dash: {line.strip()[:90]}")
        # A CLI invocation legitimately contains " -- ", so strip inline code spans and
        # skip code blocks and tables before looking for a dash used as punctuation.
        # A Rust doc comment is prose and is checked; the code around it is not. Everywhere
        # else a comment marker means a shell or python line, where a double hyphen is a
        # flag rather than a dash.
        is_rust_prose = f.suffix in {".rs", ".ts", ".tsx"} and line.lstrip().startswith(("///", "//!", "//"))
        is_code = not is_rust_prose and (
            in_fence
            or f.suffix != ".md"
            or line.lstrip().startswith(("#", "//", "$", "|"))
        )
        prose = re.sub(r"`[^`]*`", "", line)
        if not is_code and re.search(r"\S\s+--\s+\S", prose):
            fail.append(f"{f}:{n}: double hyphen as a dash: {line.strip()[:90]}")
        # A lone hyphen between clauses, which the docstring bans and an earlier version
        # of this script did not actually check. A hyphenated compound has no spaces
        # around it, and a markdown list item starts the line, so neither trips this.
        if not is_code and re.search(r"[a-z,;)\"]\s+-\s+[a-z(\"]", prose):
            fail.append(f"{f}:{n}: lone hyphen as a dash: {line.strip()[:90]}")

if fail:
    for x in fail:
        print("FAIL " + x)
    sys.exit(1)
print(f"OK  {len(files)} files, no dash violations")

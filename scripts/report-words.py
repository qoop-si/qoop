"""Write crates/qoop-domain/src/report-words.txt, the word list a report note is checked with.

Source: SCOWL's graded data (https://github.com/en-wl/wordlist, data/scowl-pre.txt). Kept: lower
case words (and their inflections) at SCOWL size 50 or less. Dropped: a word SCOWL also has as a
name, when it is less common (size 40 or 50: `john`, `google`, `amazon`), or, among the most
common (size 35), when the name is a person's (`mark`, `frank`), plus the countries in `DROP`
(`turkey`); never one of the function words (`will`, `may`) or the few trade words in `KEEP`.
Single letters but `a` and `i` go too: they would spell a name out.

usage: uv run python scripts/report-words.py <scowl-pre.txt>
"""

import re
import sys
from pathlib import Path

MAX_SIZE = 50
NAME_TAGS = ("person", "surname", "name", "place", "trademark", "demonym")
# Among the most common words, only a person's name takes a word away (a place tag there also
# covers `great`, `new`, `head`), and these countries.
COMMON_NAME_TAGS = ("n/person",)
DROP = {"china", "guinea", "jersey", "turkey"}
COMMON = 35
NAME_SOURCES = ("[name]", "[census]")
# Function words a name never takes away (the common ones from /usr/share/dict/connectives).
KEEP_WORDS = (
    "a about above after again against all am an and any are as at be because been before being"
    " below between both but by can could did do does doing down during each few for from further"
    " had has have having he her here hers herself him himself his how i if in into is it its"
    " itself just may me might more most must my myself no nor not now of off on once only or other"
    " our ours ourselves out over own same shall she should so some such than that the their"
    " theirs them themselves then there these they this those through to too under until up very"
    " was we were what when where which while who whom why will with would you your yours"
    " yourself yourselves"
    # Words a note about software needs that SCOWL also has as a name.
    " boring chance early friendly likely major master means mobile normal outlook overly"
    " printer protection reader reading reform register sale united"
    " art chase grant hope rich"
)
KEEP = set(KEEP_WORDS.split())
NOTICE = """\
# Made by scripts/report-words.py from SCOWL (https://github.com/en-wl/wordlist): sizes up to
# 50, lower case, names dropped. Its notice:
#
# Copyright 2000-2026 by Kevin Atkinson
#
# Permission to use, copy, modify, distribute, and sell any part of the English
# Speller Database (ESDB, previously known as SCOWLv2), or word lists
# created from it, is hereby granted without fee, provided that the above
# copyright notice appears in all copies and that both the above copyright
# notice and this notice appear in supporting documentation.  Kevin Atkinson
# makes no representations about the suitability of this database for any
# purpose.  It is provided "as is" without express or implied warranty.
"""
WORD = re.compile(r"^[a-z][a-z']*$")


def main(source: Path, out: Path) -> None:
    words: dict[str, int] = {}
    names: dict[str, set[str]] = {}
    lemma = ""
    for line in source.read_text(encoding="utf-8").splitlines():
        if ": " not in line:
            continue
        meta, rest = line.split(": ", 1)
        size = re.match(r"\s*(\d+)", meta)
        head, _, inflections = rest.partition(": ")
        parts = head.split(" <", 1)
        if parts[0] != "-":
            lemma = parts[0].strip()
        tag = parts[1] if len(parts) > 1 else ""
        forms = [lemma] + [
            t.rstrip("-")
            for t in re.split(
                r"[\s,()|]+", re.sub(r"\{[^}]*\}|@:\s*\S+", "", inflections)
            )
        ]
        if lemma[:1].isupper() and (
            any(t in tag for t in NAME_TAGS) or any(s in meta for s in NAME_SOURCES)
        ):
            for f in forms:
                if f:
                    names.setdefault(f.lower(), set()).add(tag.rstrip(">"))
            continue
        if size and int(size.group(1)) <= MAX_SIZE:
            for f in filter(WORD.match, forms):
                words[f] = min(words.get(f, MAX_SIZE), int(size.group(1)))

    def name(word: str) -> bool:
        tags = names.get(word, set())
        if word in DROP:
            return True
        if word in KEEP or not tags:
            return False
        return words[word] > COMMON or any(t in COMMON_NAME_TAGS for t in tags)

    # A single letter spells a name out letter by letter; only the words `a` and `i` stay.
    for letter in [w for w in words if len(w) == 1 and w not in "ai"]:
        del words[letter]
    dropped = sorted(filter(name, words))
    kept = sorted(set(words) - set(dropped))
    out.write_text(NOTICE + "\n".join(kept) + "\n", encoding="utf-8")
    print(
        f"{len(kept)} words, {len(dropped)} dropped as names, {out.stat().st_size} bytes"
    )


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    root = Path(__file__).resolve().parent.parent
    main(Path(sys.argv[1]), root / "crates/qoop-domain/src/report-words.txt")

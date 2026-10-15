# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools==4.66.1"]
# ///
"""Build crates/qoop-tui/assets/QoopSymbols.ttf, the few icons the interface draws.

Run with `uv run scripts/build-icon-font.py`. It downloads the pinned Nerd Fonts symbols
release, checks its hash and keeps only the glyphs named in GLYPHS. The symbol sets were drawn
apart and the Nerd mono build stretches each to the full cell, so a moon stands taller than a
cup: every glyph is fitted here into one box about the height of a capital, on one centre
line, scaled by an optical factor for the tall shapes, one cell wide. They are put at our own
codepoints in the supplementary private use plane, never at the Nerd ones: Terminal falls
back to an installed font only for those, and a Nerd font a person has installed can then
never draw its own glyph in place of ours. It prints the table `crates/qoop-tui/src/icons.rs`
holds; a test there checks the two agree.
"""

import hashlib
import io
import math
import pathlib
import sys
import urllib.request
import zipfile

from fontTools import subset
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.transformPen import TransformPen
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont

VERSION = "v3.5.1"
URL = f"https://github.com/ryanoasis/nerd-fonts/releases/download/{VERSION}/NerdFontsSymbolsOnly.zip"
SHA256 = "fdca3682534f6f65e1ccb2345b0362ccf67d9b8eca7c8025330946e93e2473bc"
MEMBER = "SymbolsNerdFontMono-Regular.ttf"
FAMILY = "Qoop Symbols"
OUT = pathlib.Path(__file__).resolve().parent.parent / "crates/qoop-tui/assets/QoopSymbols.ttf"

# The Nerd glyph names, in the order icons.rs lists them, and each one's optical factor:
# a crescent or a cog fills its box more than a cup does and reads larger at the same size.
GLYPHS = [
    ("md-sleep", 1.00),
    ("cod-coffee", 1.00),
    ("cod-remote", 0.92),
    ("md-cog_outline", 0.94),
    ("fa-moon_o", 0.86),
    ("md-power_sleep", 0.86),
    ("md-dots_horizontal", 0.80),
    ("md-chevron_right", 0.72),
    ("md-eye", 1.00),
    ("md-eye_off", 1.00),
    ("md-bell_outline", 0.94),
    ("md-bell_off_outline", 0.94),
]
# Glyphs the Nerd sets lack, drawn here over the outline of the one named: the shut eye is a
# lid and three lashes (every Nerd "closed" eye is a struck-through one).
DRAWN = {"md-eye_off": "shut_eye"}
# The first of our codepoints: plane 16, supplementary private use area B.
FIRST = 0x10C0A0
# In font units (2048 to the em), set against Menlo: its advance is 1233, a capital 1493
# high, a lowercase x 1120. Every glyph fits a box this wide and high, centred on MID.
ADVANCE, BOX_W, BOX_H, MID = 1233, 1210, 1440, 700


def main() -> int:
    data = urllib.request.urlopen(URL).read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != SHA256:
        print(f"hash mismatch: {digest}", file=sys.stderr)
        return 1
    font = TTFont(io.BytesIO(zipfile.ZipFile(io.BytesIO(data)).read(MEMBER)))
    by_name = {name: cp for cp, name in font.getBestCmap().items()}
    missing = [name for name, _ in GLYPHS if name not in by_name]
    if missing:
        print(f"not in {MEMBER}: {missing}", file=sys.stderr)
        return 1
    options = subset.Options()
    options.name_IDs = ["*"]
    options.notdef_outline = True
    sub = subset.Subsetter(options)
    sub.populate(unicodes=[by_name[name] for name, _ in GLYPHS])
    sub.subset(font)
    cmap = font.getBestCmap()
    for name, how in DRAWN.items():
        pen = TTGlyphPen(None)
        globals()[how](pen)
        font["glyf"][cmap[by_name[name]]] = pen.glyph()
    fit(font, by_name)
    for record in font["name"].names:
        if record.nameID in (1, 4, 16):
            record.string = FAMILY
        elif record.nameID == 6:
            record.string = FAMILY.replace(" ", "")
        elif record.nameID == 3:
            record.string = f"{FAMILY} {VERSION}"
    # The source font's own timestamp, so two builds give the same bytes.
    font.recalcTimestamp = False
    OUT.parent.mkdir(parents=True, exist_ok=True)
    font.save(OUT)
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")
    for i, (name, _) in enumerate(GLYPHS):
        print(f"    ({name!r}, '\\u{{{FIRST + i:x}}}'),")
    return 0


def shut_eye(pen) -> None:
    """A shut eye on a 24 unit grid, as wide as the open one: the lid, a curve hanging down
    between the corners, and three lashes under it. Every contour runs clockwise."""

    def point(x: float, y: float) -> tuple[int, int]:
        return round(x * 85), round(y * 85)

    curve = [(1 + 22 * t / 24, 13 - 5 * math.sin(math.pi * t / 24)) for t in range(25)]
    top = [point(x, y + 1) for x, y in curve]
    shape(pen, top + [point(x, y - 1) for x, y in reversed(curve)])
    for t, lean in ((0.22, -1.6), (0.5, 0.0), (0.78, 1.6)):
        x, y = 1 + 22 * t, 13 - 5 * math.sin(math.pi * t) - 0.5
        foot = y - 3.6
        shape(pen, [point(x - 0.8, y), point(x + 0.8, y), point(x + 0.8 + lean, foot),
                    point(x - 0.8 + lean, foot)])


def shape(pen, points) -> None:
    """One closed polygon, turned clockwise as TrueType fills it."""
    area = sum(x0 * y1 - x1 * y0 for (x0, y0), (x1, y1) in zip(points, points[1:] + points[:1]))
    if area > 0:
        points = points[::-1]
    pen.moveTo(points[0])
    for p in points[1:]:
        pen.lineTo(p)
    pen.closePath()


def fit(font: TTFont, by_name: dict[str, int]) -> None:
    """Fit every glyph to the one box and move it to our codepoints, in place."""
    cmap = font.getBestCmap()
    glyphs = font.getGlyphSet()
    glyf, hmtx = font["glyf"], font["hmtx"]
    ours = {}
    for i, (name, factor) in enumerate(GLYPHS):
        gname = cmap[by_name[name]]
        bounds = BoundsPen(glyphs)
        glyphs[gname].draw(bounds)
        x0, y0, x1, y1 = bounds.bounds
        k = min(BOX_W / (x1 - x0), BOX_H / (y1 - y0)) * factor
        dx = ADVANCE / 2 - (x0 + x1) / 2 * k
        dy = MID - (y0 + y1) / 2 * k
        pen = TTGlyphPen(None)
        glyphs[gname].draw(TransformPen(pen, (k, 0, 0, k, dx, dy)))
        glyf[gname] = pen.glyph()
        glyf[gname].recalcBounds(glyf)
        hmtx[gname] = (ADVANCE, glyf[gname].xMin)
        ours[FIRST + i] = gname
    hmtx[".notdef"] = (ADVANCE, 0)
    font["hhea"].advanceWidthMax = ADVANCE
    font["OS/2"].xAvgCharWidth = ADVANCE
    for table in font["cmap"].tables:
        table.cmap = dict(ours) if table.format == 12 else {}


if __name__ == "__main__":
    sys.exit(main())

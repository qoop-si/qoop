"""Draw the qoro dot logo as SVG: the square q mark and the qoro word, dark, light and bare."""
from pathlib import Path

GROUND, CREAM, EMBER = "#0F0F11", "#FCF7F1", "#FD4B03"
OUT = Path(__file__).parent

# Three by three letters; the q's tail is one more dot under its bottom right corner.
RING = ["xxx", "x.x", "xxx"]
R = ["xx.", "x..", "x.."]
TAIL = (2, 3)


def dots(letter_rows, ox, oy, step, ink, accent=None):
    out = []
    for y, row in enumerate(letter_rows):
        for x, d in enumerate(row):
            if d == "x":
                fill = accent if accent and (x, y) == (0, 0) else ink
                out.append((ox + x * step, oy + y * step, fill))
    return out


def svg(w, h, bg, circles, r, rx=0):
    body = "".join(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r:.1f}" fill="{f}"/>' for x, y, f in circles)
    ground = f'<rect width="{w}" height="{h}" rx="{rx}" fill="{bg}"/>' if bg else ""
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">{ground}{body}</svg>\n'


def mark(size, bg, ink, rx):
    # The nine sit dead centre; the tail hangs below them, outside the centring.
    step, r = size * 0.176, size * 0.0625
    ox = size / 2 - step
    oy = size / 2 - step
    circles = dots(RING, ox, oy, step, ink)
    circles.append((ox + TAIL[0] * step, oy + TAIL[1] * step, EMBER))
    return svg(size, size, bg, circles, r, rx)


def word(h, bg, ink):
    # Letters on a pitch of four dots (three and a gap); the band of three rows is centred
    # vertically, the q's tail drops below it.
    step, r = h * 0.195, h * 0.07
    margin = h * 0.42
    w = 2 * margin + 14 * step
    oy = h / 2 - step
    circles = []
    for i, letter in enumerate(["q", "o", "r", "o"]):
        ox = margin + i * 4 * step
        if letter == "r":
            circles += dots(R, ox, oy, step, ink, accent=EMBER)
        else:
            circles += dots(RING, ox, oy, step, ink)
        if letter == "q":
            circles.append((ox + TAIL[0] * step, oy + TAIL[1] * step, EMBER))
    return svg(round(w), h, bg, circles, r, 0)


files = {
    "q-mark-dark.svg": mark(1024, GROUND, CREAM, 224),
    "q-mark-light.svg": mark(1024, CREAM, GROUND, 224),
    "q-mark-bare-on-dark.svg": mark(1024, None, CREAM, 0),
    "q-mark-bare-on-light.svg": mark(1024, None, GROUND, 0),
    "qoro-word-dark.svg": word(320, GROUND, CREAM),
    "qoro-word-light.svg": word(320, CREAM, GROUND),
    "qoro-word-bare-on-dark.svg": word(320, None, CREAM),
    "qoro-word-bare-on-light.svg": word(320, None, GROUND),
}
for name, text in files.items():
    (OUT / name).write_text(text)
print("\n".join(files))


# --- The phone's icon and splash: the same q mark, rasterised (mobile/assets, Pillow). ---

MOBILE_ASSETS = OUT.parent.parent / "mobile" / "assets"


def _rgba(h, alpha=255):
    h = h.lstrip("#")
    return (int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16), alpha)


def raster(size, bg, circles, r, supersample=4):
    """The dots, antialiased: drawn at `supersample` times the size, then downsampled."""
    from PIL import Image, ImageDraw

    big = size * supersample
    img = Image.new("RGBA", (big, big), _rgba(bg) if bg else (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    for x, y, fill in circles:
        cx, cy, rr = x * supersample, y * supersample, r * supersample
        draw.ellipse([cx - rr, cy - rr, cx + rr, cy + rr], fill=_rgba(fill))
    return img.resize((size, size), Image.LANCZOS)


def png_mark(size, bg, ink, safe=1.0):
    """The q mark as a raster square. `safe` shrinks it to that fraction of the canvas, for
    Android's adaptive icon safe zone (the foreground and monochrome layers only show their
    inner 66%)."""
    inner = size * safe
    step, r = inner * 0.176, inner * 0.0625
    ox = size / 2 - step
    oy = size / 2 - step
    circles = dots(RING, ox, oy, step, ink)
    circles.append((ox + TAIL[0] * step, oy + TAIL[1] * step, EMBER))
    return raster(size, bg, circles, r)


png_files = {
    # The app icon: the full tile, ground behind the mark, its usual size.
    "icon.png": png_mark(1024, GROUND, CREAM),
    # Android's adaptive icon: a plain ground layer behind a transparent mark layer, the
    # mark kept inside the 66% safe zone the system does not crop.
    "android-icon-background.png": raster(512, GROUND, [], 1),
    "android-icon-foreground.png": png_mark(512, None, CREAM, safe=0.66),
    # The themed (monochrome) layer: the same shape, the system supplies its own colour.
    "android-icon-monochrome.png": png_mark(432, None, CREAM, safe=0.66),
    "favicon.png": png_mark(48, GROUND, CREAM),
    # The splash screen's own image: bare, transparent, laid over its background colour by
    # the expo-splash-screen plugin (app.json).
    "splash-icon.png": png_mark(1024, None, CREAM),
}
for name, img in png_files.items():
    img.save(MOBILE_ASSETS / name)
print("\n".join(png_files))

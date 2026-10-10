"""Draw Quiekel the pig.

Quiekel is a front-facing pig head cut into slices by parallel diagonal cuts with small gaps,
each slice a flat colour: the head (ears included) in four pinks, with a hot-pink snout set into
it with a gap. The snout has two slot nostrils, like a power socket, and a smile; the eyes are
dots and the ears are round. The icons get a white sticker edge, so they read on any wallpaper.

One geometry feeds both outputs:
  static/pig.svg                                      the logo in the app (vector)
  assets/icon.ico, assets/icon.png, static/icon.png   icons, rendered here with Pillow
"""

import math
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

PKG = Path(__file__).resolve().parents[1] / "src" / "quiekel_embed"

GAP = 5  # between pieces
OUTLINE = 7  # Windows icons: a white sticker edge, so the gaps don't show the wallpaper ...
KEYLINE = 2.5  # ... and a thin dark line around it, so it also reads on light backgrounds
APP_EDGE = 3.5  # the logo in the app: a thin edge in the theme's ink (dark on light, white on dark)
SLOPE = 0.36  # how far the cuts lean: x moves right by this much per unit downwards
CUT_Y = 84  # CUTS are measured at this height (the eyes)
CUTS = [40, 100, 160]
COLOURS = ["#ffd3df", "#ffb3c8", "#ff85a8", "#f2547f"]  # left to right
SNOUT = "#f2547f"
INK = "#151515"
WHITE = "#ffffff"


def fillet(points, radii, steps=12):
    """A polygon with rounded corners (radius per vertex), as a list of points."""
    out, n = [], len(points)
    for i, (px, py) in enumerate(points):
        r = radii[i]
        (ax, ay), (bx, by) = points[i - 1], points[(i + 1) % n]
        ux, uy = ax - px, ay - py
        vx, vy = bx - px, by - py
        lu, lv = math.hypot(ux, uy), math.hypot(vx, vy)
        ux, uy, vx, vy = ux / lu, uy / lu, vx / lv, vy / lv
        theta = math.acos(max(-1.0, min(1.0, ux * vx + uy * vy)))
        if r <= 0 or theta < 1e-3 or abs(theta - math.pi) < 1e-3:
            out.append((px, py))
            continue
        d = min(r / math.tan(theta / 2), lu / 2, lv / 2)
        r = d * math.tan(theta / 2)
        t1, t2 = (px + ux * d, py + uy * d), (px + vx * d, py + vy * d)
        bx_, by_ = ux + vx, uy + vy
        lb = math.hypot(bx_, by_)
        h = r / math.sin(theta / 2)
        cx, cy = px + bx_ / lb * h, py + by_ / lb * h
        a1, a2 = math.atan2(t1[1] - cy, t1[0] - cx), math.atan2(t2[1] - cy, t2[0] - cx)
        delta = (a2 - a1 + math.pi) % (2 * math.pi) - math.pi  # the short way round
        out += [(cx + r * math.cos(a1 + delta * k / steps), cy + r * math.sin(a1 + delta * k / steps))
                for k in range(steps + 1)]
    return out


def rrect(x0, y0, x1, y1, r):
    return fillet([(x0, y0), (x1, y0), (x1, y1), (x0, y1)], [r] * 4)


def curve(p0, c, p1, n=16):
    """A quadratic Bézier as points."""
    return [((1 - t) ** 2 * p0[0] + 2 * (1 - t) * t * c[0] + t * t * p1[0],
             (1 - t) ** 2 * p0[1] + 2 * (1 - t) * t * c[1] + t * t * p1[1]) for t in (k / n for k in range(n + 1))]


# ---- the pig ----------------------------------------------------------------------------

HEAD = [
    fillet([(46, 54), (154, 54), (184, 112), (168, 172), (32, 172), (16, 112)], [30, 30, 42, 40, 40, 42]),  # face
    fillet([(26, 88), (22, 16), (88, 46)], [12, 30, 12]),  # left ear, round
    fillet([(174, 88), (178, 16), (112, 46)], [12, 30, 12]),  # right ear
]
SNOUT_SHAPE = rrect(56, 100, 144, 158, 29)
NOSTRILS = [rrect(80, 110, 89, 132, 4.5), rrect(111, 110, 120, 132, 4.5)]  # slots, like a power socket
EYES = [(64, 84), (136, 84)]
EYE_R = 7.5
SMILE = curve((85, 140), (100, 151), (115, 140))  # on the snout, under the nostrils
SMILE_W = 4.5


def bands():
    """The slices as parallelograms (gap included), left to right."""
    edges = [-1000] + CUTS + [1000]
    y0, y1 = -20, 200
    out = []
    for i in range(len(COLOURS)):
        a = edges[i] + (GAP / 2 if i else 0)
        b = edges[i + 1] - (GAP / 2 if i < len(COLOURS) - 1 else 0)
        out.append([(a + SLOPE * (y0 - CUT_Y), y0), (b + SLOPE * (y0 - CUT_Y), y0),
                    (b + SLOPE * (y1 - CUT_Y), y1), (a + SLOPE * (y1 - CUT_Y), y1)])
    return out


def bounds(margin=4):
    pts = [p for poly in HEAD for p in poly] + SNOUT_SHAPE
    xs, ys = [x for x, _ in pts], [y for _, y in pts]
    return min(xs) - margin, min(ys) - margin, max(xs) + margin, max(ys) + margin


# ---- SVG ----------------------------------------------------------------------------------


def d_attr(poly, close=True):
    return "M " + " L ".join(f"{x:.1f} {y:.1f}" for x, y in poly) + (" Z" if close else "")


def svg(rings=()) -> str:
    """rings: (colour, width) edges around the head, outermost first. They also fill the gaps."""
    x0, y0, x1, y1 = bounds(margin=max((u for _, u in rings), default=2) + 2)
    box = f'x="{x0:.0f}" y="{y0:.0f}" width="{x1 - x0:.0f}" height="{y1 - y0:.0f}"'
    head = "".join(f'<path d="{d_attr(p)}"/>' for p in HEAD)
    defs = (f'<clipPath id="head">{head}</clipPath>'
            f'<mask id="snout-gap" maskUnits="userSpaceOnUse" {box}><rect {box} fill="white"/>'
            f'<path d="{d_attr(SNOUT_SHAPE)}" fill="black" stroke="black" stroke-width="{GAP * 2}" '
            'stroke-linejoin="round"/></mask>')
    slices = "".join(f'<path d="{d_attr(band)}" fill="{colour}"/>' for band, colour in zip(bands(), COLOURS))
    edges = [
        f'<path d="{d_attr(p)}" fill="{colour}" stroke="{colour}" stroke-width="{units * 2}" stroke-linejoin="round"/>'
        for colour, units in rings for p in HEAD + [SNOUT_SHAPE]
    ]
    parts = [
        f"<defs>{defs}</defs>",
        *edges,
        f'<g clip-path="url(#head)" mask="url(#snout-gap)">{slices}</g>',
        f'<path d="{d_attr(SNOUT_SHAPE)}" fill="{SNOUT}"/>',
        *(f'<path d="{d_attr(n)}" fill="{INK}"/>' for n in NOSTRILS),
        *(f'<circle cx="{x}" cy="{y}" r="{EYE_R}" fill="{INK}"/>' for x, y in EYES),
        f'<path d="{d_attr(SMILE, False)}" fill="none" stroke="{INK}" stroke-width="{SMILE_W}" stroke-linecap="round"/>',
    ]
    body = "\n  ".join(parts)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{x0:.0f} {y0:.0f} {x1 - x0:.0f} {y1 - y0:.0f}" '
            f'role="img" aria-label="Quiekel">\n  {body}\n</svg>\n')


# ---- Pillow ---------------------------------------------------------------------------------


def render(width: int, supersample: int = 4, rings=()) -> Image.Image:
    """rings: (colour, width) edges around the head, outermost first. They also fill the gaps."""
    x0, y0, x1, y1 = bounds(margin=max((u for _, u in rings), default=2) + 2)
    k = width * supersample / (x1 - x0)
    size = (round((x1 - x0) * k), round((y1 - y0) * k))
    tr = lambda pts: [((x - x0) * k, (y - y0) * k) for x, y in pts]  # noqa: E731

    def mask(polys):
        m = Image.new("L", size, 0)
        d = ImageDraw.Draw(m)
        for p in polys:
            d.polygon(tr(p), fill=255)
        return m

    def outlined(poly, units):
        """A polygon grown by `units` (round joins): the snout plus the gap around it."""
        m = mask([poly])
        d = ImageDraw.Draw(m)
        w = units * 2 * k
        pts = tr(poly)
        d.line(pts + pts[:1], fill=255, width=round(w), joint="curve")
        for x, y in pts:
            d.ellipse((x - w / 2, y - w / 2, x + w / 2, y + w / 2), fill=255)
        return m

    head = mask(HEAD)
    hole = outlined(SNOUT_SHAPE, GAP)
    img = Image.new("RGBA", size, (0, 0, 0, 0))
    for colour, units in rings:  # underneath and around everything: the gaps take this colour too
        edge = Image.new("L", size, 0)
        for poly in HEAD + [SNOUT_SHAPE]:
            edge = ImageChops.lighter(edge, outlined(poly, units))
        layer = Image.new("RGBA", size, colour)
        layer.putalpha(edge)
        img = Image.alpha_composite(img, layer)
    for band, colour in zip(bands(), COLOURS):
        layer = Image.new("RGBA", size, colour)
        layer.putalpha(ImageChops.subtract(ImageChops.multiply(head, mask([band])), hole))
        img = Image.alpha_composite(img, layer)

    d = ImageDraw.Draw(img)
    d.polygon(tr(SNOUT_SHAPE), fill=SNOUT)
    for nostril in NOSTRILS:
        d.polygon(tr(nostril), fill=INK)
    r = EYE_R * k
    for (cx, cy) in tr(EYES):
        d.ellipse((cx - r, cy - r, cx + r, cy + r), fill=INK)
    pts, w = tr(SMILE), SMILE_W * k
    d.line(pts, fill=INK, width=round(w), joint="curve")
    for x, y in (pts[0], pts[-1]):
        d.ellipse((x - w / 2, y - w / 2, x + w / 2, y + w / 2), fill=INK)
    return img.resize((size[0] // supersample, size[1] // supersample), Image.LANCZOS)


def square(img: Image.Image, size: int, fill: float = 0.98) -> Image.Image:
    """The head centred on a transparent square, as Windows icons need."""
    w, h = img.size
    s = size * fill / max(w, h)
    small = img.resize((max(1, round(w * s)), max(1, round(h * s))), Image.LANCZOS)
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    out.paste(small, ((size - small.width) // 2, (size - small.height) // 2), small)
    return out


def main():
    static, assets = PKG / "static", PKG / "assets"
    (static / "pig.svg").write_text(svg([(INK, APP_EDGE)]), encoding="utf-8")  # light theme
    (static / "pig-dark.svg").write_text(svg([(WHITE, APP_EDGE)]), encoding="utf-8")
    # Windows icons sit on any wallpaper or taskbar, light or dark: white edge plus a dark line.
    big = render(1024, rings=[(INK, OUTLINE + KEYLINE), (WHITE, OUTLINE)])
    square(big, 256).save(assets / "icon.png")
    square(big, 64).save(static / "icon.png")
    sizes = [16, 24, 32, 48, 64, 128, 256]
    frames = [square(big, s) for s in sizes]
    frames[-1].save(assets / "icon.ico", sizes=[(s, s) for s in sizes], append_images=frames[:-1])
    print("wrote pig.svg, icon.png, icon.ico")


if __name__ == "__main__":
    main()

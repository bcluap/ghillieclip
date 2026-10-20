"""Render the GhillieClip icon and splash artwork into assets/.

Run from the repository root:  python3 tools/make_art.py
Needs Pillow. Output is deterministic (fixed random seed).
"""
import math
import random
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ASSETS = Path(__file__).resolve().parent.parent / "assets"
FONT = ASSETS / "DejaVuSans-Bold.ttf"

FOREST_DARK = (14, 26, 16)
FOREST = (30, 52, 30)
LEAF = [(52, 78, 40), (74, 98, 50), (96, 112, 60), (120, 116, 72), (62, 70, 42), (40, 62, 36)]
AMBER = (255, 168, 38)
AMBER_HOT = (255, 226, 140)
RETICLE = (232, 236, 220)


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))


def radial_bg(w, h, inner, outer, cx=0.5, cy=0.45):
    img = Image.new("RGB", (w, h))
    px = img.load()
    rmax = math.hypot(max(cx, 1 - cx) * w, max(cy, 1 - cy) * h)
    for y in range(h):
        for x in range(w):
            t = min(1.0, math.hypot(x - cx * w, y - cy * h) / rmax)
            px[x, y] = lerp(inner, outer, t ** 1.4)
    return img


def leaf(draw, x, y, length, width, angle, colour):
    """A ragged ghillie strand: a tapered blade with a jagged edge."""
    pts_l, pts_r = [], []
    steps = 9
    for i in range(steps + 1):
        t = i / steps
        w = width * math.sin(math.pi * min(1, t * 1.15)) * (1 - 0.6 * t)
        jag = (random.random() - 0.5) * width * 0.5
        cx = x + math.cos(angle) * length * t
        cy = y + math.sin(angle) * length * t
        nx, ny = -math.sin(angle), math.cos(angle)
        pts_l.append((cx + nx * (w + jag), cy + ny * (w + jag)))
        pts_r.append((cx - nx * (w - jag), cy - ny * (w - jag)))
    draw.polygon(pts_l + pts_r[::-1], fill=colour)


def ghillie_layer(w, h, count, size, mask_fn, seed):
    random.seed(seed)
    layer = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    for _ in range(count):
        x, y = random.uniform(0, w), random.uniform(0, h)
        if not mask_fn(x, y):
            continue
        c = random.choice(LEAF)
        a = random.uniform(0, 2 * math.pi)
        s = size * random.uniform(0.5, 1.3)
        leaf(d, x, y, s, s * 0.16, a, c + (random.randint(150, 235),))
    return layer


def reticle(d, cx, cy, r, stroke, colour, gap=0.28):
    d.ellipse([cx - r, cy - r, cx + r, cy + r], outline=colour, width=stroke)
    for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1)):
        x0, y0 = cx + dx * r * gap, cy + dy * r * gap
        x1, y1 = cx + dx * r * 1.18, cy + dy * r * 1.18
        d.line([x0, y0, x1, y1], fill=colour, width=stroke)


def bb_trail(img, start, end, radius, bulge):
    """Glowing amber BB at `end` with a curved fading tracer from `start`."""
    glow = Image.new("RGBA", img.size, (0, 0, 0, 0))
    g = ImageDraw.Draw(glow)
    n = 60
    (sx, sy), (ex, ey) = start, end
    mx, my = (sx + ex) / 2, (sy + ey) / 2 - bulge
    for i in range(n):
        t = i / (n - 1)
        x = (1 - t) ** 2 * sx + 2 * (1 - t) * t * mx + t * t * ex
        y = (1 - t) ** 2 * sy + 2 * (1 - t) * t * my + t * t * ey
        r = radius * (0.25 + 0.75 * t)
        g.ellipse([x - r, y - r, x + r, y + r], fill=AMBER + (int(60 + 195 * t * t),))
    halo = glow.filter(ImageFilter.GaussianBlur(radius * 1.2))
    img.alpha_composite(halo)
    img.alpha_composite(glow)
    core = Image.new("RGBA", img.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(core)
    d.ellipse([ex - radius * 2.4, ey - radius * 2.4, ex + radius * 2.4, ey + radius * 2.4], fill=AMBER + (255,))
    img.alpha_composite(core.filter(ImageFilter.GaussianBlur(radius * 1.1)))
    core = Image.new("RGBA", img.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(core)
    d.ellipse([ex - radius, ey - radius, ex + radius, ey + radius], fill=AMBER + (255,))
    d.ellipse([ex - radius * 0.55, ey - radius * 0.65, ex + radius * 0.25, ey + radius * 0.15], fill=AMBER_HOT + (255,))
    img.alpha_composite(core)


def icon(size=1024):
    s = size
    bg = radial_bg(s, s, FOREST, FOREST_DARK).convert("RGBA")
    # Ghillie fringe denser towards the edges, leaving the reticle readable.
    edge = lambda x, y: math.hypot(x - s / 2, y - s / 2) > s * 0.30 or random.random() < 0.12
    bg.alpha_composite(ghillie_layer(s, s, 900, s * 0.16, edge, 7))
    bg = bg.filter(ImageFilter.GaussianBlur(s * 0.002))
    shade = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(shade).ellipse([s * 0.2, s * 0.2, s * 0.8, s * 0.8], fill=(8, 16, 8, 150))
    bg.alpha_composite(shade.filter(ImageFilter.GaussianBlur(s * 0.06)))

    d = ImageDraw.Draw(bg)
    reticle(d, s * 0.5, s * 0.5, s * 0.27, int(s * 0.035), RETICLE + (255,))
    bb_trail(bg, (s * 0.10, s * 0.92), (s * 0.5, s * 0.5), s * 0.05, s * 0.12)

    # Rounded-square mask (macOS / Windows tile shape).
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, s - 1, s - 1], radius=int(s * 0.22), fill=255)
    out = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    out.paste(bg, (0, 0), mask)
    return out


def splash(w=1200, h=675):
    bg = radial_bg(w // 2, h // 2, (34, 58, 34), FOREST_DARK, 0.68, 0.5).resize((w, h), Image.BICUBIC).convert("RGBA")
    bg.alpha_composite(ghillie_layer(w, h, 1400, 110, lambda x, y: x < w * 0.45 or y > h * 0.86 or y < h * 0.08 or random.random() < 0.18, 11))
    # Darken the text area so the wordmark stays crisp.
    veil = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    ImageDraw.Draw(veil).rounded_rectangle([w * 0.40, h * 0.22, w * 0.97, h * 0.80], radius=40, fill=(6, 12, 6, 170))
    bg.alpha_composite(veil.filter(ImageFilter.GaussianBlur(30)))

    ic = icon(512).resize((300, 300), Image.LANCZOS)
    shadow = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle([100, 202, 400, 502], radius=66, fill=(0, 0, 0, 170))
    bg.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(18)))
    bg.alpha_composite(ic, (90, 180))

    d = ImageDraw.Draw(bg)
    title = ImageFont.truetype(str(FONT), 92)
    tag = ImageFont.truetype(str(FONT), 30)
    x = 500
    d.text((x, 230), "Ghillie", font=title, fill=(226, 232, 212))
    gw = d.textlength("Ghillie", font=title)
    d.text((x + gw, 230), "Clip", font=title, fill=AMBER)
    d.text((x + 4, 348), "Airsoft shot detection · BB tracking", font=tag, fill=(170, 186, 150))
    d.text((x + 4, 390), "Highlight reels from your gun cam", font=tag, fill=(170, 186, 150))
    d.line([x + 4, 450, x + 640, 450], fill=(90, 110, 70), width=2)
    return bg.convert("RGB")


def main():
    big = icon(1024)
    big.save(ASSETS / "icon-1024.png")
    big.resize((256, 256), Image.LANCZOS).save(ASSETS / "icon-256.png")
    big.save(ASSETS / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    splash().save(ASSETS / "splash.png", optimize=True)
    print("wrote", *(p.name for p in sorted(ASSETS.glob("icon*")) + [ASSETS / "splash.png"]))


if __name__ == "__main__":
    main()

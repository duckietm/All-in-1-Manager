"""Draws the program's icon (crates/app/resources/app.ico and crates/app/assets/icon.png): a small isometric hotel with
a red sign on a sky-blue tile with the dark outline and hard shadow of the Hotel look. Drawn at 1024 px and scaled
down, so every size is sharp. Needs Pillow.

    python source/tools/make_icon.py
"""

import os

from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
S = 1024

OUTLINE = (15, 45, 63, 255)
SKY_TOP = (101, 161, 215, 255)
SKY_BOTTOM = (120, 176, 224, 255)
TOP = (246, 237, 207, 255)
LEFT = (230, 215, 172, 255)
RIGHT = (203, 184, 132, 255)
GLASS = (142, 195, 234, 255)
LIT = (255, 210, 90, 255)
SIGN = (217, 58, 38, 255)
SIGN_TEXT = (255, 226, 122, 255)
CLOUD = (255, 255, 255, 255)


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(4))


def tile(size):
    """The rounded sky tile with its outline and the hard shadow under it."""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    margin = size * 0.06
    radius = size * 0.2
    edge = size * 0.035
    shadow = size * 0.04
    box = [margin, margin, size - margin, size - margin - shadow]
    d.rounded_rectangle([box[0], box[1] + shadow, box[2], box[3] + shadow], radius, fill=(15, 45, 63, 110))
    # The sky: a vertical gradient inside the rounded shape.
    sky = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    sd = ImageDraw.Draw(sky)
    for y in range(int(box[1]), int(box[3]) + 1):
        t = (y - box[1]) / max(1, box[3] - box[1])
        sd.line([(box[0], y), (box[2], y)], fill=lerp(SKY_TOP, SKY_BOTTOM, t))
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle(box, radius, fill=255)
    img.paste(sky, (0, 0), mask)
    d.rounded_rectangle(box, radius, outline=OUTLINE, width=int(edge))
    return img, box


def cloud(d, x, y, scale):
    e = 14 * scale
    for cx, cy, r in [(0, 0, 40), (45, -18, 52), (95, 0, 38)]:
        d.ellipse([x + (cx - r) * scale - e, y + (cy - r) * scale - e, x + (cx + r) * scale + e, y + (cy + r) * scale + e], fill=OUTLINE)
    d.rounded_rectangle([x - 40 * scale - e, y - 5 * scale - e, x + 133 * scale + e, y + 40 * scale + e], 25 * scale, fill=OUTLINE)
    for cx, cy, r in [(0, 0, 40), (45, -18, 52), (95, 0, 38)]:
        d.ellipse([x + (cx - r) * scale, y + (cy - r) * scale, x + (cx + r) * scale, y + (cy + r) * scale], fill=CLOUD)
    d.rounded_rectangle([x - 40 * scale, y - 5 * scale, x + 133 * scale, y + 40 * scale], 25 * scale, fill=CLOUD)


def hotel(d, front, width, depth, height, floors, edge):
    """An isometric block: the corner nearest to the viewer at `front`, with rows of windows on both faces."""
    rx, ry = 0.866, -0.5
    lx, ly = -0.866, -0.5

    def p(base, ax, ay, n):
        return (base[0] + ax * n, base[1] + ay * n)

    b = front
    br = p(b, rx, ry, width)
    bl = p(b, lx, ly, depth)
    up = lambda q: (q[0], q[1] - height)
    d.polygon([b, bl, up(bl), up(b)], fill=LEFT, outline=OUTLINE, width=edge)
    d.polygon([b, br, up(br), up(b)], fill=RIGHT, outline=OUTLINE, width=edge)
    blr = p(br, lx, ly, depth)
    d.polygon([up(b), up(br), up(blr), up(bl)], fill=TOP, outline=OUTLINE, width=edge)
    floor = height / (floors + 0.6)
    for ax, ay, length in [(rx, ry, width), (lx, ly, depth)]:
        columns = 3
        step = (length - length * 0.16) / columns
        for row in range(floors):
            for column in range(columns):
                start = p(b, ax, ay, length * 0.1 + column * step)
                start = (start[0], start[1] - floor * 0.45 - row * floor)
                w = step * 0.62
                h = floor * 0.55
                end = p(start, ax, ay, w)
                lit = (row * 3 + column * 2 + (ax > 0)) % 4 == 0
                d.polygon([start, end, (end[0], end[1] - h), (start[0], start[1] - h)], fill=LIT if lit else GLASS, outline=OUTLINE, width=max(1, edge // 2))
    return up(b), up(br), up(bl), up(blr)


def draw():
    img, box = tile(S)
    d = ImageDraw.Draw(img)
    cloud(d, box[0] + S * 0.15, box[1] + S * 0.24, 0.9)
    edge = int(S * 0.012)
    front = (S * 0.52, box[3] - S * 0.085)
    top_front, top_right, top_left, top_back = hotel(d, front, S * 0.29, S * 0.26, S * 0.33, 3, edge)
    # The sign on the roof: two posts and a red board with HOTEL.
    cx = (top_front[0] + top_back[0]) / 2
    cy = (top_front[1] + top_back[1]) / 2 - S * 0.06
    w, h = S * 0.34, S * 0.11
    for dx in (-0.25, 0.25):
        d.line([(cx + dx * w, cy + h / 2), (cx + dx * w, cy + h / 2 + S * 0.05)], fill=OUTLINE, width=edge)
    d.rounded_rectangle([cx - w / 2, cy - h / 2, cx + w / 2, cy + h / 2], S * 0.02, fill=SIGN, outline=OUTLINE, width=edge)
    font_path = os.path.join(ROOT, "crates", "app", "assets", "fonts", "PixelifySans.ttf")
    font = ImageFont.truetype(font_path, int(h * 0.7))
    d.text((cx, cy + h * 0.02), "HOTEL", font=font, fill=SIGN_TEXT, anchor="mm")
    return img


def main():
    big = draw()
    sizes = [16, 24, 32, 48, 64, 128, 256]
    images = [big.resize((n, n), Image.LANCZOS) for n in sizes]
    ico = os.path.join(ROOT, "crates", "app", "resources", "app.ico")
    images[-1].save(ico, format="ICO", sizes=[(n, n) for n in sizes], append_images=images[:-1])
    png = os.path.join(ROOT, "crates", "app", "assets", "icon.png")
    big.resize((512, 512), Image.LANCZOS).save(png)
    print("written", ico, png)


if __name__ == "__main__":
    main()

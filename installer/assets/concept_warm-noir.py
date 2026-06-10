# -*- coding: utf-8 -*-
"""
concept_warm-noir.py — VoxFlow installer WELCOME-page mock, concept [warm-noir].

DARK PREMIUM / editorial-noir direction. NO acid double-neon.
Single restrained accent: warm amber/bronze (#C28840), dosed sparingly.
Warm black field (#0C0A09), soft vignette, generous air, hairline rules,
refined light typography, a warm cursor-accent that echoes the app brand
("speech bubble built from text lines + blinking cursor").

This file only renders a PREVIEW MOCK of the Welcome page (~600x400) so the
art direction can be judged. The production BMP/ICO generator is separate.

Pillow only. Output: %TEMP%/concept_warm-noir.png
"""

import os
import math
import tempfile
from PIL import Image, ImageDraw, ImageFont, ImageFilter

# ----------------------------------------------------------------- palette ---
# Warm noir: everything leans a few points warm (more R than B) so the black
# reads like aged paper under low light, not cold tech-grey.
BG_TOP     = (0x12, 0x0F, 0x0D)   # slightly lifted warm charcoal (top)
BG_BOT     = (0x0C, 0x0A, 0x09)   # #0C0A09 deep warm black (base)
PANEL      = (0x17, 0x13, 0x10)   # inset warm panel
HAIRLINE   = (0x2A, 0x23, 0x1C)   # warm hairline rule
TEXT       = (0xEDE9 >> 8, 0xE4, 0xDC)  # warm off-white  ~#EDE4DC
TEXT_HI    = (0xF4, 0xEE, 0xE6)   # brighter warm white for wordmark
SECONDARY  = (0x9A, 0x8F, 0x82)   # warm grey for body
MUTED      = (0x6B, 0x62, 0x59)   # dim warm grey for kicker/footnotes
AMBER      = (0xC2, 0x88, 0x40)   # #C28840 single restrained accent
AMBER_SOFT = (0x8C, 0x63, 0x33)   # darker amber for fills/lines
AMBER_DIM  = (0x5A, 0x42, 0x26)   # very dim amber for ambient glow

SS = 3  # supersample factor

FONT_LIGHT = r"C:\Windows\Fonts\segoeuil.ttf"    # Segoe UI Light
FONT_SLIGHT = r"C:\Windows\Fonts\segoeuisl.ttf"  # Segoe UI Semilight
FONT_SEMIB = r"C:\Windows\Fonts\seguisb.ttf"     # Segoe UI Semibold
FONT_REG   = r"C:\Windows\Fonts\segoeui.ttf"     # Segoe UI


def font(path, size):
    return ImageFont.truetype(path, size)


def lerp(a, b, t):
    return tuple(int(round(a[i] + (b[i] - a[i]) * t)) for i in range(3))


def vgrad(w, h, top, bottom):
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        t = y / max(1, h - 1)
        # ease so most of the frame sits near the deep base, lift only near top
        te = t ** 1.35
        c = lerp(top, bottom, te)
        row = bytes(c) * w
        # faster row fill
        for x in range(w):
            px[x, y] = c
    return img


def radial_glow(w, h, center, color, radius, intensity=1.0):
    layer = Image.new("L", (w, h), 0)
    d = ImageDraw.Draw(layer)
    cx, cy = center
    d.ellipse([cx - radius, cy - radius, cx + radius, cy + radius], fill=255)
    layer = layer.filter(ImageFilter.GaussianBlur(radius * 0.55))
    if intensity != 1.0:
        layer = layer.point(lambda v: int(v * intensity))
    solid = Image.new("RGBA", (w, h), color + (255,))
    out = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    out.paste(solid, (0, 0), layer)
    return out


def vignette(w, h, strength=0.55):
    """Soft dark vignette: darker at the corners/edges, transparent center."""
    layer = Image.new("L", (w, h), 0)
    d = ImageDraw.Draw(layer)
    # bright (transparent) center ellipse, then invert -> dark edges
    pad_x = int(w * 0.02)
    pad_y = int(h * 0.02)
    d.ellipse([pad_x, pad_y, w - pad_x, h - pad_y], fill=255)
    layer = layer.filter(ImageFilter.GaussianBlur(int(min(w, h) * 0.22)))
    # invert so edges are dark
    inv = layer.point(lambda v: int((255 - v) * strength))
    out = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    out.paste(Image.new("RGBA", (w, h), (0, 0, 0, 255)), (0, 0), inv)
    return out


def add_grain(img, amount=7):
    """Subtle warm film grain (very low amount)."""
    import random
    px = img.load()
    w, h = img.size
    random.seed(7)
    for y in range(h):
        for x in range(w):
            n = random.randint(-amount, amount)
            r, g, b = px[x, y]
            # warm grain: nudge r/g a hair more than b
            px[x, y] = (
                max(0, min(255, r + n)),
                max(0, min(255, g + int(n * 0.9))),
                max(0, min(255, b + int(n * 0.7))),
            )
    return img


# ----------------------------------------------------- brand mark (bubble) ---
def draw_brand_mark(canvas, cx, cy, scale):
    """
    Editorial speech-bubble mark in warm-noir tone:
    a thin warm-grey rounded bubble outline, three text lines (warm white),
    and ONE warm-amber blinking cursor (the single accent).
    Drawn directly onto an RGBA `canvas` at supersampled scale.
    """
    d = ImageDraw.Draw(canvas)
    # bubble body box around (cx, cy)
    bw = int(150 * scale)
    bh = int(108 * scale)
    x0 = cx - bw // 2
    y0 = cy - bh // 2
    x1 = cx + bw // 2
    y1 = cy + bh // 2
    radius = int(28 * scale)
    stroke = max(2, int(3.4 * scale))
    outline_col = lerp(SECONDARY, BG_BOT, 0.18)  # a touch brighter, still warm

    # thin warm outline (no fill -> keeps it airy)
    d.rounded_rectangle([x0, y0, x1, y1], radius=radius,
                        outline=outline_col + (255,),
                        width=stroke)

    # tail (small wedge, lower-left) — drawn as two strokes meeting the body
    tx_top0 = x0 + int(bw * 0.20)
    tx_top1 = x0 + int(bw * 0.40)
    tip = (x0 + int(bw * 0.14), y1 + int(28 * scale))
    d.line([(tx_top0, y1 - stroke // 2), tip], fill=outline_col + (255,),
           width=stroke)
    d.line([tip, (tx_top1, y1 - stroke // 2)], fill=outline_col + (255,),
           width=stroke)

    # three text lines inside (warm off-white, varying widths)
    pad = int(20 * scale)
    line_h = max(2, int(7 * scale))
    gap = int(18 * scale)
    ly = y0 + int(24 * scale)
    widths = [0.62, 0.78, 0.40]
    for i, frac in enumerate(widths):
        lx0 = x0 + pad
        lx1 = x0 + pad + int((bw - 2 * pad) * frac)
        col = lerp(TEXT, BG_BOT, 0.15) if i < 2 else lerp(SECONDARY, BG_BOT, 0.1)
        d.rounded_rectangle([lx0, ly, lx1, ly + line_h],
                            radius=line_h // 2, fill=col + (255,))
        # amber cursor at end of the LAST line (the one accent in the mark)
        if i == 2:
            cur_x = lx1 + int(10 * scale)
            cur_w = max(2, int(6 * scale))
            cur_h = int(20 * scale)
            d.rounded_rectangle(
                [cur_x, ly - int(7 * scale), cur_x + cur_w, ly - int(7 * scale) + cur_h],
                radius=max(1, cur_w // 2), fill=AMBER + (255,))
        ly += gap


# ----------------------------------------------------------------- compose ---
def build_welcome(W=600, H=400):
    w, h = W * SS, H * SS

    # 1) warm vertical gradient base
    base = vgrad(w, h, BG_TOP, BG_BOT).convert("RGBA")

    # 2) one soft warm amber ambient glow, low + offset (upper-left), very dim.
    glow = radial_glow(w, h, (int(w * 0.30), int(h * 0.30)),
                       AMBER_DIM, int(w * 0.42), intensity=0.30)
    base = Image.alpha_composite(base, glow)
    # a second, even dimmer warm pool lower-right for depth balance
    glow2 = radial_glow(w, h, (int(w * 0.82), int(h * 0.86)),
                        (0x3A, 0x2A, 0x18), int(w * 0.40), intensity=0.22)
    base = Image.alpha_composite(base, glow2)

    # 3) layout zones
    #    Left rail: brand mark + thin vertical hairline (sidebar feel).
    #    Right body: kicker, wordmark, tagline.
    rail_w = int(w * 0.40)

    # brand mark in left rail, upper area
    mark_canvas = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    draw_brand_mark(mark_canvas, int(rail_w * 0.52), int(h * 0.40), SS)
    base = Image.alpha_composite(base, mark_canvas)

    # vertical hairline separating rail from body
    line_layer = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    ld = ImageDraw.Draw(line_layer)
    lx = rail_w
    ld.line([(lx, int(h * 0.16)), (lx, int(h * 0.84))],
            fill=HAIRLINE + (255,), width=max(1, SS))
    base = Image.alpha_composite(base, line_layer)

    # 4) typography in the body (right of rail)
    txt = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    td = ImageDraw.Draw(txt)

    bx = rail_w + int(w * 0.055)   # body left margin

    # kicker (letterspaced, muted, uppercase) — premium editorial signature
    kicker = "L O C A L   D I C T A T I O N"
    kf = font(FONT_SEMIB, int(11 * SS))
    ky = int(h * 0.255)
    td.text((bx, ky), kicker, font=kf, fill=MUTED + (255,))
    # tiny amber tick before kicker
    td.rectangle([bx - int(16 * SS), ky + int(2 * SS),
                  bx - int(16 * SS) + int(7 * SS), ky + int(13 * SS)],
                 fill=AMBER + (255,))

    # wordmark "VoxFlow" — large, light weight, warm white
    wf = font(FONT_LIGHT, int(64 * SS))
    wy = ky + int(26 * SS)
    # draw "Vox" in warm white, "Flow" in slightly dimmer — subtle two-tone,
    # no second hue.
    word_a = "Vox"
    word_b = "Flow"
    aw = td.textbbox((0, 0), word_a, font=wf)
    td.text((bx, wy), word_a, font=wf, fill=TEXT_HI + (255,))
    td.text((bx + (aw[2] - aw[0]), wy), word_b, font=wf,
            fill=lerp(TEXT, BG_BOT, 0.20) + (255,))

    # thin amber underline accent under the wordmark (short, restrained)
    wb = td.textbbox((bx, wy), word_a + word_b, font=wf)
    uy = wb[3] + int(14 * SS)
    td.rectangle([bx, uy, bx + int(54 * SS), uy + max(2, int(2.5 * SS))],
                 fill=AMBER + (255,))

    # tagline / body copy (Semilight, warm secondary)
    tf = font(FONT_SLIGHT, int(15 * SS))
    ty = uy + int(26 * SS)
    line1 = "Speech becomes text, entirely on your machine."
    line2 = "Nothing leaves the device. No cloud, no account."
    td.text((bx, ty), line1, font=tf, fill=SECONDARY + (255,))
    td.text((bx, ty + int(24 * SS)), line2, font=tf, fill=SECONDARY + (255,))

    # footnote near bottom of body: version + setup note
    ff = font(FONT_REG, int(10 * SS))
    fy = int(h * 0.80)
    td.text((bx, fy), "Setup  ·  version 0.1.0", font=ff, fill=MUTED + (255,))

    base = Image.alpha_composite(base, txt)

    # 5) bottom hairline rule across full width (premium framing) + amber stub
    bottom = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    bd = ImageDraw.Draw(bottom)
    ry = int(h * 0.88)
    bd.line([(int(w * 0.06), ry), (int(w * 0.94), ry)],
            fill=HAIRLINE + (255,), width=max(1, SS))
    bd.rectangle([int(w * 0.06), ry, int(w * 0.06) + int(40 * SS), ry + max(1, SS)],
                 fill=AMBER_SOFT + (255,))
    base = Image.alpha_composite(base, bottom)

    # 6) vignette for the noir framing
    base = Image.alpha_composite(base, vignette(w, h, strength=0.50))

    # flatten + downscale
    flat = base.convert("RGB").resize((W, H), Image.LANCZOS)

    # 7) subtle warm grain on the final, downscaled image
    flat = add_grain(flat, amount=5)
    return flat


def main():
    out = os.path.join(tempfile.gettempdir(), "concept_warm-noir.png")
    img = build_welcome()
    img.save(out, format="PNG")
    print("WROTE:", out)
    return out


if __name__ == "__main__":
    main()

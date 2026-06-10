# -*- coding: utf-8 -*-
"""
concept_graphite-platinum.py — VoxFlow installer visual concept: GRAPHITE-PLATINUM.

A WELCOME-page mock (600x400) for the Inno Setup wizard in a restrained
"dark premium" / luxury-tech direction. NO acid double-neon.

Direction
  Deep graphite/charcoal field with a barely-there vertical gradient and a fine
  film grain. A SINGLE platinum-champagne accent (#C9B68C) appears only as a thin
  hairline, a tiny diacritic mark, and a slim text cursor — never as flood color.
  Large, quiet "VoxFlow" wordmark (Segoe UI Light, generous tracking) + a thin
  upper kicker and a muted tagline. Lots of air, hairline rules, no glow, no neon.

Echoes the app's monochrome-editorial UI and the speech-bubble / blinking-cursor
logo concept, but keeps it dark and expensive.

Renders to %TEMP%/concept_graphite-platinum.png (RGB, ~600x400 @ 3x supersample).
Pure Pillow.
"""

import os
import math
import random
import tempfile
from PIL import Image, ImageDraw, ImageFont, ImageFilter

# ------------------------------------------------------------------ palette ---
# Deep graphite field + one platinum-champagne accent. Everything else is
# greyscale: the restraint IS the brand.
BG_TOP      = (0x15, 0x16, 0x19)   # #151619  slightly lifted top
BG_BOTTOM   = (0x0B, 0x0C, 0x0E)   # #0B0C0E  near-black floor
INK         = (0xEC, 0xEC, 0xEE)   # #ECECEE  primary wordmark ink (soft white)
MUTED       = (0x8C, 0x8E, 0x94)   # #8C8E94  secondary / tagline
FAINT       = (0x55, 0x57, 0x5D)   # #55575D  kicker / fine labels
HAIRLINE    = (0x2A, 0x2C, 0x31)   # #2A2C31  thin dividers / panel edges
PLATINUM    = (0xC9, 0xB6, 0x8C)   # #C9B68C  THE accent — champagne/platinum

PALETTE = {
    "bg_top":   "#151619",
    "bg_bottom": "#0B0C0E",
    "ink":      "#ECECEE",
    "muted":    "#8C8E94",
    "faint":    "#55575D",
    "hairline": "#2A2C31",
    "accent_platinum": "#C9B68C",
}

SS = 3  # supersample
W, H = 600, 400

FONT_LIGHT = r"C:\Windows\Fonts\segoeuil.ttf"   # Segoe UI Light
FONT_SEMI  = r"C:\Windows\Fonts\segoeuisl.ttf"  # Segoe UI Semilight
FONT_REG   = r"C:\Windows\Fonts\segoeui.ttf"
FONT_BOLD  = r"C:\Windows\Fonts\segoeuib.ttf"


def font(path, size):
    return ImageFont.truetype(path, size)


def lerp(a, b, t):
    return tuple(int(round(a[i] + (b[i] - a[i]) * t)) for i in range(3))


def vgrad(w, h, top, bottom):
    """Subtle vertical gradient with a faint radial lift toward upper-center."""
    img = Image.new("RGB", (w, h))
    px = img.load()
    cx, cy = w * 0.34, h * 0.30
    maxd = math.hypot(w, h)
    for y in range(h):
        t = y / max(1, h - 1)
        base = lerp(top, bottom, t)
        for x in range(w):
            # gentle radial vignette-lift: a touch brighter near the wordmark area
            d = math.hypot(x - cx, y - cy) / maxd
            lift = max(0.0, 0.05 * (1.0 - d * 1.7))
            c = tuple(min(255, int(base[i] + (255 - base[i]) * lift)) for i in range(3))
            px[x, y] = c
    return img


def draw_tracked_text(draw, xy, text, fnt, fill, tracking=0):
    """Letter-spaced text. tracking in (supersampled) px between glyphs.
    Returns total advance width."""
    x, y = xy
    total = 0
    for i, ch in enumerate(text):
        draw.text((x + total, y), ch, font=fnt, fill=fill)
        bbox = draw.textbbox((0, 0), ch, font=fnt)
        adv = bbox[2] - bbox[0]
        # use real advance for spaces too
        if ch == " ":
            adv = fnt.getbbox("n")[2]
        total += adv + tracking
    return total - tracking if text else 0


def measure_tracked(draw, text, fnt, tracking=0):
    total = 0
    for ch in text:
        bbox = draw.textbbox((0, 0), ch, font=fnt)
        adv = bbox[2] - bbox[0]
        if ch == " ":
            adv = fnt.getbbox("n")[2]
        total += adv + tracking
    return total - tracking if text else 0


def add_grain(img, amount=7, seed=7):
    """Fine monochrome film grain, blended subtly (luxury texture, not noise)."""
    rnd = random.Random(seed)
    w, h = img.size
    noise = Image.new("L", (w, h))
    npx = noise.load()
    for y in range(h):
        for x in range(w):
            npx[x, y] = 128 + rnd.randint(-amount, amount)
    # soft-light-ish blend: overlay the centered noise at low opacity
    noise_rgb = Image.merge("RGB", (noise, noise, noise))
    return Image.blend(img, noise_rgb, 0.045)


def build():
    w, h = W * SS, H * SS

    # 1) graphite field
    img = vgrad(w, h, BG_TOP, BG_BOTTOM)
    d = ImageDraw.Draw(img)

    # 2) outer frame inset (hairline rectangle) — premium "card" containment
    inset = int(18 * SS)
    d.rectangle([inset, inset, w - inset, h - inset],
                outline=HAIRLINE, width=max(1, SS))

    # 3) tiny accent corner tick (top-left) — single platinum diacritic mark
    tick = int(10 * SS)
    tx, ty = inset, inset
    d.line([(tx, ty + tick), (tx, ty), (tx + tick, ty)], fill=PLATINUM, width=max(1, SS))

    # ---- text block, left-aligned with generous left margin & air ----
    left = int(54 * SS)

    # 4) kicker (top, faint, wide tracking, uppercase)
    kick_f = font(FONT_SEMI, int(11 * SS))
    kicker = "LOCAL DICTATION"
    ky = int(78 * SS)
    draw_tracked_text(d, (left, ky), kicker, kick_f, FAINT, tracking=int(4.5 * SS))
    # short platinum hairline under the kicker
    kw = measure_tracked(d, kicker, kick_f, tracking=int(4.5 * SS))
    d.line([(left, ky + int(22 * SS)), (left + kw, ky + int(22 * SS))],
           fill=HAIRLINE, width=max(1, SS))

    # 5) wordmark "VoxFlow" — large, quiet, Segoe UI Light, gentle tracking
    word_f = font(FONT_LIGHT, int(72 * SS))
    wy = int(132 * SS)
    word_w = draw_tracked_text(d, (left, wy), "VoxFlow", word_f, INK,
                               tracking=int(0.5 * SS))

    # platinum text cursor right after the wordmark (the "blinking cursor" motif)
    cur_h = int(60 * SS)
    cur_w = int(5 * SS)
    cur_x = left + word_w + int(10 * SS)
    cur_y = wy + int(18 * SS)
    d.rectangle([cur_x, cur_y, cur_x + cur_w, cur_y + cur_h], fill=PLATINUM)

    # 6) tagline — muted, regular weight, readable
    tag_f = font(FONT_REG, int(17 * SS))
    tag = "Speech to text. On your machine. Nothing leaves."
    tagy = int(228 * SS)
    d.text((left, tagy), tag, font=tag_f, fill=MUTED)

    # 7) bottom meta row: a hairline divider + tiny labels (version / publisher)
    by = int(322 * SS)
    d.line([(left, by), (w - inset - int(20 * SS), by)],
           fill=HAIRLINE, width=max(1, SS))
    meta_f = font(FONT_SEMI, int(10 * SS))
    d.text((left, by + int(14 * SS)), "VERSION 0.1.0",
           font=meta_f, fill=FAINT)
    # right-aligned platinum dot + "SETUP"
    setup_lbl = "SETUP"
    sw = measure_tracked(d, setup_lbl, meta_f, tracking=int(3 * SS))
    sx = w - inset - int(20 * SS) - sw
    draw_tracked_text(d, (sx, by + int(14 * SS)), setup_lbl, meta_f,
                      FAINT, tracking=int(3 * SS))
    dot_r = int(2.5 * SS)
    d.ellipse([sx - int(16 * SS) - dot_r, by + int(19 * SS) - dot_r,
               sx - int(16 * SS) + dot_r, by + int(19 * SS) + dot_r],
              fill=PLATINUM)

    # 8) faint speech-bubble watermark, off the right edge (brand echo, whisper-quiet)
    #    Drawn large and partly cropped by the frame so it reads as texture, not a logo.
    bub = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    bd = ImageDraw.Draw(bub)
    bx0 = int(W * 0.70) * SS
    by0 = int(H * 0.20) * SS
    bw = int(W * 0.46) * SS
    bh = int(bw * 0.72)
    rad = int(bh * 0.32)
    edge = (PLATINUM[0], PLATINUM[1], PLATINUM[2], 20)   # extremely faint hairline
    bd.rounded_rectangle([bx0, by0, bx0 + bw, by0 + bh], radius=rad,
                         outline=edge, width=max(1, int(1.4 * SS)))
    # tail (clean wedge at lower-left of the bubble)
    bw_line = max(1, int(1.4 * SS))
    bd.line([(bx0 + int(bw * 0.22), by0 + bh - bw_line),
             (bx0 + int(bw * 0.14), by0 + bh + int(bh * 0.16))],
            fill=edge, width=bw_line)
    bd.line([(bx0 + int(bw * 0.14), by0 + bh + int(bh * 0.16)),
             (bx0 + int(bw * 0.34), by0 + bh - bw_line)],
            fill=edge, width=bw_line)
    # three faint text-line dashes inside the bubble (the logo's "lines of text")
    line_edge = (PLATINUM[0], PLATINUM[1], PLATINUM[2], 16)
    lx = bx0 + int(bw * 0.16)
    for k, frac in enumerate((0.64, 0.78, 0.40)):
        ly = by0 + int(bh * (0.30 + k * 0.18))
        bd.line([(lx, ly), (lx + int(bw * frac), ly)],
                fill=line_edge, width=max(1, int(3 * SS)))
    img = Image.alpha_composite(img.convert("RGBA"), bub).convert("RGB")

    # 9) downscale (LANCZOS) then add fine grain at final resolution
    img = img.resize((W, H), Image.LANCZOS)
    img = add_grain(img, amount=8, seed=11)
    return img


def main():
    out = os.path.join(tempfile.gettempdir(), "concept_graphite-platinum.png")
    img = build()
    img.save(out, format="PNG")
    print("WROTE", out, img.size)
    return out


if __name__ == "__main__":
    main()

# -*- coding: utf-8 -*-
"""
concept_midnight-steel.py — VoxFlow installer visual concept [midnight-steel].

Renders a MOCK of the Welcome page (~600x400) to %TEMP%/concept_midnight-steel.png
so the dark-premium direction can be reviewed before regenerating the real
Inno wizard BMP assets.

Direction "midnight-steel":
  Almost-black blue-steel ground (#0A0D12), one dosed muted steel-teal accent
  (#5E7E84). Geometric minimalism, thin divider hairlines, generous air, clean
  typography, faint film grain. The brand glyph (speech bubble of text lines +
  blinking cursor, from voxflow/branding/installer-mono.svg) is restated as a
  quiet recessed line-mark, NOT a glowing neon sign. Serious / premium dev-tool.

Pure Pillow. Fonts: Segoe UI family + Consolas (mono) from C:\\Windows\\Fonts.
"""

import os
import random
import tempfile
from PIL import Image, ImageDraw, ImageFont, ImageFilter, ImageChops

# ----------------------------------------------------------------- palette ---
# Pillow works in RGB; the Inno [Code] consts are BGR (handled separately).
GROUND      = (0x0A, 0x0D, 0x12)   # #0A0D12 almost-black blue-steel base
GROUND_TOP  = (0x10, 0x14, 0x1B)   # subtle lift at the very top
PANEL       = (0x12, 0x16, 0x1D)   # inset surface
TEXT        = (0xEC, 0xEF, 0xF2)   # near-white primary (cool, not pure #FFF)
SECONDARY   = (0x8A, 0x93, 0x9C)   # cool grey secondary text
MUTED       = (0x59, 0x60, 0x69)   # very quiet labels / footnotes
DIVIDER     = (0x1C, 0x22, 0x2B)   # hairline divider
ACCENT      = (0x5E, 0x7E, 0x84)   # #5E7E84 muted steel-teal — the ONE accent
ACCENT_DIM  = (0x3A, 0x4E, 0x53)   # dimmer accent for fills/hairlines

HERE = os.path.dirname(os.path.abspath(__file__))
OUT  = os.path.join(tempfile.gettempdir(), "concept_midnight-steel.png")

SS = 3  # supersample factor for the master render

# Fonts (TTF available on Windows; app's .woff2 are not Pillow-loadable).
# segoeuisb.ttf (Semibold) is NOT shipped on this machine; we deliberately use
# Semilight for the wordmark to echo the app's editorial light typography, and
# Regular only where a touch more body is wanted.
F_BOLD     = r"C:\Windows\Fonts\segoeuib.ttf"
F_REG      = r"C:\Windows\Fonts\segoeui.ttf"
F_LIGHT    = r"C:\Windows\Fonts\segoeuil.ttf"
F_SEMILT   = r"C:\Windows\Fonts\segoeuisl.ttf"   # Segoe UI Semilight
F_WORDMARK = F_SEMILT                            # editorial, airy, premium
F_MONO     = r"C:\Windows\Fonts\consola.ttf"


def font(path, size):
    try:
        return ImageFont.truetype(path, size)
    except OSError:
        # Graceful fallback chain if a weight is missing on this machine.
        for alt in (F_SEMILT, F_REG, F_LIGHT):
            try:
                return ImageFont.truetype(alt, size)
            except OSError:
                continue
        return ImageFont.load_default()


def lerp(a, b, t):
    return tuple(int(round(a[i] + (b[i] - a[i]) * t)) for i in range(3))


def vgrad(w, h, top, bottom, ease=1.0):
    """Vertical gradient, optionally eased toward the bottom tone."""
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        t = (y / max(1, h - 1)) ** ease
        c = lerp(top, bottom, t)
        for x in range(w):
            px[x, y] = c
    return img


def radial_vignette(w, h, strength=0.55):
    """Darkening vignette mask toward the edges, as an 'L' image."""
    m = Image.new("L", (w, h), 0)
    d = ImageDraw.Draw(m)
    # bright ellipse in the center, blurred -> soft falloff
    pad_x = int(w * 0.06)
    pad_y = int(h * 0.04)
    d.ellipse([pad_x, pad_y, w - pad_x, h - pad_y], fill=255)
    m = m.filter(ImageFilter.GaussianBlur(int(min(w, h) * 0.18)))
    # invert -> dark at edges; scale by strength
    inv = ImageChops.invert(m).point(lambda v: int(v * strength))
    return inv


def grain_layer(w, h, amount=8, seed=7):
    """Subtle monochrome film grain as an RGB overlay around mid-grey 128."""
    rnd = random.Random(seed)
    g = Image.new("L", (w, h))
    g.putdata([128 + rnd.randint(-amount, amount) for _ in range(w * h)])
    return g


def apply_grain(base_rgb, amount=7, seed=7, opacity=0.5):
    """Overlay-blend faint grain onto an RGB image."""
    w, h = base_rgb.size
    g = grain_layer(w, h, amount=amount, seed=seed).convert("RGB")
    blended = ImageChops.overlay(base_rgb, g)
    return Image.blend(base_rgb, blended, opacity)


# ------------------------------------------------- brand glyph (line-mark) ---
def draw_glyph(canvas, x, y, size, line_color, bubble_color, accent_color):
    """
    Quiet speech-bubble line-mark echoing voxflow/branding/installer-mono.svg:
    a thin rounded-rect bubble (with a small tail) holding three text lines and
    one accent cursor block. Drawn with a 1px-equivalent hairline stroke; the
    only color is the dosed steel-teal cursor.
    `size` = bubble width in master px. Everything scales from it.
    """
    d = ImageDraw.Draw(canvas)
    w = size
    h = int(size * 0.74)
    stroke = max(SS, int(size * 0.016))
    radius = int(h * 0.30)

    box = [x, y, x + w, y + h]
    # faint inner fill so the bubble reads as a recessed surface
    d.rounded_rectangle(box, radius=radius, fill=bubble_color)
    # hairline outline
    d.rounded_rectangle(box, radius=radius, outline=line_color, width=stroke)

    # tail — small triangle at lower-left, hairline
    tail_x0 = x + int(w * 0.18)
    tail_x1 = x + int(w * 0.34)
    tail_ty = y + h - stroke
    tail_tipx = x + int(w * 0.14)
    tail_tipy = y + h + int(h * 0.26)
    d.polygon(
        [(tail_x0, tail_ty), (tail_x1, tail_ty), (tail_tipx, tail_tipy)],
        fill=bubble_color,
    )
    d.line([(tail_x0, tail_ty), (tail_tipx, tail_tipy)], fill=line_color, width=stroke)
    d.line([(tail_tipx, tail_tipy), (tail_x1, tail_ty)], fill=line_color, width=stroke)

    # three text lines inside (decreasing widths)
    pad = int(w * 0.16)
    line_h = max(SS, int(h * 0.085))
    gap = int(h * 0.17)
    ly = y + int(h * 0.24)
    widths = [0.62, 0.74, 0.40]
    inner_w = w - pad * 2
    for i, frac in enumerate(widths):
        lw = int(inner_w * frac)
        d.rounded_rectangle(
            [x + pad, ly, x + pad + lw, ly + line_h],
            radius=line_h // 2, fill=line_color,
        )
        ly += line_h + gap

    # accent cursor block — the single dosed color, sits at the 3rd line end
    cur_w = max(SS, int(w * 0.042))
    cur_h = int(line_h * 2.6)
    cur_x = x + pad + int(inner_w * 0.40) + int(w * 0.055)
    cur_y = ly - (line_h + gap) - int((cur_h - line_h) * 0.5)
    d.rounded_rectangle(
        [cur_x, cur_y, cur_x + cur_w, cur_y + cur_h],
        radius=cur_w // 2, fill=accent_color,
    )


def text_tracked(draw, xy, s, fnt, fill, tracking=0):
    """Draw text with manual letter-spacing (tracking, in px)."""
    x, y = xy
    for ch in s:
        draw.text((x, y), ch, font=fnt, fill=fill)
        bb = draw.textbbox((0, 0), ch, font=fnt)
        x += (bb[2] - bb[0]) + tracking
    return x


def measure_tracked(draw, s, fnt, tracking=0):
    w = 0
    for ch in s:
        bb = draw.textbbox((0, 0), ch, font=fnt)
        w += (bb[2] - bb[0]) + tracking
    return w - tracking if s else 0


# ------------------------------------------------------------------ build ----
def build():
    W, H = 600, 400
    w, h = W * SS, H * SS

    # 1) ground: very subtle vertical gradient, near-black blue-steel
    img = vgrad(w, h, GROUND_TOP, GROUND, ease=1.4)

    # 2) edge vignette to focus the center, keep it premium-dark
    vig = radial_vignette(w, h, strength=0.50)
    dark = Image.new("RGB", (w, h), (0x05, 0x07, 0x0A))
    img = Image.composite(dark, img, vig)

    d = ImageDraw.Draw(img)

    # --- layout geometry ---
    margin = int(w * 0.085)
    # A faint vertical accent rule on the far left (like an editorial spine).
    spine_x = int(w * 0.0)
    d.rectangle([spine_x, 0, spine_x + max(SS, int(w * 0.006)), h],
                fill=ACCENT_DIM)

    # 3) brand glyph, upper-left region, quiet
    gsize = int(w * 0.135)
    gx = margin
    gy = int(h * 0.135)
    draw_glyph(img, gx, gy, gsize,
               line_color=lerp(GROUND, TEXT, 0.55),  # soft, not pure white
               bubble_color=lerp(GROUND, PANEL, 0.85),
               accent_color=ACCENT)

    # 4) kicker label — mono, uppercase, tracked, muted (above wordmark)
    kicker_font = font(F_MONO, int(w * 0.0235))
    ky = int(h * 0.40)
    text_tracked(d, (margin, ky), "LOCAL  DICTATION  /  v0.1.0",
                 kicker_font, MUTED, tracking=int(w * 0.006))

    # thin accent tick before/with the kicker for one spark of color
    d.rectangle([margin, ky - int(h * 0.035),
                 margin + int(w * 0.045), ky - int(h * 0.035) + max(SS, int(h * 0.004))],
                fill=ACCENT)

    # 5) wordmark "VoxFlow" — large, cool near-white, editorial semilight
    wm_font = font(F_WORDMARK, int(w * 0.108))
    wm_y = int(h * 0.455)
    # measure for baseline placement
    bb = d.textbbox((0, 0), "VoxFlow", font=wm_font)
    d.text((margin - bb[0], wm_y - bb[1]), "VoxFlow", font=wm_font,
           fill=TEXT)
    wm_w = bb[2] - bb[0]
    wm_h = bb[3] - bb[1]

    # 6) thin divider hairline under the wordmark
    div_y = wm_y - bb[1] + wm_h + int(h * 0.055)
    d.rectangle([margin, div_y, w - margin, div_y + max(1, int(h * 0.0025))],
                fill=DIVIDER)
    # a short accent segment overlapping the divider (dosed)
    d.rectangle([margin, div_y, margin + int(w * 0.12),
                 div_y + max(SS, int(h * 0.0035))], fill=ACCENT)

    # 7) tagline — secondary, semilight, generous line height
    tag_font = font(F_SEMILT, int(w * 0.032))
    tag_y = div_y + int(h * 0.055)
    line1 = "Speak. It types. Everything stays on your machine."
    d.text((margin, tag_y), line1, font=tag_font, fill=SECONDARY)
    tb = d.textbbox((0, 0), line1, font=tag_font)
    line2 = "Private voice-to-text for people who write."
    d.text((margin, tag_y + (tb[3] - tb[1]) + int(h * 0.035)), line2,
           font=tag_font, fill=lerp(GROUND, SECONDARY, 0.78))

    # 8) bottom-right footnote — quiet provenance, mono
    fn_font = font(F_MONO, int(w * 0.020))
    fn = "SETUP"
    fn_w = measure_tracked(d, fn, fn_font, tracking=int(w * 0.008))
    text_tracked(d, (w - margin - fn_w, int(h * 0.90)), fn, fn_font,
                 MUTED, tracking=int(w * 0.008))

    # 9) faint hairline frame inset (premium "card" feel), very subtle
    inset = int(w * 0.028)
    d.rectangle([inset, inset, w - inset, h - inset],
                outline=lerp(GROUND, DIVIDER, 1.0), width=max(1, int(SS * 0.8)))

    # --- downscale to target, then add grain at final resolution ---
    img = img.resize((W, H), Image.LANCZOS)
    img = apply_grain(img, amount=6, seed=11, opacity=0.45)
    return img


def main():
    img = build()
    img.save(OUT, format="PNG")
    print("WROTE:", OUT, img.size)


if __name__ == "__main__":
    main()

# -*- coding: utf-8 -*-
"""
build_neon_art.py — VoxFlow Inno Setup wizard NEON asset generator.

Pure Pillow (PIL). No headless Chrome. Renders a high-res master per asset,
then downscales with LANCZOS. Bakes the near-black base into every BMP because
Inno wizard BMPs cannot use alpha.

Outputs (relative to this file's directory = installer/assets):
  Left banner (WizardImageFile)        : wizard-banner-164/-246/-328/-459.bmp
  Top-right small (WizardSmallImageFile): wizard-small-55/-83/-110/-138.bmp
  Progress gradient                    : progress-grad.bmp        (500x18, 24-bit)
  Installer icon                       : voxflow-neon.ico         (16..256)

Aesthetic (brief §4-§5):
  base       #0A0A0F   panel #111118   text #F5F5F7
  secondary  #8A8A99   divider #1E1E28
  neon-cyan  #00E5FF   neon-magenta #FF2BD6
  Brand glyph = speech bubble with glowing neon outline; wordmark "VoxFlow"
  in Segoe UI. Glow = Gaussian-blurred bright copies composited under crisp shape.
"""

import os
from PIL import Image, ImageDraw, ImageFont, ImageFilter

# ---------------------------------------------------------------- palette ----
BASE        = (0x0A, 0x0A, 0x0F)   # near-black base
PANEL       = (0x11, 0x11, 0x18)
TEXT        = (0xF5, 0xF5, 0xF7)
SECONDARY   = (0x8A, 0x8A, 0x99)
DIVIDER     = (0x1E, 0x1E, 0x28)
NEON_CYAN   = (0x00, 0xE5, 0xFF)
NEON_MAG    = (0xFF, 0x2B, 0xD6)

PALETTE = {
    "base": "#0A0A0F", "panel": "#111118", "text": "#F5F5F7",
    "secondary": "#8A8A99", "divider": "#1E1E28",
    "neon_cyan": "#00E5FF", "neon_magenta": "#FF2BD6",
}

HERE = os.path.dirname(os.path.abspath(__file__))

SS = 4  # supersampling factor for masters

FONT_BOLD = r"C:\Windows\Fonts\segoeuib.ttf"
FONT_REG  = r"C:\Windows\Fonts\segoeui.ttf"


def load_font(path, size):
    return ImageFont.truetype(path, size)


def lerp(a, b, t):
    return tuple(int(round(a[i] + (b[i] - a[i]) * t)) for i in range(3))


# --------------------------------------------------------- bubble geometry ---
def rounded_rect_path(draw, box, radius):
    """Draw a filled rounded rectangle onto a single-channel mask draw obj."""
    draw.rounded_rectangle(box, radius=radius, fill=255)


def make_bubble_mask(w, h, stroke):
    """
    Build a speech-bubble OUTLINE mask (white outline on black, mode 'L').
    The bubble is a rounded rectangle body with a tail at lower-left.
    `stroke` = outline thickness in px (in master/supersampled space).
    Returns an 'L' image sized (w, h).
    """
    fill = Image.new("L", (w, h), 0)
    fd = ImageDraw.Draw(fill)

    # Body box with margins; leave room at the bottom for the tail.
    mx = int(w * 0.14)
    my_top = int(h * 0.16)
    my_bot = int(h * 0.34)
    body = [mx, my_top, w - mx, h - my_bot]
    radius = int(min(body[2] - body[0], body[3] - body[1]) * 0.30)
    fd.rounded_rectangle(body, radius=radius, fill=255)

    # Tail: a triangle hanging from the lower-left of the body.
    tail_base_x0 = mx + int((body[2] - body[0]) * 0.18)
    tail_base_x1 = mx + int((body[2] - body[0]) * 0.42)
    tail_top_y = body[3] - stroke  # overlap slightly into the body
    tail_tip_x = mx + int((body[2] - body[0]) * 0.16)
    tail_tip_y = body[3] + int((h - body[3]) * 0.62)
    fd.polygon(
        [(tail_base_x0, tail_top_y),
         (tail_base_x1, tail_top_y),
         (tail_tip_x, tail_tip_y)],
        fill=255,
    )

    # Erode the filled silhouette to obtain an outline: filled - inner.
    inner = Image.new("L", (w, h), 0)
    idd = ImageDraw.Draw(inner)
    ib = [body[0] + stroke, body[1] + stroke, body[2] - stroke, body[3] - stroke]
    ir = max(1, radius - stroke)
    idd.rounded_rectangle(ib, radius=ir, fill=255)
    # inner tail (shrunk)
    shrink = stroke
    idd.polygon(
        [(tail_base_x0 + shrink, tail_top_y - shrink),
         (tail_base_x1 - shrink, tail_top_y - shrink),
         (tail_tip_x + int(shrink * 0.6), tail_tip_y - int(shrink * 1.6))],
        fill=255,
    )

    # outline = fill AND NOT inner
    from PIL import ImageChops
    outline = ImageChops.subtract(fill, inner)

    # Optional: dots inside the bubble (three message dots) for brand character.
    dots = Image.new("L", (w, h), 0)
    dd = ImageDraw.Draw(dots)
    cy = (body[1] + body[3]) // 2
    span = (body[2] - body[0])
    r = max(2, int(span * 0.045))
    gap = int(span * 0.20)
    cx = (body[0] + body[2]) // 2
    for off in (-gap, 0, gap):
        dd.ellipse([cx + off - r, cy - r, cx + off + r, cy + r], fill=255)
    outline = ImageChops.lighter(outline, dots)

    return outline


def colorize_mask(mask, color):
    """Turn an 'L' mask into an RGBA image of solid `color` with mask as alpha."""
    solid = Image.new("RGBA", mask.size, color + (255,))
    out = Image.new("RGBA", mask.size, (0, 0, 0, 0))
    out.paste(solid, (0, 0), mask)
    return out


def neon_glyph_rgba(w, h, stroke, glow_color, core_color, glow_radius,
                    glow_passes=2):
    """
    Render a neon speech-bubble glyph as RGBA (transparent background).
    Layers blurred colored copies (glow) under a crisp white-ish core stroke.
    """
    mask = make_bubble_mask(w, h, stroke)

    canvas = Image.new("RGBA", (w, h), (0, 0, 0, 0))

    # Glow layers: blurred copies of the colored mask, increasing spread.
    for i in range(glow_passes, 0, -1):
        rad = glow_radius * i / glow_passes
        glow = colorize_mask(mask, glow_color)
        glow = glow.filter(ImageFilter.GaussianBlur(rad))
        canvas = Image.alpha_composite(canvas, glow)
    # one tight inner glow for punch
    tight = colorize_mask(mask, glow_color).filter(
        ImageFilter.GaussianBlur(max(1, glow_radius * 0.25)))
    canvas = Image.alpha_composite(canvas, tight)

    # Crisp core stroke (bright, near-white tinted toward core_color).
    core = colorize_mask(mask, core_color)
    canvas = Image.alpha_composite(canvas, core)

    return canvas


# ------------------------------------------------------------ compositors ----
def flatten_on_base(rgba, base=BASE):
    """Composite an RGBA image onto an opaque base -> RGB."""
    bg = Image.new("RGBA", rgba.size, base + (255,))
    out = Image.alpha_composite(bg, rgba)
    return out.convert("RGB")


def vertical_base_gradient(w, h, top, bottom):
    """Subtle vertical gradient between two near-black tones (RGB image)."""
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        t = y / max(1, h - 1)
        c = lerp(top, bottom, t)
        for x in range(w):
            px[x, y] = c
    return img


def radial_glow_layer(w, h, center, color, radius, intensity=1.0):
    """An RGBA soft radial glow blob centered at `center`."""
    layer = Image.new("L", (w, h), 0)
    d = ImageDraw.Draw(layer)
    cx, cy = center
    d.ellipse([cx - radius, cy - radius, cx + radius, cy + radius], fill=255)
    layer = layer.filter(ImageFilter.GaussianBlur(radius * 0.6))
    if intensity != 1.0:
        layer = layer.point(lambda v: int(v * intensity))
    return colorize_mask(layer, color)


# --------------------------------------------------------------- banner ------
def build_banner_master():
    """
    Build the left-banner master at the largest target aspect (164x314),
    supersampled. Returns an RGB master image. Downscaling done by caller.
    """
    BW, BH = 459, 916          # largest target, same 164:314 aspect-ish
    # Note: targets are 164x314, 246x459, 328x627, 459x916 -> ratio varies
    # slightly; we render at the 459x916 frame and resize to each exact size.
    w, h = BW * SS, BH * SS

    # 1) base vertical gradient (very subtle, panel-ish at top)
    bg = vertical_base_gradient(
        w, h,
        top=(0x0D, 0x0D, 0x14),
        bottom=BASE,
    ).convert("RGBA")

    # 2) ambient neon glows in the background (very soft, low intensity)
    glowA = radial_glow_layer(w, h, (int(w * 0.30), int(h * 0.30)),
                              NEON_CYAN, int(w * 0.55), intensity=0.22)
    glowB = radial_glow_layer(w, h, (int(w * 0.78), int(h * 0.72)),
                              NEON_MAG, int(w * 0.55), intensity=0.18)
    bg = Image.alpha_composite(bg, glowA)
    bg = Image.alpha_composite(bg, glowB)

    # 3) thin neon divider near the bottom (cyan->magenta), subtle
    div = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    dd = ImageDraw.Draw(div)
    dy = int(h * 0.80)
    dh = max(1, int(h * 0.0035))
    for x in range(0, w, SS):
        t = x / max(1, w - 1)
        c = lerp(NEON_CYAN, NEON_MAG, t)
        dd.rectangle([x, dy, x + SS, dy + dh], fill=c + (140,))
    div = div.filter(ImageFilter.GaussianBlur(SS * 1.0))
    bg = Image.alpha_composite(bg, div)

    # 4) brand glyph: large neon speech bubble, upper-center
    gw = int(w * 0.56)
    gh = int(gw * 0.92)
    stroke = max(2, int(gw * 0.045))
    glyph = neon_glyph_rgba(
        gw, gh, stroke,
        glow_color=NEON_CYAN,
        core_color=(0xEA, 0xFE, 0xFF),  # white tinted cyan
        glow_radius=stroke * 3.2,
        glow_passes=3,
    )
    # add a magenta rim glow underneath for two-tone neon
    rim = neon_glyph_rgba(
        gw, gh, max(2, int(stroke * 0.7)),
        glow_color=NEON_MAG,
        core_color=NEON_MAG,
        glow_radius=stroke * 4.0,
        glow_passes=2,
    )
    gx = (w - gw) // 2
    gy = int(h * 0.18)
    composite = bg
    rim_layer = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    rim_layer.paste(rim, (gx, gy), rim)
    composite = Image.alpha_composite(composite, rim_layer)
    glyph_layer = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    glyph_layer.paste(glyph, (gx, gy), glyph)
    composite = Image.alpha_composite(composite, glyph_layer)

    # 5) wordmark "VoxFlow" in Segoe UI Bold, white, with faint neon glow
    font_size = int(w * 0.16)
    font = load_font(FONT_BOLD, font_size)
    word = "VoxFlow"
    # measure
    tmp = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    td = ImageDraw.Draw(tmp)
    bbox = td.textbbox((0, 0), word, font=font)
    tw = bbox[2] - bbox[0]
    th = bbox[3] - bbox[1]
    tx = (w - tw) // 2 - bbox[0]
    ty = gy + gh + int(h * 0.04) - bbox[1]

    # glow copy
    glow_txt = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    gtd = ImageDraw.Draw(glow_txt)
    gtd.text((tx, ty), word, font=font, fill=NEON_CYAN + (200,))
    glow_txt = glow_txt.filter(ImageFilter.GaussianBlur(SS * 3.0))
    composite = Image.alpha_composite(composite, glow_txt)
    # crisp white text
    crisp_txt = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    ctd = ImageDraw.Draw(crisp_txt)
    ctd.text((tx, ty), word, font=font, fill=TEXT + (255,))
    composite = Image.alpha_composite(composite, crisp_txt)

    # 6) small tagline in secondary gray
    tag_font = load_font(FONT_REG, int(w * 0.052))
    tag = "Voice to text"
    tb = td.textbbox((0, 0), tag, font=tag_font)
    tagw = tb[2] - tb[0]
    tagx = (w - tagw) // 2 - tb[0]
    tagy = ty + th + int(h * 0.05)
    tag_layer = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    tld = ImageDraw.Draw(tag_layer)
    tld.text((tagx, tagy), tag, font=tag_font, fill=SECONDARY + (235,))
    composite = Image.alpha_composite(composite, tag_layer)

    return flatten_on_base(composite)


# ----------------------------------------------------------- small image -----
def build_small_master():
    """
    Top-right small image: bubble glyph with neon ring, on dark.
    Rendered square (140 target max). Returns RGB master.
    """
    S = 140
    w = h = S * SS

    bg = vertical_base_gradient(
        w, h, top=(0x10, 0x10, 0x18), bottom=BASE).convert("RGBA")

    # neon ring behind the glyph (cyan->magenta arc-ish via two glows)
    ringA = radial_glow_layer(w, h, (w // 2, h // 2),
                              NEON_CYAN, int(w * 0.46), intensity=0.30)
    ringB = radial_glow_layer(w, h, (w // 2, h // 2),
                              NEON_MAG, int(w * 0.30), intensity=0.22)
    bg = Image.alpha_composite(bg, ringA)
    bg = Image.alpha_composite(bg, ringB)

    # crisp thin neon ring outline
    ring = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    rd = ImageDraw.Draw(ring)
    pad = int(w * 0.10)
    rw = max(2, int(w * 0.018))
    rd.ellipse([pad, pad, w - pad, h - pad], outline=NEON_CYAN + (255,),
               width=rw)
    ring_glow = ring.filter(ImageFilter.GaussianBlur(SS * 2.2))
    bg = Image.alpha_composite(bg, ring_glow)
    bg = Image.alpha_composite(bg, ring)

    # bubble glyph centered, smaller than ring
    gw = int(w * 0.50)
    gh = int(gw * 0.92)
    stroke = max(2, int(gw * 0.06))
    glyph = neon_glyph_rgba(
        gw, gh, stroke,
        glow_color=NEON_CYAN,
        core_color=(0xEA, 0xFE, 0xFF),
        glow_radius=stroke * 3.0,
        glow_passes=3,
    )
    gx = (w - gw) // 2
    gy = (h - gh) // 2
    gl = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    gl.paste(glyph, (gx, gy), glyph)
    bg = Image.alpha_composite(bg, gl)

    return flatten_on_base(bg)


# ------------------------------------------------------------- gradient ------
def build_progress_gradient():
    """Horizontal cyan->magenta gradient, 500x18, with brighter top edge."""
    W, H = 500, 18
    img = Image.new("RGB", (W, H))
    px = img.load()
    for x in range(W):
        t = x / (W - 1)
        c = lerp(NEON_CYAN, NEON_MAG, t)
        for y in range(H):
            # brighter top edge -> neon look. Top rows lifted toward white.
            edge = max(0.0, 1.0 - (y / (H * 0.45)))  # 1 at top, 0 mid
            lift = 0.45 * edge
            cc = tuple(min(255, int(c[i] + (255 - c[i]) * lift)) for i in range(3))
            # slight darkening at the very bottom for depth
            if y > H * 0.7:
                dk = (y - H * 0.7) / (H * 0.3)
                cc = tuple(int(cc[i] * (1.0 - 0.18 * dk)) for i in range(3))
            px[x, y] = cc
    return img


# ---------------------------------------------------------------- icon -------
def build_icon_layer(size):
    """Render one icon size (RGBA, transparent bg): bubble glyph, neon cyan."""
    w = h = size * SS
    canvas = Image.new("RGBA", (w, h), (0, 0, 0, 0))

    # soft dark disc behind glyph so it reads on light + dark backgrounds
    disc = Image.new("L", (w, h), 0)
    dd = ImageDraw.Draw(disc)
    pad = int(w * 0.04)
    dd.ellipse([pad, pad, w - pad, h - pad], fill=255)
    disc = disc.filter(ImageFilter.GaussianBlur(w * 0.01))
    disc_rgba = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    disc_rgba.paste(Image.new("RGBA", (w, h), BASE + (235,)), (0, 0), disc)
    canvas = Image.alpha_composite(canvas, disc_rgba)

    gw = int(w * 0.62)
    gh = int(gw * 0.92)
    stroke = max(2, int(gw * 0.07))
    glow_passes = 3 if size >= 48 else 2
    glyph = neon_glyph_rgba(
        gw, gh, stroke,
        glow_color=NEON_CYAN,
        core_color=(0xEA, 0xFE, 0xFF),
        glow_radius=stroke * (3.0 if size >= 48 else 1.8),
        glow_passes=glow_passes,
    )
    gx = (w - gw) // 2
    gy = (h - gh) // 2
    gl = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    gl.paste(glyph, (gx, gy), glyph)
    canvas = Image.alpha_composite(canvas, gl)

    out = canvas.resize((size, size), Image.LANCZOS)
    return out


# ----------------------------------------------------------------- main ------
def save_bmp_rgb(img, path, size):
    """Resize (LANCZOS) to exact size and save as 24-bit BMP (RGB, no alpha)."""
    if img.size != size:
        img = img.resize(size, Image.LANCZOS)
    img = img.convert("RGB")
    img.save(path, format="BMP")


def main():
    os.makedirs(HERE, exist_ok=True)
    written = []

    # ---- banner ----
    banner_master = build_banner_master()
    banner_sizes = {
        "wizard-banner-164.bmp": (164, 314),
        "wizard-banner-246.bmp": (246, 459),
        "wizard-banner-328.bmp": (328, 627),
        "wizard-banner-459.bmp": (459, 916),
    }
    for name, sz in banner_sizes.items():
        p = os.path.join(HERE, name)
        save_bmp_rgb(banner_master, p, sz)
        written.append((p, sz))

    # ---- small ----
    small_master = build_small_master()
    small_sizes = {
        "wizard-small-55.bmp":  (55, 55),
        "wizard-small-83.bmp":  (83, 80),
        "wizard-small-110.bmp": (110, 106),
        "wizard-small-138.bmp": (138, 140),
    }
    for name, sz in small_sizes.items():
        p = os.path.join(HERE, name)
        save_bmp_rgb(small_master, p, sz)
        written.append((p, sz))

    # ---- progress gradient ----
    grad = build_progress_gradient()
    gp = os.path.join(HERE, "progress-grad.bmp")
    grad.save(gp, format="BMP")
    written.append((gp, (500, 18)))

    # ---- icon ----
    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    layers = [build_icon_layer(s) for s in ico_sizes]
    ico_path = os.path.join(HERE, "voxflow-neon.ico")
    # Pillow: save largest, pass sizes list; but to guarantee crisp per-size
    # rendering we save the 256 image with explicit sizes.
    base_layer = layers[-1]  # 256
    base_layer.save(
        ico_path, format="ICO",
        sizes=[(s, s) for s in ico_sizes],
        append_images=layers[:-1],
    )
    written.append((ico_path, "ico"))

    print("=== WRITTEN ===")
    for p, sz in written:
        print(f"  {p}  {sz}")

    return written


if __name__ == "__main__":
    main()

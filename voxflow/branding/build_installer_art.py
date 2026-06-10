#!/usr/bin/env python3
"""VoxFlow installer art builder — MONOCHROME editorial wizard.

Renders the NSIS wizard images (header 150x57, sidebar 164x314) and a
monochrome installer icon from HTML/SVG via headless Chrome, then converts
to the exact formats NSIS needs:
  - wizard images -> 24-bit BMP (BI_RGB, no alpha), white background
  - installer icon -> multi-size ICO (16..256), transparent, per-size art

Cyrillic-path gotcha (R5/D-019): Chrome writes --screenshot relative to its
cwd and chokes on Cyrillic. So we copy inputs into an ASCII temp dir, render
there, and let Pillow (which handles Cyrillic output fine) write the BMP/ICO
into the real (Cyrillic) project paths.
"""
import os, io, struct, shutil, subprocess
from PIL import Image

CHROME = r"C:\Program Files\Google\Chrome\Application\chrome.exe"
BRAND = os.path.dirname(os.path.abspath(__file__))
INSTALLER_DIR = os.path.normpath(os.path.join(BRAND, "..", "src-tauri", "installer"))
ICONS_DIR = os.path.normpath(os.path.join(BRAND, "..", "src-tauri", "icons"))
FONTS_DIR = os.path.normpath(os.path.join(BRAND, "..", "src", "assets", "fonts"))

TMP = os.path.join(os.environ.get("TEMP", r"C:\Windows\Temp"), "voxinst")
PROFILE = os.path.join(TMP, "cprofile")
if os.path.exists(TMP):
    shutil.rmtree(TMP)
os.makedirs(PROFILE)

FONTS = [
    "unbounded-latin-800.woff2", "unbounded-latin-700.woff2",
    "plex-sans-cyrillic-500.woff2", "plex-sans-cyrillic-600.woff2",
    "plex-mono-cyrillic-500.woff2",
]
ASSETS = [
    "installer-header.html", "installer-sidebar.html",
    "installer-mono.svg", "installer-mono-small.svg",
]
for f in FONTS:
    shutil.copy(os.path.join(FONTS_DIR, f), os.path.join(TMP, f))
for f in ASSETS:
    shutil.copy(os.path.join(BRAND, f), os.path.join(TMP, f))


def url(name):
    return "file:///" + os.path.join(TMP, name).replace("\\", "/")


def render(src, out_png, w, h, transparent=False):
    bg = "00000000" if transparent else "FFFFFFFF"
    cmd = [
        CHROME, "--headless=new", "--disable-gpu", "--no-sandbox",
        "--no-first-run", "--no-default-browser-check", "--hide-scrollbars",
        f"--user-data-dir={PROFILE}", "--force-device-scale-factor=1",
        f"--default-background-color={bg}", f"--window-size={w},{h}",
        "--virtual-time-budget=4000", f"--screenshot={out_png}", url(src),
    ]
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if not os.path.exists(out_png):
        raise SystemExit(f"render failed: {src} -> {out_png}")


def to_bmp(png, bmp, w, h):
    im = Image.open(png).convert("RGB")
    if im.size != (w, h):
        im = im.resize((w, h), Image.LANCZOS)
    im.save(bmp, "BMP")  # Pillow writes 24-bit BI_RGB for RGB images


def write_ico(path, images):
    blobs = []
    for im in images:
        buf = io.BytesIO()
        im.save(buf, format="PNG")
        blobs.append((im.size[0], im.size[1], buf.getvalue()))
    count = len(blobs)
    header = struct.pack("<HHH", 0, 1, count)
    entries = b""
    offset = 6 + 16 * count
    for w, h, data in blobs:
        bw = 0 if w >= 256 else w
        bh = 0 if h >= 256 else h
        entries += struct.pack("<BBBBHHII", bw, bh, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    with open(path, "wb") as f:
        f.write(header + entries + b"".join(b for _, _, b in blobs))


# ── 1) wizard BMPs (white bg, 24-bit) ──
hp = os.path.join(TMP, "header.png")
sp = os.path.join(TMP, "sidebar.png")
render("installer-header.html", hp, 150, 57)
render("installer-sidebar.html", sp, 164, 314)
to_bmp(hp, os.path.join(INSTALLER_DIR, "installer-header.bmp"), 150, 57)
to_bmp(sp, os.path.join(INSTALLER_DIR, "installer-sidebar.bmp"), 164, 314)

# ── 2) monochrome installer icon (transparent, per-size art) ──
mp = os.path.join(TMP, "mono.png")
msp = os.path.join(TMP, "mono_small.png")
render("installer-mono.svg", mp, 256, 256, transparent=True)
render("installer-mono-small.svg", msp, 256, 256, transparent=True)
master = Image.open(mp).convert("RGBA")
small = Image.open(msp).convert("RGBA")
SRC = {256: master, 128: master, 64: master, 48: master, 32: small, 24: small, 16: small}
SIZES = [256, 128, 64, 48, 32, 24, 16]
frames = [SRC[n].resize((n, n), Image.LANCZOS) for n in SIZES]
write_ico(os.path.join(ICONS_DIR, "icon-mono.ico"), frames)

# ── 3) contact sheet for eyeballing ──
sheet = Image.new("RGB", (150 + 164 + 60, 314 + 40), (210, 210, 210))
sheet.paste(Image.open(hp).convert("RGB"), (20, 20))
sheet.paste(Image.open(sp).convert("RGB"), (20 + 150 + 20, 20))
sheet.save(os.path.join(BRAND, "check-installer.png"))

print("OK")
print("  header.bmp  ", os.path.getsize(os.path.join(INSTALLER_DIR, "installer-header.bmp")), "bytes")
print("  sidebar.bmp ", os.path.getsize(os.path.join(INSTALLER_DIR, "installer-sidebar.bmp")), "bytes")
print("  icon-mono.ico", os.path.getsize(os.path.join(ICONS_DIR, "icon-mono.ico")), "bytes")

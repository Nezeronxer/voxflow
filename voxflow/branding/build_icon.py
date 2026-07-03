#!/usr/bin/env python3
"""VoxFlow icon builder.

Inputs (rendered from SVG via headless Chrome, transparent bg):
  master_1024.png  — detailed bubble, used for sizes 48..256
  small_512.png    — simplified bubble (2 thick lines), used for sizes 16..32

Outputs:
  icon.ico         — multi-size ICO (PNG-compressed frames) with per-size art
  out/<n>.png      — individual rasters (verification + Tauri assets)
  check.png        — contact sheet on white + dark for eyeballing
"""
import io, os, shutil, struct
from PIL import Image

BRAND = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(BRAND, "out")
TAURI_ICONS = os.path.normpath(os.path.join(BRAND, "..", "src-tauri", "icons"))
os.makedirs(OUT, exist_ok=True)
os.makedirs(TAURI_ICONS, exist_ok=True)

master = Image.open(os.path.join(BRAND, "master_1024.png")).convert("RGBA")
small = Image.open(os.path.join(BRAND, "small_512.png")).convert("RGBA")

# which source feeds which size
SRC = {256: master, 128: master, 64: master, 48: master,
       32: small, 24: small, 16: small}
SIZES = [256, 128, 64, 48, 32, 24, 16]

def raster(n):
    return SRC[n].resize((n, n), Image.LANCZOS)

frames = {n: raster(n) for n in SIZES}
for n, im in frames.items():
    im.save(os.path.join(OUT, f"{n}.png"))

# ---- write multi-size ICO with PNG-compressed frames (Vista+; per-size art) ----
def write_ico(path, images):
    # images: list of (PIL.Image) sorted any order
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

ico_frames = [frames[n] for n in SIZES]
write_ico(os.path.join(BRAND, "icon.ico"), ico_frames)
print("icon.ico written:", os.path.getsize(os.path.join(BRAND, "icon.ico")), "bytes,", len(ico_frames), "sizes")

# Keep the Windows/Tauri app icon assets in sync with the canonical branding
# rasters. The installer has its own setup.exe icon; these files are for
# voxflow.exe, window, tray and shortcuts.
frames[32].save(os.path.join(TAURI_ICONS, "32x32.png"))
frames[128].save(os.path.join(TAURI_ICONS, "128x128.png"))
frames[256].save(os.path.join(TAURI_ICONS, "128x128@2x.png"))
frames[64].save(os.path.join(TAURI_ICONS, "64x64.png"))
frames[256].save(os.path.join(TAURI_ICONS, "icon.png"))
shutil.copyfile(os.path.join(BRAND, "icon.ico"), os.path.join(TAURI_ICONS, "icon.ico"))
print("synced Windows/Tauri app icons")

# ---- contact sheet: every size on white and on dark ----
pad = 18
labelh = 16
cols = SIZES
sheet_w = pad + sum(n + pad for n in cols)
sheet_h = pad + 256 + labelh + pad + 256 + labelh + pad
sheet = Image.new("RGBA", (sheet_w, sheet_h), (255, 255, 255, 255))
dark = Image.new("RGBA", (sheet_w, sheet_h), (31, 39, 51, 255))
# top band white, bottom band dark
sheet.paste(dark.crop((0, sheet_h // 2, sheet_w, sheet_h)), (0, sheet_h // 2))

x = pad
for n in cols:
    im = frames[n]
    # white band (top): center each in a 256 tall slot, baseline-aligned to bottom
    y_top = pad + (256 - n)
    sheet.alpha_composite(im, (x, y_top))
    # dark band (bottom)
    y_bot = sheet_h // 2 + pad + (256 - n)
    sheet.alpha_composite(im, (x, y_bot))
    x += n + pad

sheet.convert("RGB").save(os.path.join(BRAND, "check.png"))
print("check.png written")

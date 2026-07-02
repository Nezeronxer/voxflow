#!/usr/bin/env python3
"""Build the VoxFlow installer executable icon.

The app icon itself is dark and tasteful, but setup.exe needs to read clearly in
Explorer on both light and dark backgrounds. This generator creates a brighter
installer-specific multi-size ICO: VoxFlow speech bubble + install arrow badge.
"""

from __future__ import annotations

import struct
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter


HERE = Path(__file__).resolve().parent
OUT = HERE / "voxflow-setup.ico"
SIZES = [256, 128, 64, 48, 32, 24, 16]


def rounded_rect_mask(size: int, radius: int) -> Image.Image:
    mask = Image.new("L", (size, size), 0)
    draw = ImageDraw.Draw(mask)
    draw.rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=255)
    return mask


def make_icon(size: int) -> Image.Image:
    scale = size / 256
    icon = Image.new("RGBA", (size, size), (0, 0, 0, 0))

    tile = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    px = tile.load()
    top = (0, 229, 255)
    bottom = (38, 255, 168)
    for y in range(size):
        t = y / max(1, size - 1)
        color = tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3))
        for x in range(size):
            px[x, y] = (*color, 255)

    mask = rounded_rect_mask(size, max(4, int(size * 0.20)))
    tile.putalpha(mask)

    shadow = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    shadow_mask = mask.filter(ImageFilter.GaussianBlur(max(1, int(size * 0.035))))
    shadow.putalpha(shadow_mask.point(lambda value: int(value * 0.30)))
    icon.alpha_composite(shadow, (0, max(1, int(5 * scale))))
    icon.alpha_composite(tile)

    draw = ImageDraw.Draw(icon)
    dark = (7, 11, 18, 255)
    dark2 = (12, 18, 28, 255)
    white = (255, 255, 255, 255)

    if size < 24:
        x0, y0 = int(size * 0.14), int(size * 0.18)
        x1, y1 = int(size * 0.82), int(size * 0.64)
        stroke = 1
    else:
        x0, y0 = int(size * 0.20), int(size * 0.20)
        x1, y1 = int(size * 0.74), int(size * 0.61)
        stroke = max(2, int(size * 0.055))
    radius = max(5, int(size * 0.10))
    draw.rounded_rectangle(
        [x0, y0, x1, y1],
        radius=radius,
        fill=(255, 255, 255, 236),
        outline=dark,
        width=stroke,
    )
    tail = [
        (int(size * 0.34), y1 - stroke // 2),
        (int(size * 0.25), int(size * 0.74)),
        (int(size * 0.47), y1 - stroke // 2),
    ]
    draw.polygon(tail, fill=(255, 255, 255, 236), outline=dark)

    if size < 24:
        line_h = 1
        line_x = int(size * 0.30)
        for i, frac in enumerate([0.38, 0.28]):
            y = int(size * (0.34 + i * 0.13))
            draw.line([line_x, y, line_x + int(size * frac), y], fill=dark2, width=1)
    elif size >= 32:
        line_h = max(2, int(size * 0.028))
        line_x = int(size * 0.32)
        for i, frac in enumerate([0.34, 0.26, 0.20]):
            y = int(size * (0.32 + i * 0.095))
            draw.rounded_rectangle(
                [line_x, y, line_x + int(size * frac), y + line_h],
                radius=line_h,
                fill=dark2,
            )

    if size >= 24:
        bx0, by0 = int(size * 0.57), int(size * 0.57)
        bx1, by1 = int(size * 0.86), int(size * 0.86)
        draw.ellipse(
            [bx0, by0, bx1, by1],
            fill=dark,
            outline=white,
            width=max(1, int(size * 0.018)),
        )
        cx = (bx0 + bx1) // 2
        top_y = int(size * 0.64)
        bot_y = int(size * 0.78)
        arrow_w = max(3, int(size * 0.045))
        draw.line([cx, top_y, cx, bot_y], fill=white, width=max(2, int(size * 0.030)))
        draw.polygon(
            [(cx - arrow_w, bot_y - arrow_w), (cx + arrow_w, bot_y - arrow_w), (cx, bot_y + arrow_w)],
            fill=white,
        )

    return icon


def write_ico(path: Path, frames: list[Image.Image]) -> None:
    blobs = []
    for frame in frames:
        blobs.append((frame.size[0], frame.size[1], dib_frame(frame)))

    header = struct.pack("<HHH", 0, 1, len(blobs))
    entries = b""
    offset = 6 + 16 * len(blobs)
    for width, height, data in blobs:
        entries += struct.pack(
            "<BBBBHHII",
            0 if width >= 256 else width,
            0 if height >= 256 else height,
            0,
            0,
            1,
            32,
            len(data),
            offset,
        )
        offset += len(data)
    path.write_bytes(header + entries + b"".join(data for _, _, data in blobs))


def dib_frame(frame: Image.Image) -> bytes:
    frame = frame.convert("RGBA")
    width, height = frame.size

    # ICO stores DIB rows bottom-up. For 32-bit icons, BGRA alpha is understood by
    # Win32 and Delphi/VCL code that may not load PNG-compressed ICO frames.
    bgra_rows = []
    for y in range(height - 1, -1, -1):
        row = frame.crop((0, y, width, y + 1)).tobytes("raw", "BGRA")
        bgra_rows.append(row)
    xor_bitmap = b"".join(bgra_rows)

    # 1-bit AND mask, padded to 32-bit row boundaries. Alpha already carries
    # transparency, so the mask is fully opaque.
    mask_row_bytes = ((width + 31) // 32) * 4
    and_mask = b"\x00" * (mask_row_bytes * height)

    header = struct.pack(
        "<IIIHHIIIIII",
        40,  # BITMAPINFOHEADER size
        width,
        height * 2,  # XOR + AND mask height
        1,  # planes
        32,  # bit count
        0,  # BI_RGB
        len(xor_bitmap),
        0,
        0,
        0,
        0,
    )
    return header + xor_bitmap + and_mask


def main() -> None:
    frames = [make_icon(size) for size in SIZES]
    write_ico(OUT, frames)
    print(f"wrote {OUT.name} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()

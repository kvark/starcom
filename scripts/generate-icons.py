#!/usr/bin/env python3
"""Write the Starcom app icon SVG, PNG, and ICO from one set of geometry."""
from __future__ import annotations

import math
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VIEW = 256

BG_TOP = (0x1C, 0x24, 0x34)
BG_BOT = (0x24, 0x2F, 0x44)
STROKE = (0x0F, 0x16, 0x24)
CYAN = (0x56, 0xB6, 0xC2)

# Mirrored prompt `> ■ <`: one commander between two directions.
CHEVRON_SPAN = 32.0
CHEVRON_RADIUS = 13.0
CHEVRON_LEFT = 64.0
CHEVRON_RIGHT = 192.0
CURSOR = (112.0, 112.0, 144.0, 144.0)
CURSOR_RADIUS = 6.0
MID_Y = 128.0


def fmt(value: float) -> str:
    return f"{value:g}"


def svg_text() -> str:
    span = CHEVRON_SPAN
    left_back_x = CHEVRON_LEFT - span * 0.55
    left_tip_x = CHEVRON_LEFT + span * 0.7
    right_back_x = CHEVRON_RIGHT + span * 0.55
    right_tip_x = CHEVRON_RIGHT - span * 0.7
    top_y = MID_Y - span
    bot_y = MID_Y + span
    x0, y0, x1, y1 = CURSOR
    stroke = fmt(CHEVRON_RADIUS * 2)
    return f"""\
<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#1c2434"/>
      <stop offset="1" stop-color="#242f44"/>
    </linearGradient>
  </defs>
  <rect x="8" y="8" width="240" height="240" rx="24" fill="url(#bg)" stroke="#0f1624" stroke-width="8"/>
  <g fill="none" stroke="#56b6c2" stroke-width="{stroke}" stroke-linecap="round">
    <path d="M{fmt(left_back_x)} {fmt(top_y)} L{fmt(left_tip_x)} {fmt(MID_Y)}"/>
    <path d="M{fmt(left_back_x)} {fmt(bot_y)} L{fmt(left_tip_x)} {fmt(MID_Y)}"/>
    <path d="M{fmt(right_back_x)} {fmt(top_y)} L{fmt(right_tip_x)} {fmt(MID_Y)}"/>
    <path d="M{fmt(right_back_x)} {fmt(bot_y)} L{fmt(right_tip_x)} {fmt(MID_Y)}"/>
  </g>
  <rect x="{fmt(x0)}" y="{fmt(y0)}" width="{fmt(x1 - x0)}" height="{fmt(y1 - y0)}" rx="{fmt(CURSOR_RADIUS)}" fill="#56b6c2"/>
</svg>
"""


def sd_round_box(px: float, py: float, x0: float, y0: float, x1: float, y1: float, radius: float) -> float:
    half_w = (x1 - x0) * 0.5
    half_h = (y1 - y0) * 0.5
    radius = min(radius, half_w, half_h)
    dx = abs(px - (x0 + half_w)) - (half_w - radius)
    dy = abs(py - (y0 + half_h)) - (half_h - radius)
    return math.hypot(max(dx, 0.0), max(dy, 0.0)) + min(max(dx, dy), 0.0) - radius


def sd_capsule(px: float, py: float, ax: float, ay: float, bx: float, by: float, radius: float) -> float:
    abx, aby = bx - ax, by - ay
    denom = abx * abx + aby * aby
    t = 0.0 if denom == 0 else max(0.0, min(1.0, ((px - ax) * abx + (py - ay) * aby) / denom))
    return math.hypot(px - (ax + t * abx), py - (ay + t * aby)) - radius


def chevron_gt(px: float, py: float, cx: float, cy: float, span: float, radius: float) -> float:
    return min(
        sd_capsule(px, py, cx - span * 0.55, cy - span, cx + span * 0.7, cy, radius),
        sd_capsule(px, py, cx - span * 0.55, cy + span, cx + span * 0.7, cy, radius),
    )


def chevron_lt(px: float, py: float, cx: float, cy: float, span: float, radius: float) -> float:
    return min(
        sd_capsule(px, py, cx + span * 0.55, cy - span, cx - span * 0.7, cy, radius),
        sd_capsule(px, py, cx + span * 0.55, cy + span, cx - span * 0.7, cy, radius),
    )


def glyph(px: float, py: float) -> float:
    return min(
        chevron_gt(px, py, CHEVRON_LEFT, MID_Y, CHEVRON_SPAN, CHEVRON_RADIUS),
        sd_round_box(px, py, *CURSOR, CURSOR_RADIUS),
        chevron_lt(px, py, CHEVRON_RIGHT, MID_Y, CHEVRON_SPAN, CHEVRON_RADIUS),
    )


def coverage(signed_distance: float) -> float:
    if signed_distance >= 0.5:
        return 0.0
    if signed_distance <= -0.5:
        return 1.0
    return 0.5 - signed_distance


def lerp(a: tuple[int, int, int], b: tuple[int, int, int], t: float) -> tuple[float, float, float]:
    t = 0.0 if t < 0.0 else 1.0 if t > 1.0 else t
    return (
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    )


def over(pixel: list[float], red: float, green: float, blue: float, alpha: float) -> None:
    if alpha <= 0.0:
        return
    inverse = 1.0 - alpha
    pixel[0] = red * alpha + pixel[0] * inverse
    pixel[1] = green * alpha + pixel[1] * inverse
    pixel[2] = blue * alpha + pixel[2] * inverse
    pixel[3] = alpha + pixel[3] * inverse


def render(size: int) -> bytes:
    scale = size / VIEW
    pixels = [[[0.0, 0.0, 0.0, 0.0] for _ in range(size)] for _ in range(size)]
    for y in range(size):
        py = (y + 0.5) / scale
        t = (py - 8) / 240
        fill = lerp(BG_TOP, BG_BOT, t)
        row = pixels[y]
        for x in range(size):
            px = (x + 0.5) / scale
            signed = sd_round_box(px, py, 8, 8, 248, 248, 24)
            alpha = coverage(signed * scale)
            if alpha:
                over(row[x], fill[0], fill[1], fill[2], alpha)
            edge = coverage((abs(signed) - 4) * scale)
            if edge:
                over(row[x], float(STROKE[0]), float(STROKE[1]), float(STROKE[2]), edge)
            mark = coverage(glyph(px, py) * scale)
            if mark:
                over(row[x], float(CYAN[0]), float(CYAN[1]), float(CYAN[2]), mark)
    out = bytearray(size * size * 4)
    i = 0
    for row in pixels:
        for red, green, blue, alpha in row:
            out[i] = int(red + 0.5)
            out[i + 1] = int(green + 0.5)
            out[i + 2] = int(blue + 0.5)
            out[i + 3] = int(alpha * 255.0 + 0.5)
            i += 4
    return bytes(out)


def png_bytes(width: int, height: int, rgba: bytes) -> bytes:
    def chunk(tag: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", crc)

    raw = b"".join(b"\x00" + rgba[y * width * 4 : (y + 1) * width * 4] for y in range(height))
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico_bytes(images: list[tuple[int, bytes]]) -> bytes:
    count = len(images)
    offset = 6 + 16 * count
    header = struct.pack("<HHH", 0, 1, count)
    entries = bytearray()
    payload = bytearray()
    for size, data in images:
        stored = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", stored, stored, 0, 0, 1, 32, len(data), offset)
        payload += data
        offset += len(data)
    return bytes(header + entries + payload)


def main() -> None:
    macos = ROOT / "etc" / "macos"
    windows = ROOT / "etc" / "windows"
    macos.mkdir(parents=True, exist_ok=True)
    windows.mkdir(parents=True, exist_ok=True)
    (ROOT / "etc" / "starcom.svg").write_text(svg_text(), encoding="utf-8")
    # cargo-bundle's ICNS writer maps density-1 PNGs to 16/32/64/128/256/512.
    # 1024 px is 512@2x; the @2x filename is what sets density so the type
    # matches. icon.png stays 256 for the winit window icon.
    for size in (16, 32, 64, 128, 256, 512):
        data = png_bytes(size, size, render(size))
        (macos / f"icon_{size}.png").write_bytes(data)
        if size == 256:
            (macos / "icon.png").write_bytes(data)
    png1024 = png_bytes(1024, 1024, render(1024))
    (macos / "icon_512@2x.png").write_bytes(png1024)
    ico_images = [(size, png_bytes(size, size, render(size))) for size in (16, 24, 32, 48, 64, 128, 256)]
    (windows / "icon.ico").write_bytes(ico_bytes(ico_images))
    print(
        "wrote etc/starcom.svg, etc/macos/icon.png, etc/macos/icon_{16,32,64,128,256,512}.png, "
        "etc/macos/icon_512@2x.png, etc/windows/icon.ico"
    )


if __name__ == "__main__":
    main()

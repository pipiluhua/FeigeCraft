#!/usr/bin/env python3
"""生成 FeigeCraft 的应用图标（纸飞机 + 蓝色渐变）。

产物：
  crates/gui/assets/icon.ico        多尺寸图标，供 Windows 资源 / 任务栏使用
  crates/gui/assets/icon-256.png    单张 256px PNG，供 eframe 窗口图标使用

不依赖 Pillow：PNG 用 zlib 手写，多边形用 4x4 超采样光栅化。
绘制逻辑与界面里的 `widgets::paint_brand` 是同一套几何，保证窗口内外一致。
"""

import math
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "crates" / "gui" / "assets"

# 品牌色（与 theme::accent 同源）
C_LOW = (29, 78, 216)     # #1D4ED8 左下
C_HIGH = (96, 165, 250)   # #60A5FA 右上


def lerp(a, b, t):
    return a + (b - a) * t


def mix(c1, c2, t):
    return (lerp(c1[0], c2[0], t), lerp(c1[1], c2[1], t), lerp(c1[2], c2[2], t))


def in_round_rect(x, y, r):
    cx = min(max(x, r), 1.0 - r)
    cy = min(max(y, r), 1.0 - r)
    dx, dy = x - cx, y - cy
    return dx * dx + dy * dy <= r * r


def sign(ax, ay, bx, by, cx, cy):
    return (ax - cx) * (by - cy) - (bx - cx) * (ay - cy)


def in_tri(p, a, b, c):
    d1 = sign(p[0], p[1], a[0], a[1], b[0], b[1])
    d2 = sign(p[0], p[1], b[0], b[1], c[0], c[1])
    d3 = sign(p[0], p[1], c[0], c[1], a[0], a[1])
    neg = d1 < 0 or d2 < 0 or d3 < 0
    pos = d1 > 0 or d2 > 0 or d3 > 0
    return not (neg and pos)


# 纸飞机：与 widgets::paint_brand 相同的四个点
NOSE = (0.815, 0.205)
TAIL = (0.175, 0.505)
BELLY = (0.575, 0.800)
CREASE = (0.450, 0.545)

# 折面：同一条机身，靠左侧的一小块用浅蓝白，做出折叠感
SHADOW = (0.0, 0.016)


def sample(x, y):
    """返回该采样点的 (r, g, b, a)，a 为 0 或 1。"""
    if not in_round_rect(x, y, 0.225):
        return (0, 0, 0, 0)

    # 对角渐变 + 左上柔光，避免大色块显得平
    t = x * 0.62 + (1.0 - y) * 0.38
    color = mix(C_LOW, C_HIGH, t)
    glow = max(0.0, 1.0 - math.hypot(x - 0.18, y - 0.10) / 1.05) * 0.16
    color = mix(color, (255, 255, 255), glow)

    # 先画阴影，再画机身，最后画折面
    p = (x, y)
    shadow_p = (x - SHADOW[0], y - SHADOW[1])
    if in_tri(shadow_p, TAIL, NOSE, BELLY):
        color = mix(color, (8, 20, 48), 0.22)
    if in_tri(p, TAIL, NOSE, BELLY):
        color = (255, 255, 255)
    if in_tri((x, y), TAIL, NOSE, CREASE):
        color = mix((255, 255, 255), (198, 219, 255), 0.55)
    return (color[0], color[1], color[2], 1.0)


def render(size, ss=4):
    """超采样渲染，返回 RGBA 行列表。"""
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            r = g = b = 0.0
            cover = 0
            for sy in range(ss):
                for sx in range(ss):
                    x = (px + (sx + 0.5) / ss) / size
                    y = (py + (sy + 0.5) / ss) / size
                    sr, sg, sb, sa = sample(x, y)
                    if sa > 0:
                        r += sr
                        g += sg
                        b += sb
                        cover += 1
            if cover == 0:
                row += bytes((0, 0, 0, 0))
            else:
                alpha = int(round(255 * cover / (ss * ss)))
                row += bytes(
                    (
                        int(round(r / cover)),
                        int(round(g / cover)),
                        int(round(b / cover)),
                        alpha,
                    )
                )
        rows.append(bytes(row))
    return rows


def png_bytes(size, rows):
    def chunk(tag, payload):
        return (
            struct.pack(">I", len(payload))
            + tag
            + payload
            + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)
        )

    raw = b"".join(b"\x00" + row for row in rows)
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def main():
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    sizes = [16, 24, 32, 48, 64, 128, 256]
    images = []
    for size in sizes:
        # 小尺寸用 8x 超采样补足细节，大尺寸 4x 足够
        rows = render(size, ss=8 if size <= 32 else 4)
        images.append((size, png_bytes(size, rows)))
        if size == 256:
            (OUT_DIR / "icon-256.png").write_bytes(images[-1][1])

    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = b""
    for size, data in images:
        entries += struct.pack(
            "<BBBBHHII",
            size % 256,
            size % 256,
            0,
            0,
            1,
            32,
            len(data),
            offset,
        )
        offset += len(data)
    (OUT_DIR / "icon.ico").write_bytes(header + entries + b"".join(d for _, d in images))
    print("wrote", OUT_DIR / "icon.ico", (OUT_DIR / "icon.ico").stat().st_size, "bytes")
    print("wrote", OUT_DIR / "icon-256.png", (OUT_DIR / "icon-256.png").stat().st_size, "bytes")


if __name__ == "__main__":
    main()

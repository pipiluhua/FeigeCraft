"""把 capture.rs 存出的 24 位 BMP 转成 PNG（纯标准库，方便肉眼复核截图）。"""

import struct
import sys
import zlib


def read_bmp(path):
    data = open(path, "rb").read()
    assert data[:2] == b"BM", "不是 BMP"
    offset = struct.unpack_from("<I", data, 10)[0]
    width, height = struct.unpack_from("<ii", data, 18)
    bpp = struct.unpack_from("<H", data, 28)[0]
    assert bpp == 24, f"只支持 24 位，当前 {bpp}"
    row = (width * 3 + 3) & ~3
    rows = []
    for y in range(abs(height)):
        start = offset + y * row
        line = data[start : start + width * 3]
        rows.append(
            b"".join(
                line[i + 2 : i + 3] + line[i + 1 : i + 2] + line[i : i + 1]
                for i in range(0, len(line), 3)
            )
        )
    if height > 0:
        rows.reverse()
    return width, abs(height), b"".join(rows)


def write_png(path, width, height, rgb):
    raw = b"".join(b"\x00" + rgb[y * width * 3 : (y + 1) * width * 3] for y in range(height))

    def chunk(tag, payload):
        body = tag + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")
    open(path, "wb").write(png)


if __name__ == "__main__":
    for source in sys.argv[1:]:
        target = source.rsplit(".", 1)[0] + ".png"
        write_png(target, *read_bmp(source))
        print(target)

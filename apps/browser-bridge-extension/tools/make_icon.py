import struct
import zlib
from pathlib import Path


def chunk(tag: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def write_png(path: Path, w: int, h: int, rgba_pixels: list[list[tuple[int, int, int, int]]]) -> None:
    raw = b"".join(
        b"\x00" + bytes([c for px in row for c in px]) for row in rgba_pixels
    )
    comp = zlib.compress(raw, 9)
    png = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    png += chunk(b"IHDR", ihdr)
    png += chunk(b"IDAT", comp)
    png += chunk(b"IEND", b"")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(png)


def main() -> None:
    w = h = 128
    bg = (0x7C, 0x3A, 0xED, 0xFF)  # Nexus purple
    white = (0xFF, 0xFF, 0xFF, 0xFF)

    img = [[bg for _ in range(w)] for _ in range(h)]

    def rect(x0: int, y0: int, x1: int, y1: int, color: tuple[int, int, int, int]) -> None:
        for y in range(max(0, y0), min(h, y1)):
            row = img[y]
            for x in range(max(0, x0), min(w, x1)):
                row[x] = color

    # Draw a stylized "N" without font dependencies
    rect(28, 24, 44, 104, white)   # left bar
    rect(84, 24, 100, 104, white)  # right bar
    for i in range(0, 60):         # diagonal (thick)
        rect(44 + i, 24 + i, 44 + i + 8, 24 + i + 8, white)

    out = Path(__file__).resolve().parents[1] / "icon128.png"
    write_png(out, w, h, img)
    print(f"wrote {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()


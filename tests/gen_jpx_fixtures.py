#!/usr/bin/env python3
"""Generate JPEG 2000 test fixtures with glymur (independent OpenJPEG
encoder) plus reference decodes for pixel-parity tests.

Outputs into tests/fixtures/:
  jpx_gray_64.j2k / jpx_gray_64.raw    — 64x64 8-bit gray (raw = rows, MSB..)
  jpx_rgb_64.j2k  / jpx_rgb_64.raw     — 64x64 8-bit RGB
  jpx_gray_tiles.j2k / jpx_gray_tiles.raw — 128x64 gray, two 64x64 tiles

Requires: pip install glymur numpy (with system openjp2).
"""

import os
import sys

import numpy as np

try:
    import glymur  # noqa: F401  (openjp2 is auto-located)
except Exception as e:  # pragma: no cover
    sys.exit(f"glymur/openjp2 unavailable: {e}")

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.join(HERE, "fixtures")
os.makedirs(FIX, exist_ok=True)


def write_case(name, arr, numres=3, tiles=None):
    """Write a raw-codestream (.j2k) fixture + reference decode."""
    path = os.path.join(FIX, name + ".j2k")
    if os.path.exists(path):
        os.unlink(path)
    j = glymur.Jp2k(path, data=arr, numres=numres, tilesize=tiles or arr.shape[:2])
    # re-read for the reference decode (independent roundtrip through openjp2)
    back = j[:]
    if back.dtype != arr.dtype or back.shape != arr.shape:
        back = np.asarray(back).reshape(arr.shape)
    raw_path = os.path.join(FIX, name + ".raw")
    back.tofile(raw_path)
    print(f"{name}: {arr.shape} {arr.dtype} -> {path} ({os.path.getsize(path)} B),"
          f" ref {raw_path} ({os.path.getsize(raw_path)} B)")


def main():
    rng = np.random.default_rng(20261111)

    gray = rng.integers(0, 256, size=(64, 64), dtype=np.uint8)
    write_case("jpx_gray_64", gray)

    rgb = rng.integers(0, 256, size=(64, 64, 3), dtype=np.uint8)
    write_case("jpx_rgb_64", rgb)

    tiled = rng.integers(0, 256, size=(128, 64), dtype=np.uint8)
    write_case("jpx_gray_tiles", tiled, tiles=(64, 64))


if __name__ == "__main__":
    main()

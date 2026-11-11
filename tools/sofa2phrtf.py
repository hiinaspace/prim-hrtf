#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "scipy", "h5py"]
# ///
"""Bake a SimpleFreeFieldHRIR .sofa file into Prim's compact .phrtf format.

    uv run tools/sofa2phrtf.py IN.sofa OUT.phrtf [--rate 48000] [--steam-inl cipic_124.inl]

What the bake does, in order (mirrors what libmysofa's mysofa_open_no_norm hands Mumble,
minus the loudness normalisation, which Steam Audio also skips):
  1. read Data.IR (M x 2 x N), Data.SamplingRate, SourcePosition, Data.Delay
  2. apply integer Data.Delay by prepending zeros (CIPIC has none)
  3. resample to --rate with a polyphase Kaiser filter (scipy resample_poly)
  4. convert positions to unit vectors, SOFA frame: +x front, +y left, +z up
  5. precompute libmysofa's 6-neighbour table (az+-, el+-, radius+-) with its exact
     0.5-degree stepping search, so the runtime needs no kd-tree
  6. quantise IRs to int16 with one global scale

--steam-inl compares the result with Steam Audio's embedded 48 kHz table for the
same dataset (core/src/core/cipic_124.inl) and prints the per-direction error.

Format (little-endian), version 1:
  0   8   magic b"PHRTF\\0\\0\\1"
  8   4   u32 sample rate
  12  4   u32 taps N
  16  4   u32 directions M
  20  4   f32 int16 -> float scale
  24  32  sha256 of the source .sofa
  56  M*12      f32[3] unit direction per measurement (SOFA frame)
  ..  M*24      i32[6] neighbour indices, -1 for none
  ..  M*2*N*2   i16 IR, per direction: left N taps then right N taps
"""
import argparse
import hashlib
import math
from pathlib import Path
import re
import struct
import sys

import h5py
import numpy as np
from scipy.signal import resample_poly
from scipy.spatial import cKDTree

MAGIC = b'PHRTF\0\0\1'
STEP = 0.5          # MYSOFA_DEFAULT_NEIGH_STEP_ANGLE
MAX_SEARCH = 45.0   # maxNeighborSearchAngle in neighbors.c


def sph_to_cart(az_deg, el_deg, r):
    az, el = np.radians(az_deg), np.radians(el_deg)
    return np.stack([r * np.cos(el) * np.cos(az), r * np.cos(el) * np.sin(az), r * np.sin(el)], -1)


def cart_to_sph(v):
    x, y, z = v
    return (math.degrees(math.atan2(y, x)), math.degrees(math.atan2(z, math.hypot(x, y))),
            math.sqrt(x * x + y * y + z * z))


def neighbours(points):
    """Port of mysofa_neighborhood_init_withstepdefine for a single-radius set."""
    tree = cKDTree(points)
    out = np.full((len(points), 6), -1, dtype=np.int32)
    for i, p in enumerate(points):
        phi, theta, r = cart_to_sph(p)
        for slot, axis, sign in ((0, 0, 1), (1, 0, -1), (2, 1, 1), (3, 1, -1)):
            step = STEP
            while step <= MAX_SEARCH:
                a = phi + sign * step if axis == 0 else phi
                t = theta + sign * step if axis == 1 else theta
                index = int(tree.query(sph_to_cart(a, t, r))[1])
                if index != i:
                    out[i, slot] = index
                    break
                step += STEP
        # radius neighbours (slots 4, 5) stay -1: Prim bakes one radius.
    return out


def load_sofa(path):
    with h5py.File(path, 'r') as f:
        ir = np.asarray(f['Data.IR'], dtype=np.float64)
        rate = float(np.asarray(f['Data.SamplingRate']).ravel()[0])
        pos = np.asarray(f['SourcePosition'], dtype=np.float64)
        pos_type = f['SourcePosition'].attrs.get('Type', b'spherical')
        delay = np.asarray(f['Data.Delay'], dtype=np.float64) if 'Data.Delay' in f else np.zeros((1, 2))
    if isinstance(pos_type, bytes):
        pos_type = pos_type.decode()
    if ir.ndim != 3 or ir.shape[1] != 2:
        sys.exit(f'expected Data.IR M x 2 x N, got {ir.shape}')
    if str(pos_type).lower().startswith('cart'):
        unit = pos / np.linalg.norm(pos, axis=1, keepdims=True)
        radii = np.linalg.norm(pos, axis=1)
    else:
        unit = sph_to_cart(pos[:, 0], pos[:, 1], 1.0)
        radii = pos[:, 2]
    if np.ptp(radii) > 1e-3:
        sys.exit('multi-radius SOFA files are not supported (Prim bakes one radius)')
    return ir, rate, unit, delay


def apply_delay(ir, delay):
    delay = np.broadcast_to(delay, (ir.shape[0], 2))
    if not np.any(delay):
        return ir
    pad = int(np.max(np.round(delay)))
    out = np.zeros((ir.shape[0], 2, ir.shape[2] + pad))
    for m in range(ir.shape[0]):
        for e in range(2):
            d = int(round(delay[m, e]))
            out[m, e, d:d + ir.shape[2]] = ir[m, e]
    return out


def resample(ir, rate, target):
    if rate == target:
        return ir
    g = math.gcd(int(rate), int(target))
    up, down = int(target) // g, int(rate) // g
    return resample_poly(ir, up, down, axis=2)


def parse_steam_inl(path, rate):
    text = Path(path).read_text()
    body = text[text.index('{') + 1:text.rindex('}')]
    raw = np.array([int(t) for t in re.findall(r'\d+', body)], dtype=np.uint8).tobytes()
    o = 0
    magic, version, m = struct.unpack_from('<4sii', raw, o)
    o += 12 + m * 8
    (n_rates,) = struct.unpack_from('<i', raw, o)
    o += 4
    rates = [struct.unpack_from('<ii', raw, o + 8 * k) for k in range(n_rates)]
    o += 8 * n_rates
    for r, n in rates:
        size = m * 2 * n
        if r == rate:
            # Stored ear-major (every left IR, then every right IR), despite the
            # per-direction layout described in hrtf_map.cpp's header comment.
            data = np.frombuffer(raw, dtype='<f4', count=size, offset=o).reshape(2, m, n)
            return data.transpose(1, 0, 2)
        o += size * 4
    sys.exit(f'Steam table has no {rate} Hz data (has {[r for r, _ in rates]})')


def compare_steam(ir, steam):
    n = min(ir.shape[2], steam.shape[2])
    a, b = ir[:, :, :n], steam[:, :, :n]
    err = np.sqrt(((a - b) ** 2).sum(-1) / np.maximum((b ** 2).sum(-1), 1e-20))
    err_db = 20 * np.log10(np.maximum(err, 1e-12))
    gain = float(np.sqrt((a ** 2).sum() / (b ** 2).sum()))
    print(f'steam table: {steam.shape}, ours {ir.shape}; overall gain ours/steam {20*math.log10(gain):+.2f} dB')
    print(f'per-IR relative error: median {np.median(err_db):.1f} dB, worst {err_db.max():.1f} dB')


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('sofa')
    ap.add_argument('out')
    ap.add_argument('--rate', type=int, default=48000)
    ap.add_argument('--steam-inl')
    args = ap.parse_args()
    src = Path(args.sofa).read_bytes()
    ir, rate, unit, delay = load_sofa(args.sofa)
    ir = resample(apply_delay(ir, delay), rate, args.rate)
    if args.steam_inl:
        compare_steam(ir, parse_steam_inl(args.steam_inl, args.rate))
    nb = neighbours(unit)
    scale = float(np.abs(ir).max()) / 32767.0
    q = np.clip(np.round(ir / scale), -32768, 32767).astype('<i2')
    qerr = np.sqrt(((q * scale - ir) ** 2).sum() / (ir ** 2).sum())
    m, _, n = ir.shape
    with open(args.out, 'wb') as f:
        f.write(MAGIC)
        f.write(struct.pack('<IIIf', args.rate, n, m, scale))
        f.write(hashlib.sha256(src).digest())
        f.write(unit.astype('<f4').tobytes())
        f.write(nb.astype('<i4').tobytes())
        f.write(q.tobytes())
    size = Path(args.out).stat().st_size
    print(f'wrote {args.out}: {m} directions x {n} taps @ {args.rate} Hz, {size/1024:.0f} KiB, '
          f'int16 error {20*math.log10(qerr):.1f} dB, missing neighbours {(nb[:, :4] < 0).sum()}')


if __name__ == '__main__':
    main()

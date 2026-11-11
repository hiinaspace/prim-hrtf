#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "scipy", "h5py"]
# ///
"""Bake a SimpleFreeFieldHRIR .sofa file into Prim's compact .phrtf format.

    uv run tools/sofa2phrtf.py IN.sofa OUT.phrtf [--rate 48000] [--steam-inl cipic_124.inl]

The runtime interpolates the way Steam Audio does (hrtf_map.cpp, hrtf_database.cpp):
bilinear weights on the measurement grid in interaural coordinates, then a blend of
log magnitude and unwrapped phase. So the bake keeps the grid, not neighbour tables:
  1. read Data.IR (M x 2 x N), Data.SamplingRate, SourcePosition, Data.Delay
  2. apply integer Data.Delay by prepending zeros (CIPIC has none)
  3. resample to --rate with a polyphase Kaiser filter (scipy resample_poly)
  4. convert each position to Steam's interaural coordinates, rounded to whole
     degrees as HRTFMap::loadDirections does: lateral = asin(x) in [-90, 90]
     (+ right), elevation = (180 + atan2(z, y)) mod 360 measured from straight down
     (front 90, up 180, back 270), in Steam's frame (+x right, +y up, +z back)
  5. check the grid is ring-major (each run of P measurements shares one lateral
     angle, laterals ascending, elevations ascending within a ring), which
     Steam's lookup assumes without checking
  6. quantise IRs to int16 with one global scale

--steam-inl compares the IRs and the derived grid with Steam Audio's embedded table
for the same dataset (core/src/core/cipic_124.inl).

Format (little-endian), version 2:
  0   8   magic b"PHRTF\\0\\0\\2"
  8   4   u32 header length in bytes (64); readers skip anything past what they know
  12  4   u32 sample rate
  16  4   u32 taps N
  20  4   u32 lateral rings L
  24  4   u32 measurements per ring P (M = L * P)
  28  4   f32 int16 -> float scale
  32  32  sha256 of the source .sofa
  64  L*4       f32 lateral angle per ring, degrees, ascending
  ..  M*4       f32 interaural elevation per measurement, degrees, ring-major
  ..  M*2*N*2   i16 IR, per measurement: left N taps then right N taps
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

MAGIC = b'PHRTF\0\0\2'
HEADER = 64


def sph_to_cart(az_deg, el_deg, r):
    az, el = np.radians(az_deg), np.radians(el_deg)
    return np.stack([r * np.cos(el) * np.cos(az), r * np.cos(el) * np.sin(az), r * np.sin(el)], -1)


def round_half_away(x):
    """C roundf: halves round away from zero (Python's round() is banker's)."""
    return np.sign(x) * np.floor(np.abs(x) + 0.5)


def interaural_grid(unit):
    """Steam's InterauralSphericalVector3f of SOFA-frame unit vectors, in whole degrees,
    computed in float32 like Steam (halves of the 5.625-degree CIPIC polar step land on
    .5 exactly, so the precision decides the rounding)."""
    s = unit.astype(np.float32)
    x, y, z = -s[:, 1], s[:, 2], -s[:, 0]  # Steam canonical: +x right, +y up, +z back
    lat = np.arcsin(np.clip(x, -1, 1)).astype(np.float32)
    elev = np.fmod(np.float32(math.pi) + np.arctan2(z, y), np.float32(2 * math.pi)).astype(np.float32)
    pole = (np.abs(lat - np.float32(math.pi / 2)) < 1e-5) | (np.abs(lat + np.float32(math.pi / 2)) < 1e-5)
    elev[pole] = 0
    to_deg = np.float32(math.pi / 180)
    return round_half_away(lat / to_deg), round_half_away(elev / to_deg)


def check_grid(lat, elev):
    rings = np.unique(lat)
    m = len(lat)
    if m % len(rings):
        sys.exit(f'{m} measurements do not split into {len(rings)} equal lateral rings')
    p = m // len(rings)
    lat_r, elev_r = lat.reshape(len(rings), p), elev.reshape(len(rings), p)
    if not np.all(lat_r == rings[:, None]):
        sys.exit('measurements are not ring-major by ascending lateral angle')
    if not np.all(np.diff(elev_r, axis=1) > 0):
        sys.exit('elevations are not strictly ascending within each ring')
    return rings, p


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
    o += 12
    dirs = np.frombuffer(raw, dtype='<f4', count=2 * m, offset=o).reshape(m, 2)
    o += m * 8
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
            return data.transpose(1, 0, 2), dirs
        o += size * 4
    sys.exit(f'Steam table has no {rate} Hz data (has {[r for r, _ in rates]})')


def steam_grid(dirs):
    """Steam's own grid: canonical (elevation, azimuth) degrees -> interaural, float32."""
    d = np.radians(dirs.astype(np.float64)).astype(np.float32)
    el, az = d[:, 0], d[:, 1]
    x = np.cos(el) * -np.sin(az)
    y = np.sin(el)
    z = np.cos(el) * -np.cos(az)
    sofa = np.stack([-z, -x, y], -1)  # back to SOFA so interaural_grid applies
    return interaural_grid(sofa)


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
    lat, elev = interaural_grid(unit)
    rings, p = check_grid(lat, elev)
    if args.steam_inl:
        steam_ir, steam_dirs = parse_steam_inl(args.steam_inl, args.rate)
        compare_steam(ir, steam_ir)
        slat, selev = steam_grid(steam_dirs)
        print(f'grid vs steam: lateral mismatches {(slat != lat).sum()}, elevation mismatches {(selev != elev).sum()}')
    scale = float(np.abs(ir).max()) / 32767.0
    q = np.clip(np.round(ir / scale), -32768, 32767).astype('<i2')
    qerr = np.sqrt(((q * scale - ir) ** 2).sum() / (ir ** 2).sum())
    m, _, n = ir.shape
    with open(args.out, 'wb') as f:
        f.write(MAGIC)
        f.write(struct.pack('<IIIIIf', HEADER, args.rate, n, len(rings), p, scale))
        f.write(hashlib.sha256(src).digest())
        f.write(rings.astype('<f4').tobytes())
        f.write(elev.astype('<f4').tobytes())
        f.write(q.tobytes())
    size = Path(args.out).stat().st_size
    print(f'wrote {args.out}: {len(rings)} rings x {p} = {m} measurements x {n} taps @ {args.rate} Hz, '
          f'{size/1024:.0f} KiB, int16 error {20*math.log10(qerr):.1f} dB')


if __name__ == '__main__':
    main()

#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "scipy"]
# ///
"""R2a renderer bench: today's Godot Steam Audio voice path vs prim-hrtf vs Mumble.

    nix develop -c uv run bench/run_bench.py [--run DIR] [--godot PATH] [--steam-audio STORE_PATH]

Stages (each skipped when its outputs already exist in the run directory):
  1. inputs: seeded white noise and LibriSpeech clips (tools/fetch_speech.py)
  2. Godot capture: headless Godot 4.7 + patched godot-steam-audio, avatar.gd voice
     settings, master bus recorded via AudioEffectCapture into a private null sink
  3. prim-hrtf renders (target/release/prim-hrtf-bench)
  4. Mumble renders (.cache/mumble-harness, if built)
  5. analysis: ITD/ILD per azimuth, band timbre vs Godot, level -> results.json
  6. blind A/B packet (Godot vs prim-hrtf) -> packets/<name>/ with the key kept
     in packets/<name>-key.json, outside the packet

Bulk renders live in .cache/bench/<run>/ (CACHEDIR.TAG); results and the packet do not.
"""
import argparse
import json
import os
from pathlib import Path
import random
import shutil
import subprocess
import time
import wave

import numpy as np
from scipy.signal import butter, correlate, resample_poly, sosfiltfilt

ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / '.cache'
RATE = 48000
DISTANCE = 1.5                    # table seat distance, metres
GAIN = 1.5                        # prim voice_gain(1.5 m) at defaults: 150 %, inside 3 m
MEASURE_AZ = [0, 45, 90, 135, 180, -135, -90, -45]
# Through the interaural poles, where CIPIC has no measurement (last ring is 80 deg).
SWEEP_AZ = [a * s for s in (1, -1) for a in (70, 75, 80, 85, 88, 92, 95, 100, 105, 110)]
SEATS = [0, 45, -45, 90, -90, 180]
TABLES = {'table-front': [-90, -45, 45, 90], 'table-around': [0, 90, 180, -90]}
STEAM_AUDIO = '/nix/store/0zlxvazq99j2pri42chb4094r6fj2rfx-godot-steam-audio-8f65c29-sdk-4.8.1'
GODOT = Path.home() / 'code/prim-identity/.local/godot/bin/godot'
BANDS = [(100, 200), (200, 400), (400, 800), (800, 1600), (1600, 3200), (3200, 6400),
         (6400, 10000), (10000, 16000)]


def ensure_cache():
    CACHE.mkdir(exist_ok=True)
    tag = CACHE / 'CACHEDIR.TAG'
    if not tag.exists():
        tag.write_text('Signature: 8a477f597d28d172789f06886806bc55\n'
                       '# prim-hrtf regenerable bench output\n')


def write_wav(path, data, rate=RATE):
    data = np.asarray(data, dtype=np.float64)
    pcm = np.clip(np.round(data * 32767), -32768, 32767).astype('<i2')
    with wave.open(str(path), 'wb') as w:
        w.setnchannels(1 if data.ndim == 1 else data.shape[1])
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(pcm.tobytes())


def side(az):
    if az == 0:
        return 'front'
    if abs(az) == 180:
        return 'behind'
    return f"{abs(az)} deg {'right' if az > 0 else 'left'}"


# ---------------------------------------------------------------- stage 1: inputs

def make_inputs(run):
    inp = run / 'inputs'
    if (inp / 'inputs.json').exists():
        return json.loads((inp / 'inputs.json').read_text())
    inp.mkdir(parents=True)
    rng = np.random.default_rng(42)
    noise = (rng.standard_normal(3 * RATE) * 0.05).astype(np.float32)
    clips = json.loads((CACHE / 'speech/sources.json').read_text())['clips']
    items = {'noise': noise}
    for c in clips:
        x = np.fromfile(CACHE / f"speech/{c['id']}-48k.f32", dtype='<f4').astype(np.float64)
        x = x / np.sqrt(np.mean(x ** 2)) * 10 ** (-26 / 20)     # -26 dBFS RMS
        items[c['id']] = x.astype(np.float32)
    meta = {}
    for name, x in items.items():
        x.astype('<f4').tofile(inp / f'{name}.f32')
        write_wav(inp / f'{name}.wav', x)
        meta[name] = {'f32': str(inp / f'{name}.f32'), 'wav': str(inp / f'{name}.wav'),
                      'seconds': len(x) / RATE}
    meta['_speech'] = [c['id'] for c in clips]
    (inp / 'inputs.json').write_text(json.dumps(meta, indent=2))
    return meta


def render_plan(inputs):
    speech = inputs['_speech']
    renders = [{'name': f'noise_{az}', 'voices': [{'input': 'noise', 'azimuth': az}]}
               for az in MEASURE_AZ + SWEEP_AZ]
    for k, az in enumerate(SEATS):
        renders.append({'name': f'speech_{az}', 'voices': [{'input': speech[k % len(speech)], 'azimuth': az}]})
    for name, seats in TABLES.items():
        renders.append({'name': name, 'voices': [{'input': speech[k], 'azimuth': az}
                                                 for k, az in enumerate(seats)]})
    return renders


# ---------------------------------------------------------------- stage 2: Godot

def godot_capture(run, inputs, renders, godot, steam_audio):
    out = run / 'godot'
    if (out / 'godot.json').exists():
        return json.loads((out / 'godot.json').read_text())
    proj = CACHE / 'godot-project'
    addon = proj / 'addons/godot-steam-audio'
    if proj.exists():
        subprocess.run(['chmod', '-R', 'u+w', str(proj)], check=True)  # store copies are r/o
        shutil.rmtree(proj)
    (addon / 'bin').mkdir(parents=True)
    for f in Path(steam_audio, 'bin').iterdir():
        shutil.copy(f, addon / 'bin' / f.name)
    shutil.copytree(Path(steam_audio, 'icons'), addon / 'icons')
    subprocess.run(['chmod', '-R', 'u+w', str(proj)], check=True)
    # Mirror prim-identity's [audio] settings (defaults apart from input).
    (proj / 'project.godot').write_text(
        'config_version=5\n\n[application]\nconfig/name="prim-hrtf-bench"\n\n'
        '[audio]\ndriver/enable_input=false\n')
    shutil.copy(ROOT / 'bench/godot/capture.gd', proj / 'capture.gd')
    out.mkdir(parents=True, exist_ok=True)
    plan = {'max_voices': max(len(r['voices']) for r in renders), 'renders': [
        {'name': r['name'], 'voices': [{'wav': inputs[v['input']]['wav'], 'azimuth': v['azimuth'],
                                        'distance': DISTANCE, 'gain': GAIN} for v in r['voices']]}
        for r in renders]}
    (out / 'plan.json').write_text(json.dumps(plan, indent=2))
    sink = f'prim_hrtf_bench_{os.getpid()}'
    module = subprocess.check_output(['pactl', 'load-module', 'module-null-sink', f'sink_name={sink}',
                                      'rate=48000', 'channels=2'], text=True).strip()
    try:
        env = os.environ | {'PULSE_SINK': sink, 'PRIM_HRTF_OUT': str(out),
                            'XDG_DATA_HOME': str(run / 'godot-profile')}
        with (out / 'run.log').open('w') as log:
            # First pass imports the extension; second runs the capture.
            subprocess.run([str(godot), '--headless', '--path', str(proj), '--import'],
                           env=env, stdout=log, stderr=subprocess.STDOUT, timeout=120)
            subprocess.run([str(godot), '--headless', '--audio-driver', 'PulseAudio', '--path', str(proj),
                            '--script', 'res://capture.gd'], env=env, stdout=log,
                           stderr=subprocess.STDOUT, timeout=300, check=True)
    finally:
        subprocess.run(['pactl', 'unload-module', module], check=True)
    if not (out / 'godot.json').exists():
        raise SystemExit(f'Godot capture failed; see {out / "run.log"}')
    return json.loads((out / 'godot.json').read_text())


# ---------------------------------------------------------------- stages 3-4: renders

def offline_renders(run, inputs, renders, kind):
    out = run / kind
    out.mkdir(exist_ok=True)
    hrtf = ROOT / 'data/cipic_124.phrtf'
    for r in renders:
        dest = out / f"{r['name']}.f32"
        if dest.exists():
            continue
        mix = None
        for k, v in enumerate(r['voices']):
            tmp = out / f"{r['name']}.{k}.tmp"
            src = inputs[v['input']]['f32']
            if kind == 'prim':
                cmd = [str(ROOT / 'target/release/prim-hrtf-bench'), 'render', str(hrtf), src, str(tmp),
                       str(v['azimuth']), str(DISTANCE)]
            else:
                cmd = [str(CACHE / 'mumble-harness/mumble-harness'), str(CACHE / 'cipic_124.sofa'),
                       src, str(tmp), str(v['azimuth']), str(GAIN)]
            subprocess.run(cmd, check=True, capture_output=True)
            x = np.fromfile(tmp, dtype='<f4').reshape(-1, 2)
            tmp.unlink()
            if mix is None:
                mix = x.astype(np.float64)
            else:
                n = max(len(mix), len(x))
                mix = np.pad(mix, ((0, n - len(mix)), (0, 0))) + np.pad(x, ((0, n - len(x)), (0, 0)))
        mix.astype('<f4').tofile(dest)


def load_stereo(path, rate):
    x = np.fromfile(path, dtype='<f4').reshape(-1, 2).astype(np.float64)
    if rate != RATE:
        x = resample_poly(x, 160, 147, axis=0) if rate == 44100 else resample_poly(x, RATE, int(rate), axis=0)
    return x


def align(ref, x, max_lag=RATE // 2):
    """Shift x so it lines up with ref (mono-sum cross-correlation); trim to ref length."""
    a, b = ref.sum(1), x.sum(1)
    n = min(len(a), len(b))
    c = correlate(b[:n], a[:n], mode='full', method='fft')
    mid = n - 1
    lo, hi = max(0, mid - max_lag), min(len(c), mid + max_lag + 1)
    lag = int(np.argmax(c[lo:hi])) + lo - mid
    if lag >= 0:
        y = x[lag:]
    else:
        y = np.pad(x, ((-lag, 0), (0, 0)))
    y = y[:len(ref)]
    return np.pad(y, ((0, len(ref) - len(y)), (0, 0))), lag


# ---------------------------------------------------------------- stage 5: analysis

LOW = butter(4, 1500, 'low', fs=RATE, output='sos')


def itd_us(x):
    """Interaural time difference, microseconds; positive = right ear leads."""
    l, r = sosfiltfilt(LOW, x[:, 0]), sosfiltfilt(LOW, x[:, 1])
    m = 48  # +-1 ms
    c = correlate(l, r, mode='full', method='fft')
    mid = len(r) - 1
    seg = c[mid - m:mid + m + 1]
    k = int(np.argmax(seg))
    frac = 0.0
    if 0 < k < len(seg) - 1:
        y0, y1, y2 = seg[k - 1], seg[k], seg[k + 1]
        frac = 0.5 * (y0 - y2) / (y0 - 2 * y1 + y2)
    return (k - m + frac) / RATE * 1e6


def band_power(x, bands=BANDS):
    n = 8192
    blocks = x[:len(x) // n * n].reshape(-1, n, x.shape[1])
    p = (np.abs(np.fft.rfft(blocks * np.hanning(n)[None, :, None], axis=1)) ** 2).mean(0)
    f = np.fft.rfftfreq(n, 1 / RATE)
    return np.array([p[(f >= lo) & (f < hi)].mean(0) for lo, hi in bands])  # bands x ears


def db(x):
    return 10 * np.log10(np.maximum(x, 1e-30))


def analyse(run, renders, godot_rate, paths):
    data = {p: {} for p in paths}
    lags = {}
    for r in renders:
        ref = load_stereo(run / 'prim' / f"{r['name']}.f32", RATE)
        for p in paths:
            if p == 'prim':
                data[p][r['name']] = ref
                continue
            f = run / p / f"{r['name']}.f32"
            if not f.exists():
                continue
            x, lag = align(ref, load_stereo(f, godot_rate if p == 'godot' else RATE))
            data[p][r['name']] = x
            lags[f'{p}/{r["name"]}'] = lag
    noise_in = np.fromfile(run / 'inputs/noise.f32', dtype='<f4').astype(np.float64)
    in_band = band_power(noise_in[:, None])[:, 0]
    results = {'azimuths': {}, 'alignment_lag_samples': lags}
    for az in MEASURE_AZ + SWEEP_AZ:
        row = {}
        for p in paths:
            x = data[p].get(f'noise_{az}')
            if x is None:
                continue
            x = x[RATE // 4: -RATE // 4]   # skip onset/tail
            bp = band_power(x)
            ild_bands = {f'{lo}-{hi}': float(db(bp[i, 1]) - db(bp[i, 0]))
                         for i, (lo, hi) in enumerate(BANDS) if lo >= 400}
            row[p] = {
                'itd_us': round(itd_us(x), 1),
                'ild_db': round(float(db(np.mean(x[:, 1] ** 2)) - db(np.mean(x[:, 0] ** 2))), 2),
                'ild_bands_db': {k: round(v, 2) for k, v in ild_bands.items()},
                'level_db': round(float(db(np.mean(x ** 2)) - db(np.mean(noise_in ** 2))), 2),
                'band_gain_db': [round(float(v), 2) for v in db(bp.mean(1)) - db(in_band)],
            }
        if 'godot' in row:
            for p in row:
                if p == 'godot':
                    continue
                diff = np.array(row[p]['band_gain_db']) - np.array(row['godot']['band_gain_db'])
                row[p]['vs_godot'] = {
                    'level_db': round(row[p]['level_db'] - row['godot']['level_db'], 2),
                    'timbre_max_abs_db': round(float(np.abs(diff - diff.mean()).max()), 2),
                    'itd_us': round(row[p]['itd_us'] - row['godot']['itd_us'], 1),
                    'ild_db': round(row[p]['ild_db'] - row['godot']['ild_db'], 2),
                }
        results['azimuths'][str(az)] = row
    return results, data


# ---------------------------------------------------------------- stage 6: packet

def packet(name, renders, data, results):
    pdir = ROOT / 'packets' / name
    key_path = ROOT / 'packets' / f'{name}-key.json'
    if pdir.exists():
        shutil.rmtree(pdir)
    pdir.mkdir(parents=True)
    rng = random.Random(name)
    items = [r for r in renders if not r['name'].startswith('noise_')]
    key = {}
    lines = []
    for k, r in enumerate(items, 1):
        g, p = data['godot'][r['name']], data['prim'][r['name']]
        # Level-match prim to Godot (today's level), so the comparison is placement
        # and timbre, not loudness. The raw difference is in results.json.
        p = p * np.sqrt(np.mean(g ** 2) / np.mean(p ** 2))
        peak = max(np.abs(g).max(), np.abs(p).max())
        scale = min(1.0, 0.89 / peak)
        order = ['godot', 'prim'] if rng.random() < 0.5 else ['prim', 'godot']
        for label, which in zip('AB', order):
            write_wav(pdir / f'{k:02d}-{label}.wav', (g if which == 'godot' else p) * scale)
        key[f'{k:02d}'] = {'A': order[0], 'B': order[1], 'render': r['name']}
        if len(r['voices']) == 1:
            where = side(r['voices'][0]['azimuth'])
            desc = f'one voice, {where}'
        else:
            where = ', '.join(side(v['azimuth']) for v in r['voices'])
            desc = f'{len(r["voices"])} voices at once: {where}'
        lines.append(f'{k:02d}  {desc}')
    (pdir / 'README.txt').write_text(
        'Prim R2a renderer listening packet (blind)\n\n'
        'Each numbered pair is the same speech placed at the same table seat, 1.5 m away,\n'
        'rendered two ways. One is today\'s Prim voice path (Godot + Steam Audio), the other\n'
        'is the new prim-hrtf renderer. Which is which is randomised per pair and kept in a\n'
        'separate key file. The pairs are level-matched.\n\n'
        'Listen on headphones. Seats are relative to you facing forward.\n\n'
        + '\n'.join(lines) + '\n\n'
        'Speech: LibriSpeech test-clean (CC BY 4.0), Panayotov, Chen, Povey, Khudanpur;\n'
        'recordings from LibriVox.\n')
    key_path.write_text(json.dumps(key, indent=2) + '\n')
    return pdir, key_path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', help='reuse an existing run directory')
    ap.add_argument('--godot', default=str(GODOT))
    ap.add_argument('--steam-audio', default=STEAM_AUDIO)
    ap.add_argument('--packet', default=time.strftime('r2a-%Y%m%d'))
    args = ap.parse_args()
    ensure_cache()
    run = (Path(args.run) if args.run else CACHE / 'bench' / time.strftime('%Y%m%d-%H%M%S')).resolve()
    run.mkdir(parents=True, exist_ok=True)
    inputs = make_inputs(run)
    renders = render_plan(inputs)
    meta = godot_capture(run, inputs, renders, args.godot, args.steam_audio)
    offline_renders(run, inputs, renders, 'prim')
    paths = ['godot', 'prim']
    if (CACHE / 'mumble-harness/mumble-harness').exists():
        offline_renders(run, inputs, renders, 'mumble')
        paths.append('mumble')
    results, data = analyse(run, renders, meta['rate'], paths)
    results['godot'] = meta
    results['run'] = str(run)
    results['setup'] = {'distance_m': DISTANCE, 'gain': GAIN, 'rate': RATE,
                        'steam_audio': args.steam_audio}
    (run / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    pdir, key = packet(args.packet, renders, data, results)
    print(json.dumps({'run': str(run), 'packet': str(pdir), 'key': str(key)}))


if __name__ == '__main__':
    main()

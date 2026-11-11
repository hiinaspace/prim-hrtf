#!/usr/bin/env python3
"""Fetch LibriSpeech test-clean speech clips (CC BY 4.0) for the renderer bench.

Clip 0 is the same clip Prim's viseme tests use (tools/fetch-viseme-fixture.py in
prim-identity, same member and sha256). The others are the first clip between 4 and
9 seconds from a few more speakers, chosen deterministically by archive order, so the
listening packet has more than one voice. Streams the archive and stops when done.

Output: .cache/speech/<id>.flac, <id>-48k.f32 (mono float32 48 kHz) and sources.json.
Run: uv run tools/fetch_speech.py
"""
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
URL = 'https://www.openslr.org/resources/12/test-clean.tar.gz'
VISEME_MEMBER = 'LibriSpeech/test-clean/6930/75918/6930-75918-0000.flac'
VISEME_SHA = '9ce35224156f071ab58eb7feb8a5ceae600f6f9f353da2a6cbf797b6b1ac8a23'
EXTRA_SPEAKERS = 3
CACHE = ROOT / '.cache'
OUT = CACHE / 'speech'


def ensure_cache_dir():
    CACHE.mkdir(exist_ok=True)
    tag = CACHE / 'CACHEDIR.TAG'
    if not tag.exists():
        tag.write_text('Signature: 8a477f597d28d172789f06886806bc55\n'
                       '# prim-hrtf regenerable bench output (renders, captures, fixtures)\n')
    OUT.mkdir(exist_ok=True)


def duration(data: bytes) -> float:
    probe = subprocess.run(['ffprobe', '-v', 'error', '-show_entries', 'format=duration',
                            '-of', 'csv=p=0', '-i', 'pipe:0'], input=data,
                           capture_output=True, check=True)
    return float(probe.stdout.strip())


def main():
    ensure_cache_dir()
    sources_path = OUT / 'sources.json'
    if sources_path.exists():
        print(sources_path)
        return
    picked = {}
    speakers = set()
    with urllib.request.urlopen(URL, timeout=60) as response:
        with tarfile.open(fileobj=response, mode='r|gz') as archive:
            for member in archive:
                if not member.name.endswith('.flac'):
                    continue
                speaker = member.name.split('/')[2]
                want_viseme = member.name == VISEME_MEMBER
                want_extra = (speaker not in speakers and speaker != '6930'
                              and len(speakers) < EXTRA_SPEAKERS)
                if not (want_viseme or want_extra):
                    if VISEME_MEMBER in picked and len(speakers) >= EXTRA_SPEAKERS:
                        break
                    continue
                data = archive.extractfile(member).read()
                sha = hashlib.sha256(data).hexdigest()
                if want_viseme:
                    if sha != VISEME_SHA:
                        raise RuntimeError('viseme clip hash mismatch')
                elif not 4.0 <= duration(data) <= 9.0:
                    continue
                else:
                    speakers.add(speaker)
                picked[member.name] = (data, sha)
                if VISEME_MEMBER in picked and len(speakers) >= EXTRA_SPEAKERS:
                    break
    if VISEME_MEMBER not in picked:
        raise RuntimeError('viseme clip missing from archive')
    records = []
    for name, (data, sha) in sorted(picked.items(), key=lambda kv: kv[0] != VISEME_MEMBER):
        clip_id = Path(name).stem
        flac = OUT / f'{clip_id}.flac'
        flac.write_bytes(data)
        subprocess.run(['ffmpeg', '-nostdin', '-v', 'error', '-y', '-i', str(flac), '-ac', '1',
                        '-ar', '48000', '-f', 'f32le', str(OUT / f'{clip_id}-48k.f32')], check=True)
        records.append({'id': clip_id, 'member': name, 'sha256': sha,
                        'seconds': round(duration(data), 2)})
    sources_path.write_text(json.dumps({
        'url': URL, 'license': 'CC BY 4.0',
        'attribution': 'LibriSpeech ASR corpus, Vassil Panayotov, Guoguo Chen, Daniel Povey, '
                       'Sanjeev Khudanpur; recordings from LibriVox',
        'clips': records}, indent=2) + '\n')
    print(sources_path)


if __name__ == '__main__':
    main()

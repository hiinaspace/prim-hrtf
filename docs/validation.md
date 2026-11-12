# Validation notes

How prim-hrtf was checked against Prim's previous voice path (Godot + Steam Audio 4.8.1), and what is known about its behaviour. Raw numbers are in `results/`, reviews in `reviews/`, and blind listening packets in `packets/`.

## Running the bench

`nix develop -c uv run -q bench/run_bench.py --run .cache/bench/<name>` does four things:

- renders noise and LibriSpeech through Prim's Godot voice path, using
  `bench/godot/capture.gd` with the exact SteamAudioPlayer settings from `avatar.gd`;
- renders the same inputs through prim-hrtf and through Mumble's `HrtfSpatializer`
  (`bench/mumble/build.sh`);
- measures ITD, ILD and per-ear band levels;
- writes a blind A/B packet under `packets/`.

### Current evaluator (Steam Audio port)

Full numbers: `results/r2a4-20261111.json`. 40 seats: the 8 compass points, 20
through the ±90° poles, and 12 between measurements (2.5°, 32.5°, 60°, ±72.5°, 0° at
2.8° elevation, four elevated or lowered seats, and 89° and 91° left at 30° below). Against the Godot path, at every
seat:

- ITD within 0.4 µs;
- ILD within 0.65 dB;
- each ear's band levels within 0.7 dB, after removing one common level offset.

The libmysofa method (Mumble's evaluator) is up to 10.5 dB off in one ear at the
off-grid seats. The first prim-hrtf evaluator was the same method.

prim-hrtf is 4.5–5.0 dB louder than the Godot path at the same gain, at every seat.
It is also about 0.4 dB wider in ILD, consistently toward the source's side. Both were
already there with the first evaluator, so they are not interpolation. Their cause is
not checked. Match the level when this is wired into Prim.

Two discontinuities are Steam Audio's own, and the port keeps them (`eval.rs`, test
`small_moves_change_the_hrir_a_little`):

- **The exact interaural pole (±90°, 0° elevation).** Steam forces the elevation to 0
  there. That blends the front-low and back-low measurements of the 80° ring instead of
  using the horizontal one. The cone is about 0.02° wide, and a 0.25° step into it
  changes a band by up to 11.7 dB.
- **The gap below -45° interaural elevation.** CIPIC has no measurements there, and
  Steam clamps to the front or back end of the ring rather than blending across. A
  source more than about 45° below the ear axis that crosses the side plane switches
  between the two. At 30° below the horizon, crossing 90° left changes one band by
  12.4 dB in one step. The Godot path does the same: from 89° to 91° left at 30° below,
  the far ear's 10–16 kHz band drops 9.8 dB in both, to within 0.1 dB.

Steam's behaviour is kept on purpose, for parity. Blending across the gap remains an
option.

Everywhere else, the largest change per 0.25° step is 0.75 dB.

CPU per 480-frame callback, 7 sources, Ryzen 7 9800X3D:

| Case | Mean | p99 | Max |
|---|---|---|---|
| Head turning at 90°/s, every source re-evaluating and crossfading every callback (the worst case) | 49 µs | 56 µs | 73 µs |
| Static | 13.9 µs | 17.7 µs | 45 µs |

The worst case is 0.7 % of the 10 ms budget.

Not checked: a listening test of this evaluator (it matches Steam Audio numerically); the level offset's cause; anything on the Frame.

### Earlier evaluator (libmysofa port, replaced)

Full numbers: `results/r2a-20261111.json`. These are corrected after the evaluator
review (`reviews/2026-11-11-evaluator/`):

- At 0°, ±45°, ±135° and 180° it matched the Godot path within about 1 µs ITD and
  0.5 dB ILD.
- Elsewhere it did not:
  - at exactly ±90°, ILD was 1.4–2.3 dB narrower;
  - at +92°, ILD was 1.2 dB off;
  - at ±70°, ±75° and ±110°, one ear's colour was 5–6 dB off in some bands.
- The timbre figure averaged the two ears, which hid single-ear errors.
- The packet level-matched each clip, which hid the raw offset of about 4.7 dB.
- Every seat was an exact measurement point or the pole, so the off-grid notching the
  review found was never exercised.

**Blind listening test (headphones, earlier evaluator):**

- Seven pairs had no audible difference: 01 front, 02 45° right, 03 45° left, 04 90°
  right, 06 behind, and both four-voice scenes.
- Pair 05 (90° left) had a slight timbre difference. Neither side sounded wrong, and
  placement was the same.

A closer review of the evaluator then found the off-grid errors above, which led to
the Steam port.

## Steam Audio re-check (4.8.1; HEAD 480dd64 has no diff in these files)

The mono point-source binaural path is HRTF lookup plus interpolation, then
convolution:

- `binaural_effect.cpp:44-118`: with mono input, spatial blend only applies to stereo
  input. The defaults are `spatialBlend=1` and `phaseType=None` (`binaural_effect.h:35-36`).
- `hrtf_map.cpp:97-136`: bilinear weights in interaural coordinates, mirrored past the
  ±80° ring.
- `hrtf_database.cpp:127-153`, `343-391`: interpolation of log magnitude and unwrapped
  phase.
- `overlap_add_convolution_effect.cpp:29-109`: Tukey-windowed overlap-add.
- `distance_attenuation.cpp:54`: 1/max(d, min) distance attenuation. Prim disables it
  (`distance_attenuation=false`), as it does air absorption and occlusion.
- There is no near-field code. Prim's voice path has reflections off and uses
  `IPL_HRTFTYPE_DEFAULT`, which is the embedded CIPIC 124 table (`hrtf_map.cpp:25,64-66`).
- One loose end: godot-steam-audio passes an uninitialised `IPLHRTFSettings`
  (`server_init.cpp:18-20`), so `normType` is undefined. The measured levels match
  "no normalisation".
- aarch64: `CMakeLists.txt:54-55` detects linux-arm64 and a toolchain file exists, but
  the 4.8.1 release ships no linux-arm64 library. A build was not attempted.

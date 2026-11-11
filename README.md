# prim-hrtf

Binaural voice renderer for Prim. It replaces Steam Audio on the voice path with the
approach from S's Mumble `spatial-room` branch: the same CIPIC 124 HRTF, a ported
libmysofa evaluator, and a block convolver. Status: R2a candidate, waiting on S's
listening verdict.

- `tools/sofa2phrtf.py` bakes a SOFA file into `.phrtf` (48 kHz, int16 + global
  scale, libmysofa's neighbour table precomputed). `data/cipic_124.phrtf` is baked from
  Mumble's `default.sofa`: 1250 directions x 218 taps, 1.1 MiB, quantisation error
  -76 dB. It matches the table embedded in Steam Audio (`cipic_124.inl`) to within
  ±0.01 dB from 50 Hz to 16 kHz.
- `Hrtf::evaluate` ports `mysofa_interpolate`: nearest measurement plus the closer of each
  neighbour pair, inverse-distance weighted in the time domain.
- `Renderer` convolves with overlap-save (FFT 1024 for 480-frame blocks). Each source
  keeps a shared input history and crossfades over one block when the direction moves
  more than about 8° (Mumble's 0.99 dot-product threshold). The gain ramps across each
  block. `render_source` does not allocate: `tests/no_alloc.rs` runs 7 sources for 400
  blocks with retargeting under a counting allocator.
- `distance::voice_gain` is Prim's law: full level to 3 m, smoothstep to silence at
  15 m, times 150 %.

Build it with `nix develop -c cargo test --release`. Cross-build it for the Frame with
`nix develop .#aarch64 -c cargo build --release --target aarch64-unknown-linux-gnu`.
That build links. It has not been run on the device.

## R2a bench (2026-11-11)

`nix develop -c uv run -q bench/run_bench.py --run .cache/bench/<name>` does four things:

- renders noise and LibriSpeech through today's Godot voice path, using
  `bench/godot/capture.gd` with the exact SteamAudioPlayer settings from `avatar.gd`;
- renders the same inputs through prim-hrtf and through Mumble's `HrtfSpatializer`
  (`bench/mumble/build.sh`);
- measures ITD and ILD;
- writes a blind A/B packet under `packets/`.

The full numbers are in `results/r2a-20261111.json`.

What I found:

- At 0°, ±45°, ±80–100°, ±135° and 180°, prim-hrtf matches the Godot path closely:
  - ITD within about 1 µs;
  - ILD within 0.5 dB;
  - spectral shape within 0.7 dB.
- prim-hrtf is narrower than Godot at the exact ±90° seats:
  - ILD is 14.1 dB against 16.4 dB on the right, and 13.8 dB against 15.2 dB on the left.
  - On the right, Mumble itself measures 17.0 dB.
  - At those exact directions there are many equally near measurements. My port and
    libmysofa's kd-tree pick different ones.
- At ±70°, ±75° and ±110°, both prim-hrtf and Mumble differ from Steam Audio in colour
  by 5–6 dB in some bands. This is the libmysofa method itself: it blends unaligned
  impulse responses in time, where Steam interpolates magnitude and phase. These angles
  are passed through while someone turns their head, not seat positions.
- Mumble's ITD runs about 22 µs below the other two everywhere, and its level is
  +2 dB (libmysofa loudness normalisation). Neither shows up in the packet.

CPU per 480-frame callback, 7 moving sources, Ryzen 7 9800X3D: mean 16.8 µs, p99
42 µs, max 58 µs (0.6 % of the 10 ms budget at worst). With static sources: mean
14.7 µs, p99 20 µs.

**S's listening verdict (2026-11-11, blind, headphones):**

- 7 of 8 pairs were answered; pair 03 (45° left) was left blank.
- Six pairs had no audible difference: 01 front, 02 45° right, 04 90° right, 06 behind,
  and both four-voice scenes. Placement was the same and there was no preference.
- Pair 05 (90° left; A = Godot, B = prim-hrtf) had a slight timbre difference. Neither
  side sounded wrong, and placement was the same with no preference. S thinks this clip
  may just have the spectral content that makes the difference noticeable.
- The measured ±90° ILD gap did not show up as a placement difference. +90°, the larger
  measured gap, was not heard at all.

Not checked: pair 03, angles passed through during head turns (±70°, ±75°, ±110°),
and anything running on the Frame.

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

## Data

The speech is from LibriSpeech test-clean (CC BY 4.0): Panayotov, Chen, Povey and
Khudanpur, with recordings from LibriVox. CIPIC HRTF database: UC Davis CIPIC
Interface Laboratory.

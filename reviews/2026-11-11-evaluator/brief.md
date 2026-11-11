# Artifact review: prim-hrtf HRTF evaluator and renderer

## What this is and what's being decided

The owner, S, is a solo operator. No enterprise ceremony: findings should matter for
a small P2P VR cowatching app (Prim). The scale is 2–8 people at a virtual table,
each hearing up to about 7 remote voices.

prim-hrtf replaces Steam Audio on Prim's voice path. It ports the approach of S's
Mumble branch (`spatial-room`), which was written by early-2026 agents:

- libmysofa-style evaluation: the nearest measured HRIR plus 3 neighbours,
  inverse-distance weighted, blended in the time domain;
- held filters, retargeted when the direction moves more than about 8°;
- a one-block crossfade on each retarget;
- FFT block convolution.

**S has accepted the static sound.** In a blind A/B against today's Godot Steam Audio
path he could not tell 7 of 8 static seat pairs apart. One pair, 90° left, had a slight
timbre difference, and neither side sounded wrong. He also used the Mumble build with
head tracking through the OpenVR link plugin, and it sounded fine.

Before the evaluator is accepted as the long-term design, S wants a close look for two
things:

- subtler issues that will bite later, especially dynamic ones: head turns, moving
  talkers, and many sources;
- non-obvious evaluation optimisations that were not apparent when the strategy was
  chosen.

Review the code as written. Don't relitigate dropping Steam Audio.

## Target (read these)

All paths are relative to
`/tmp/claude-1000/-home-s-org/d78bce5d-a098-436f-bdc6-00ee03ba97fe/scratchpad/`.

- `prim-hrtf/src/eval.rs`: the `mysofa_interpolate` port and the nearest-neighbour
  search.
- `prim-hrtf/src/render.rs`: the per-source state, overlap-save convolution, retarget
  and crossfade, and the gain ramp.
- `prim-hrtf/src/format.rs` and `prim-hrtf/tools/sofa2phrtf.py`: the baked format and
  the baker (resampling, neighbour table, int16 quantisation).
- `prim-hrtf/src/distance.rs` and `prim-hrtf/src/lib.rs`: the distance law and the
  coordinate frames.
- `prim-hrtf/tests/no_alloc.rs` and `prim-hrtf/src/bin/bench.rs`.
- `prim-hrtf/README.md`: the measured results. The full numbers are in
  `prim-hrtf/results/r2a-20261111.json`.

## References (read to validate claims)

- `review-ref/libmysofa-hrtf/`: `interpolate.c`, `lookup.c`, `neighbors.c`,
  `loudness.c`, `easy.c` and `resample.c`.
- `review-ref/HrtfSpatializer.{cpp,h}`: Mumble's implementation, the port's origin.
- `review-ref/steam-core/`: Steam Audio 4.8.1 `binaural_effect`, `hrtf_database`,
  `hrtf_map` and `overlap_add_convolution_effect`, which is today's path.

## Environment facts

| Fact | Status |
|---|---|
| HRTF is CIPIC subject 124: 1250 directions, 25 lateral × 50 polar, lateral ±80° max. The SOFA is resampled 44.1 → 48 kHz, giving 218 taps. It is int16 with a global scale (quantisation error −76 dB). | verified: baker output and `.phrtf` header |
| The Steam Audio embedded table is the same data: within ±0.01 dB from 50 Hz to 16 kHz, lag 0. | verified: `sofa2phrtf.py --steam-inl` |
| Render block is 480 frames at 48 kHz (10 ms). FFT is 1024, realfft crate. Each source has a shared input history and 2 filter sets × 2 ears of spectra. | verified: code and tests |
| The retarget threshold is a dot product below 0.99, about 8.1°, the same as Mumble's `kDirChangeDotThreshold`. A crossfade runs over one block, and no new retarget starts while it fades. | verified: render.rs ~182–220 |
| Evaluation happens on the audio thread at retarget time. `nearest` is a brute-force scan of 1250 directions, followed by IDW and 2 forward FFTs per ear set. | verified: code |
| `render_source` allocates nothing: 7 sources × 400 blocks, retargeting every block, under a counting allocator. | verified: tests/no_alloc.rs |
| CPU on a Ryzen 9800X3D, 7 sources moving 0.9° per callback: mean 16.8 µs, p99 42 µs, max 58 µs per 10 ms callback. | verified: bench |
| The aarch64 cross-build links. It has not been run on the target, the Steam Frame (Snapdragon-class ARM). | verified build; run unknown |
| Measured against Godot Steam Audio, at a static 1.5 m distance with noise input. | verified: results JSON |
| ↳ At 0°, ±45°, ±80–100°, ±135° and 180°: ITD within about 1 µs, ILD within 0.5 dB, band spectral shape within 0.7 dB. | verified: results JSON |
| ↳ At exactly ±90°: ILD is 2.3 dB and 1.5 dB lower. | verified: results JSON |
| ↳ At ±70°, ±75° and ±110°: 5–6 dB band deviations, for both prim-hrtf and Mumble. | verified: results JSON |
| ↳ At +90°, prim-hrtf and Mumble differ (ILD 14.1 vs 17.0 dB). This is suspected to be nearest-neighbour tie-breaking at the interaural pole versus libmysofa's kd-tree. | the difference is verified; the cause is unverified |
| The port ignores SOFA delays, as Mumble does. CIPIC delays are zero; ITD lives in the HRIRs. | verified |
| Steam interpolates in interaural coordinates (bilinear), blending log magnitude and unwrapped phase. It uses Tukey-windowed overlap-add with frameSize/4 overlap. | verified: hrtf_database.cpp:127-153, 343-391; hrtf_map.cpp:97-136 |
| Prim's runtime context: voices arrive as 48 kHz Opus, 10 ms frames, over iroh P2P. Head pose updates at about 90 Hz. Talker positions are seats, but heads move. Where prim-hrtf will run (a native voice process vs inside Godot at a 44.1 kHz mix) is not settled. | unknown: device I/O is out of scope for this round |
| Distance law: full level to 3 m, smoothstep fade to 0 at 15 m, times 1.5. | verified: distance.rs |

## Criteria

1. Correctness bugs or numerical hazards in eval, render or baker. For example:
   - frame or handedness errors;
   - degenerate directions (zero vector, exact poles);
   - neighbour-table holes (2 missing neighbours were reported at bake);
   - crossfade or gain-ramp discontinuities;
   - circular-convolution aliasing (taps + block − 1 ≤ FFT?);
   - denormals;
   - state reset on stream gaps.
2. Dynamic behaviour. The held-filter plus 8° threshold plus one-block crossfade scheme
   under realistic head motion (yaw up to ~300°/s, small jitter): directional
   quantisation, crossfade comb or flanging between dissimilar IRs, zipper or chatter
   around the threshold. Also any interaction with the time-domain IDW blend.
3. Evaluation optimisations or quality wins not considered. For example:
   - minimum-phase plus separate fractional ITD delay;
   - precomputed per-direction spectra at bake time;
   - baking a denser or interaural-regular grid offline;
   - frequency-domain interpolation;
   - moving evaluation off the audio thread;
   - SIMD on aarch64;
   - partitioned convolution or latency;
   - sharing work across sources.

   Weigh each against its added complexity for a solo operator.
4. Anything in the baked format that would be painful to change later: versioning,
   rate, taps, per-direction metadata.

Deliver numbered findings with severity, each with file:line, the failure scenario, and
a concrete fix or the measurement that would settle it. Then list the calls that are
genuinely S's. "No findings" on a criterion is a fine answer.

Not-list:

- the choice of HRTF subject;
- Steam Audio versus our own renderer;
- the daemon or device I/O design;
- bench tooling style (bench/*.py, capture.gd) unless it invalidates a result.

Depth budget: the src/ files and the baker are small (about 750 lines), so read all
of them. Re-run numbers only where a finding depends on it. `nix develop` lives on a
different host, but plain `python3` with numpy, or quick reasoning, is fine here.

# Review: prim-hrtf evaluator and renderer (artifact mode)

**Verdict.** The renderer (`render.rs`) is sound. The evaluator (`eval.rs`, the libmysofa port) is not a good long-term design, and S's acceptance does not cover it: every direction in the blind A/B is an exact CIPIC grid point or the pole, where no interpolation happens.

**How I checked.** I baked `/home/s/lib/steam-audio/core/data/hrtf/cipic_124.sofa` with the repo's own baker functions and reimplemented `Hrtf::evaluate` in numpy (f32, same tie rules). Scripts are in `/tmp/claude-1000/-home-s-org/d78bce5d-a098-436f-bdc6-00ee03ba97fe/scratchpad/fable-tmp/` (`bake.py`, `exp.py` to `exp4.py`). No project files were modified.

**Not checked.**
- The Rust binary itself: `data/cipic_124.phrtf` is absent from the review copy, so I ran no cargo tests or bench.
- Whether that SOFA is byte-identical to Mumble's `default.sofa`. The bake does reproduce 1250 × 218 and the 2 missing neighbours.
- Anything by ear.

## Findings

### 1. MAJOR: time-domain blending puts deep notches at off-grid directions, and the acceptance evidence never exercised it

`eval.rs:52-90`.

- **Evidence gap.** Bench seats are `[0, ±45, ±90, 180]` plus tables on the same angles (`run_bench.py:43-44`). In the horizontal plane every multiple of 5° up to ±45° is a measured point, so `evaluate` returns a single IR (`eval.rs:46-50`). At ±90° it also returns a single IR (finding 2).
- **Cause.** CIPIC onsets are not smooth across rings. Raw 44.1 kHz onsets at polar 0:

  | Lateral | Left | Right |
  |---|---|---|
  | 0° | 785 µs | 771 µs |
  | 5° | 873 µs | 813 µs |
  | 10° | 896 µs | 794 µs |

  Blending rings 0 and 5 sums two copies 89 µs apart in the left ear.
- **Measured at az 2.5°, el 0** (third-octave bands, blend against its two source IRs):
  - left ear: −16 to −21.5 dB at 5 to 6.3 kHz, where the sources are −0.5 to −8 dB;
  - right ear: −16 dB at 10 kHz, where the sources are −6.5 dB.
- **Other midpoints**, deviation from a dB-magnitude blend of the same two IRs:

  | Azimuth | Deviation |
  |---|---|
  | 32.5° | 13.9 dB at 10 kHz |
  | 50° | 16.9 dB at 6.3 kHz |
  | 60° | 9.7 and 12.5 dB |
  | 72.5° | 8.1 and 11.4 dB |
  | 107° | 14.8 dB |

- **Consistency with the README.** Its 5 to 6 dB at ±70°, ±75° and ±110° is the same effect seen through wider bands. The README line that these angles are passed through "while someone turns their head, not seat positions" is wrong in general: almost every real direction is off-grid.
- **Fix confirmed numerically.** Time-aligning the two IRs before blending drops the deviation to 0.1 to 2 dB in most cases. A residual of 3 to 7 dB remains for the contralateral ear across the 65°–80° gap, where the IRs really differ.
- **Settling measurement.** Add static A/B pairs at az 2.5°, 32.5°, 60°, 72.5°, and one at az 0°, el 2.8°. The bench already takes arbitrary azimuth and elevation.

### 2. MAJOR: the libmysofa neighbour scheme is discontinuous and extrapolates on this grid

`eval.rs:24-35, 59-86`; `sofa2phrtf.py:63-80`.

The az/el stepping search does not match CIPIC's interaural-polar grid.

- **Wrong-side neighbour.** Rings are unevenly spaced (…40, 45, 55, 65, 80). "The closer of the az pair" for a ring-45 point is ring 40 until az 47.5°. So from 45° to 47.5° the source sits between 45 and 55 but blends 45 with 40, then jumps. The same happens from 65° to 67.5°.
- **Jump sizes** on a 0.1° horizontal sweep: ITD up to 39 µs per step (57 µs across two steps at 47.5°), ILD up to 3.8 dB.
- **Tie rule.** `fequals(da, db) → None` (`eval.rs:64`) drops both neighbours on every grid line, so weights jump when the direction leaves the line.
- **Off the horizontal plane** (el +7°, +15°, −20°, +40°), per 360° sweep at 0.1°:
  - 27 to 39 steps with an ITD jump above 20 µs (max 115 µs);
  - 28 to 42 steps with an ILD jump above 1 dB (max 4.3 dB);
  - 11 to 29 steps where the IR changes by more than −6 dB relative.
- **Pole cap** (beyond the ±80° ring, a 10° cone on the ear axis). At exactly ±90° all 50 ring points tie and every neighbour pair ties, so the output is one ring IR chosen by float rounding. In my run that was polar −33.8°, front-below. Ring ILDs span 13.7 to 17.4 dB, which brackets prim's 14.1 and Mumble's 17.0. That confirms the brief's unverified tie-breaking row in mechanism.
- **Near the pole.** At el +2°, the nearest polar index walks 11° to 90° between az 76° and 90°, with ILD steps up to 3.7 dB per 0.25°. At az 107° the blend swaps between ring points 17° of polar apart, a relative IR change of about 100%.
- **Why it matters for Prim.** A cowatching neighbour beside you sits in the 70°–110° zone as a seat, not a transit.
- **Polar gap.** The grid has no data from 230.6° to 315° (below). `nearest` clamps to an edge and flips front to back near straight down. This is a rare direction; fix it in the same pass.
- **Missing neighbours.** The 2 reported at bake are index 624 (zenith), az+ and az−. The `None` arms handle them; harmless.

**Fix for 1 and 2 together** (one change, about 100 lines of Rust and 40 of baker). The table is already in grid order (`index = lat_idx*50 + pol_idx`; 608 is lateral 0, polar 0).

- **Baker:**
  - estimate a fractional onset per direction and ear;
  - remove it, keeping a few lead samples;
  - store it as an f32 delay;
  - store the 25 lateral angles and the polar start, step and count in the header;
  - synthesise a ±90° pole entry as the mean of the aligned ring-80 IRs and delays.
- **Runtime:**
  - compute lateral `asin(-y)` and polar `atan2(z, x)`;
  - take 4-point bilinear weights on the grid, wrapping polar across the bottom gap;
  - blend aligned IRs and delays with the same weights;
  - apply the delay as a linear-phase ramp on the spectrum after `r2c` (phasor recurrence, no per-bin sin/cos).
- **What goes away:** the 1250-point scan, the neighbour table, IDW and the `fequals` ties.
- **Constraint:** aligned taps plus max delay must stay at or below `fft_len − block + 1` = 545 (218 today).

This is Steam's scheme (`hrtf_map.cpp:97-136`) with aligned-IR-plus-delay in place of log-magnitude and unwrapped phase.

**Cheaper alternative.** Do the good interpolation offline: bake a 1°–2° lateral grid plus poles (about 5 to 10 MB as int16) and use nearest-only at runtime. The runtime is simpler; the file and RAM are larger.

### 3. MAJOR for VR, cheap to change, but order matters: the 8.1° hold makes the image head-locked then snapping

`render.rs:13, 182-187`.

- **Scenario.** Facing a talker with ordinary head motion under 8°, no retarget ever happens. The voice is glued to the head, then jumps.
- **Measured steps on a yaw sweep at el 0:**

  | Threshold | Retargets per 360° | ITD step median / p90 / max | ILD step median / p90 / max |
  |---|---|---|---|
  | 8.1° | 44 | 69 / 125 / 135 µs | 1.4 / 4.8 / 7.1 dB |
  | 1° | 352 | 8 / 18 / 73 µs | 0.4 / 1.1 / 3.9 dB |

  ITD just-noticeable difference is about 10 to 20 µs. At 1° the remaining max is finding 2's discontinuities, not the threshold.
- **CPU.** A fade costs 7 FFTs per source-block against 3. At 16.8 µs for 7 sources this is free.
- **Interaction.** Lowering the threshold before fixing 1 and 2 turns static notches into notches that sweep with head motion (phaser-like). Do 1 and 2 first, or A/B the threshold alone on the headset knowing that.
- **Chatter.** None found. After a fade, `dir_current = dir_next`, so there is no oscillation at the threshold.
- **Crossfade comb.** At 8° steps the per-ear delay change is about 35 µs, a null near 14 kHz for 10 ms. That is marginal, and it vanishes at 1° to 2° steps.
- **Nit.** For thresholds under about 0.5°, compare squared chord length rather than an f32 dot near 1.

### 4. MINOR: a non-finite direction on a slot's first block poisons the whole mix permanently

`render.rs:72-81, 177-181`.

- `normalized` passes NaN through, because `len < 1e-4` is false.
- `load` then builds NaN filters and sets `dir_current` to NaN.
- `dot(..) < retarget_dot` is false forever, so the slot never retargets.
- Output is added into the shared `out_left` and `out_right`, so all voices become NaN until `reset_source`.

An Inf position gives the same result. NaN directions after init are ignored.

Fix: `if !(len >= 1e-4 && len.is_finite())`, fall back to forward. Sanitise `gain` the same way.

### 5. MINOR: stream gaps have no contract

`render.rs:139-145, 189-190`.

If the caller skips `render_source` during DTX or loss, the history keeps 544 stale samples. The first block after resume convolves their tail (up to 4.5 ms of old audio) and ramps from a stale gain.

Fix: document "feed zeros or call `reset_source`". Better, add an idle path:
- count consecutive all-zero blocks;
- after `ceil(fft_len / block)` = 3 of them, skip all FFTs;
- on resume, load the new direction directly with no crossfade, since the history is silent.

That is also the only many-source optimisation worth having, because most of the 7 voices are silent most of the time.

### 6. MINOR: nothing ties the Hrtf sample rate to the stream

`render.rs:16-24`; `format.rs:31`.

`Hrtf.sample_rate` is never checked. If this lands in Godot's 44.1 kHz mix with the 48 kHz bake, cues and ITD scale by 8.8% silently.

Fix: put `sample_rate` in `RendererConfig` and assert it. A 44.1 kHz target can bake natively (200 taps, no resample).

The fixed block of 480 means any other device callback size needs a FIFO, which adds latency. Decide that when placement is settled.

### 7. MINOR: format v1 cannot carry what the fix needs

`format.rs:5-6, 59-62`.

The version lives in the magic, the header is a fixed 56 bytes, and the length check is exact. There is one consumer and a 2-second rebake, so a v2 bump is cheap. Do it once, with finding 2:
- add `header_len`;
- add grid metadata (lateral list; polar start, step and count);
- add per-direction, per-ear delay;
- drop the neighbour table.

Trimming the common lead-in (about 0.7 ms) at the same time removes that latency for free. The int16 global scale is fine: −76 dB overall, about −50 dB relative on the quietest contralateral IRs.

### 8. NITS

- `bench.rs:89-91`: "moving" retargets every ~9 callbacks, so the published p99 and max are not the worst case. Time the retarget-every-block case before lowering the threshold or running on the Frame.
- `sofa2phrtf.py:109-114`: `apply_delay` rounds to integer samples at the source rate. Irrelevant for CIPIC; it loses sub-sample ITD for SOFAs that carry ITD in `Data.Delay`.
- `format.rs`: directions are not checked to be finite or unit length.
- Tests do not cover front/back or up/down. The mapping `[-z, -x, y]` is correct by inspection, and the Godot A/B at 135° and 180° agrees.

## No findings

- **Circular aliasing.** `fft_len ≥ block + taps − 1` always holds. The last 480 of 1024 outputs are valid (needs index ≥ 217, takes from 544).
- **Crossfade and gain ramp.** Continuous at block edges. The shared history means the new filter is correct from its first sample.
- **Handedness and frames.** Correct.
- **Denormals.** No feedback path; the history is exactly zero after 3 silent blocks.
- **Allocation.** The claim is consistent with the code.
- **CPU optimisations.** None are worth doing at 0.6% of budget:
  - precomputed spectra (10 MB) and off-thread evaluation;
  - partitioned convolution;
  - explicit SIMD (rustfft already uses NEON on aarch64);
  - summing sources in the frequency domain to share IFFTs. It is possible, because fades are block-aligned, but pointless at this scale.

  Spend the headroom on findings 1 to 3.

## Calls that are S's

1. **Is the off-grid colour audible to you?** Listen to static pairs at 2.5°, 32.5°, 60°, 72.5°, and 0° with el 2.8°, against Godot, before the evaluator rewrite is funded. My default is to do the rewrite anyway, because finding 2 is structural, but your ear sets the priority.
2. **Is the 8° hold audible in the headset?** Face one talker, turn your head slowly ±15°, and compare 0.99 against cos 1° = 0.99985. Expect the 1° setting to expose a sweeping notch until finding 1 is fixed.
3. **Which fix route?** Grid-bilinear plus aligned delays (small file, format v2, about 100 lines of runtime), or a dense offline bake with nearest-only runtime (5 to 10 MB, simplest runtime). I recommend the first.
4. **Is beside-you seating (talkers at 70° to 110°) a real Prim layout?** If yes, the pole synthesis in finding 2 is required, not optional.
# Evaluator review disposition (2026-11-11)

Two reviewers read the same brief (`brief.md`):

- Fable 5.2, neutral, at its default effort: `review-fable.md`.
- Codex gpt-6-astra at high reasoning effort, challenging the dynamic-behaviour
  invariant: `review-astra.md`.

The main loop reproduced the load-bearing numbers in an independent float64 numpy
port:

- Fade midpoint between 0° and 10°, left ear: −20.2 dB at 3.92 kHz, against −7.6 and
  −4.9 dB at the endpoints.
- At 2.5° azimuth, left ear, 5–6.3 kHz: the blend is −19.0 dB, against −2.8 and −5.0 dB
  for the two source IRs.
- At 70° azimuth, raising elevation from 0 to 0.001° moves the right ear by up to
  9.0 dB in one band.
- From 45.5° to 47.4° azimuth, the azimuth pair is lateral rings 40/55, and the closer
  one (40) is on the wrong side.

## The convolver is sound

Both reviewers say so.

- Overlap-save sizing is right: 697 ≤ 1024.
- The shared history is correct.
- The crossfade and the gain ramp are continuous.
- Frames and handedness are correct.
- No allocations.
- No denormal path.

CPU optimisations are not worth doing at 0.6 % of budget: precomputed spectra,
partitioned convolution, explicit SIMD and frequency-domain summing. They would only
matter if Frame numbers say otherwise.

## Findings

| # | Finding | Who | Disposition |
|---|---|---|---|
| 1 | Time-domain blending of IRs whose onsets differ by tens of µs puts deep notches (−15 to −20 dB) at off-grid directions and at crossfade midpoints. The blind A/B never exercised it: every seat was an exact grid point or the pole. | both | **Adopt.** Reproduced. Fix: onset-aligned IRs plus a separate fractional delay, blended with the same weights. Needs S's go-ahead (format v2). |
| 2 | libmysofa neighbour selection is discontinuous on CIPIC's interaural grid. Tie rules drop neighbours on grid lines. It picks the wrong-side ring between unevenly spaced rings (45→55, 65→80). The ±90° cap is chosen by float rounding. | both | **Adopt, with 1.** Reproduced. Fix: bilinear weights in interaural lateral/polar coordinates (Steam's topology), a synthesised ±90° pole, and a wrap across the bottom gap. This removes the kd-scan, the neighbour table, IDW and the ties. |
| 3 | The 8.1° hold leaves the direction wrong indefinitely after a small turn: ITD steps have a median of 69 µs and a p90 of 125 µs per retarget. | both | **Adopt after 1+2.** Lower to about 1°, and compare squared chord length rather than dot. Doing it before 1+2 would turn static notches into sweeping ones (Fable). |
| 4 | A NaN or Inf direction, or a NaN gain, on a slot's first block poisons the whole mix permanently. | both | **Adopt.** Guard with `is_finite`, keep the last valid direction, and add a test. |
| 5 | Stream gaps have no contract. Skipped blocks leave a stale history and a stale gain. | both | **Adopt.** Document "feed silence or reset". Add an idle path: after 3 zero blocks, skip FFTs; on resume, load directly with no fade (Fable). |
| 6 | The HRTF sample rate is never checked against the stream. A 44.1 kHz mix would shift cues by 8.8 %. | both | **Adopt.** Add the rate to `RendererConfig` and reject a mismatch. A 44.1 kHz target bakes natively. |
| 7 | Format v1 can't carry delays or grid metadata. Its fixed header and exact length check make it hard to extend. | Fable | **Adopt, with 1+2.** v2 gets `header_len`, grid metadata and per-direction per-ear delay, drops the neighbour table, and trims the common lead-in (about 0.7 ms latency). |
| 8 | README overstates the static agreement: +92° is off by 1.2 dB ILD. The timbre metric averages the ears. The packet's per-clip level match hides a raw level offset of about 4.7 dB. | Astra | **Adopt.** Correct the README. Report per-ear deviations. Calibrate dynamic tests to one fixed gain. |
| 9 | The CPU bench's "moving" case retargets every 9 callbacks, so its p99 is not the worst case. | Fable | **Adopt.** Add a retarget-every-block timing before running on the Frame. |
| 10 | Nearest search could compare squared distances. | Astra | **Moot** if 2 lands, since the search goes away. |
| 11 | Dense offline bake with nearest-only runtime (5–10 MB) as the cheaper alternative to 1+2. | Fable | **Rejected as default.** It needs the same aligned interpolation offline and a much larger file. Astra notes that baking by sampling the current evaluator would not fix anything. It stays an option if S prefers the simplest runtime. |

## For S

The listening result stands for the seats tested. Mumble's head-tracked use sounding
fine is consistent with the findings, since Mumble has the same evaluator. The
reviewers' point is that nobody has listened to off-grid directions, and that is where
most real directions fall.

The recommended next round:

1. Make the defect fixes (4, 5, 6, 8, 9).
2. Implement 1+2+7: interaural bilinear interpolation with aligned IRs and delays, in
   format v2.
3. Lower the threshold (3).
4. Build a second blind packet against Godot. Add off-grid static seats (2.5°, 32.5°,
   60°, 72.5°, and 0° at elevation 2.8°) and rendered head-turn clips: a slow ±15° turn
   and a fast sweep.

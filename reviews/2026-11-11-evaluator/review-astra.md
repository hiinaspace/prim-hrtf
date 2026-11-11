**The challenge does not survive.** The actual CIPIC table produces substantial cancellation during valid retargets, and the held direction can remain wrong indefinitely after a small head turn. Whether S hears these effects in ordinary conversation remains a listening question.

I checked all named implementation files and references. Numerical probes used an independent float32 translation of the evaluator and the original `.phrtf` from the build host; the original `eval.rs` and `render.rs` hashes match the review copy. These are signal measurements, not a Rust runtime test or listening verdict.

1. **Major — the one-block crossfade introduces substantial spectral cancellation.**  
   **Location:** `prim-hrtf/src/render.rs:182–224`.

   Concrete scenario: initialize at 0°, then turn at 300°/s with 90 Hz poses sampled by 10 ms audio callbacks. The first retarget occurs at 40 ms, from 0° to the latest available pose, 10°. At the fade midpoint, the effective filter is exactly:
   \[
   H_{\mathrm{fade}}=(H_0+H_{10})/2.
   \]

   Measured left-ear responses, before the common gain:

   | Measurement | 0° filter | 10° filter | Fade midpoint |
   |---|---:|---:|---:|
   | 3.92 kHz magnitude | −8.14 dB | −5.04 dB | **−20.54 dB** |
   | Mean power over 3–5 kHz, in dB | −0.96 dB | −2.51 dB | **−8.85 dB** |

   The 3.92 kHz endpoint phases differ by about 170°. This is destructive interference, not merely directional quantization. At the actual head angle at 45 ms, 13.5°, directly evaluating the same table gives −4.64 dB at that frequency: a **15.9 dB difference**. Adding ±0.01° elevation perturbations preserves the counterexample.

   **Action:** include this trajectory in a dynamic comparison using speech, noise, and a diagnostic tone. First compare against evaluation every callback with smaller transitions. If cancellation remains objectionable, evaluate delay-aligned interpolation or minimum-phase filters with separately interpolated fractional delays. Merely lengthening the crossfade prolongs the cancellation; an equal-power fade cannot eliminate phase cancellation.

2. **Major — the evaluator is discontinuous under tiny direction changes.**  
   **Location:** `prim-hrtf/src/eval.rs:59–85`.

   At azimuth 70°, elevation 0°, the evaluator uses measurements 1158 and 1208 with weights approximately 0.6665 and 0.3335. At elevation **0.001°**, measurement 1159 suddenly receives **0.3800** weight, reducing the previous weights to approximately 0.4132 and 0.2068.

   This follows directly from dropping both neighbours when their distances are nearly equal, then admitting one at full inverse-distance weight immediately outside that tolerance. With a 0.01° elevation change, right-ear power over 300 Hz–8 kHz changes by about **1.17 dB**, with individual frequency differences reaching about **9.3 dB**.

   This faithfully reproduces the supplied libmysofa logic; it is an inherited algorithmic problem, not an inaccurate Rust port. The held renderer suppresses isolated tiny jitter, but real yaw motion triggers evaluations whose results depend strongly on that jitter. Simply evaluating more frequently exposes this discontinuity more often.

   **Action:** test continuity across neighbour-selection and nearest-measurement boundaries. For a replacement, use continuous weights over a defined interpolation topology, with an explicit policy for the unmeasured lateral caps. Changing tie-breaking alone does not fix it. A denser bake made by sampling this same evaluator also does not automatically remove the defect.

3. **Major — the 8.1° deadband creates persistent direction error, not just a 10 ms tracking delay.**  
   **Location:** `prim-hrtf/src/render.rs:182–186, 220–223`; inherited claim in `review-ref/HrtfSpatializer.cpp:173–174`.

   Initialize at 0°, turn to 7°, and stop: the renderer continues using 0° **forever**. In the actual table, the interaural phase delay measured at 500 Hz changes from approximately 10.5 µs at 0° to 131.3 µs at 7°; the held renderer omits that change.

   With 90 Hz pose sampling and 10 ms callbacks, my trajectory simulation gives approximately **5° mean direction error at 90°/s**, and up to **12° at 300°/s**, before any device latency. These are filter-target errors; a crossfade does not necessarily have a single perceptual direction.

   **Action:** compare the current policy against per-callback tracking or a smaller angular threshold plus a maximum filter age. Include slow turns and turn-then-stop tests. “One-block lag” is an incorrect justification for the current policy.

4. **Minor — a non-finite initial direction permanently poisons a source slot.**  
   **Location:** `prim-hrtf/src/render.rs:72–80, 177–183`; `prim-hrtf/src/eval.rs:44–89`.

   If the first direction contains NaN, normalization passes it through, evaluation creates NaN filters, and `dir_current` becomes NaN. Subsequent valid poses cannot recover: `dot(...) < retarget_dot` is always false. That source then contaminates the summed output until explicitly reset.

   **Fix:** validate finite direction components before normalization, and validate gain before applying it. Use a stable fallback for initialization and retain the last valid direction afterward. Add a regression covering invalid initialization followed by valid poses.

5. **Minor — skipped callbacks preserve stale voice history across a stream gap.**  
   **Location:** `prim-hrtf/src/render.rs:139–145, 189–190`.

   If a caller stops rendering a silent or disconnected source and later resumes the same slot, elapsed time is invisible to the renderer. The previous input tail is convolved into the resumed block; for the current 218-tap filter, stale contributions can occupy its first 217 samples.

   `reset_source` provides the necessary mechanism, but its documentation mentions leaving/restarting rather than ordinary skipped intervals.

   **Fix:** make the integration contract explicit: supply silence/PLC continuously, or reset when discontinuous input resumes. Test a nonzero block, a skipped interval, and resumed silence. This matters before optimizing away inactive sources.

6. **Minor — the sample-rate metadata is not enforced at the rendering boundary.**  
   **Location:** `prim-hrtf/src/format.rs:52–57`; `prim-hrtf/src/render.rs:88–124`.

   A 48 kHz table can silently be used on a 44.1 kHz stream. Its delays then become **8.84% longer**, and spectral features shift to **91.875%** of their intended frequencies. The renderer consumes the `Hrtf` without checking a processing rate or exposing its rate afterward.

   **Fix:** require a processing rate at construction and reject mismatches, or expose and clearly enforce the table’s required rate. This is an API contract independent of which device architecture is chosen.

7. **Minor — the reported static agreement overstates what the measurements establish.**  
   **Location:** `prim-hrtf/README.md:42–54`; `prim-hrtf/bench/run_bench.py:281–290, 311–314`.

   The claimed ±80–100° agreement is not supported across that interval. At +92°, the supplied JSON reports **8.7 µs ITD error, 1.16 dB ILD error, and 1.09 dB timbre error**.

   Also, `timbre_max_abs_db` uses **power averaged across ears**, then removes a mean spectral offset. That can conceal substantial changes in the quieter ear. The listening packet separately level-matches each clip; its acceptance therefore does not validate the raw approximately **+4.7 dB** level difference at many seats.

   **Fix:** name the matching sampled angles precisely, report per-ear spectral deviations, and retain the distinction between level-matched sound acceptance and production gain calibration. Dynamic comparisons should use one fixed calibration, not normalize away time-varying level changes.

The inexpensive optimization is in `eval.rs:24–33`: compare **squared distances** during nearest search, then take square roots only for the selected distances needed by IDW. That preserves ordering mathematically, though floating-point ties deserve regression coverage. Precomputed 1024-point spectra would occupy about **9.8 MiB** and remove two filter FFTs per retarget, but weighted complex-spectrum interpolation is mathematically equivalent to the current time-domain blend—it does **not** solve cancellation. Benchmark it on Frame before accepting the memory tradeoff. Partitioned convolution, worker-thread evaluation, and custom SIMD are not justified by the supplied desktop measurements.

I found no overlap-save sizing error: 480 + 218 − 1 = 697 ≤ 1024. Sharing input history correctly initializes the incoming filter. The fade and gain endpoints advance by their normal final sample increment; they do not introduce an extra boundary jump. The two missing angular neighbours belong to the zenith measurement, where azimuth is degenerate. Small jitter alone also does not repeatedly chatter around the retarget threshold, because each completed retarget establishes a new centre.

**S’s calls:**

- **Recommended:** keep the accepted static sound as the baseline, but defer long-term dynamic acceptance until a blind comparison covers slow turn-and-stop, fast sweeps, and slight pitch motion near the lateral directions.
- If those differences are audible, choose between **preserving the existing coloration with better tracking** and **accepting a changed interpolation method to reduce cancellation**. The measurements justify building that comparison; they cannot choose S’s preferred sound.
- If the dynamic comparison remains indistinguishable, accepting the measured residuals is reasonable. “No audible artifacts” should then describe that tested listening result, not an invariant of this algorithm.
//! HRIR evaluation: a port of Steam Audio 4.8.1's bilinear HRTF interpolation, the path
//! Prim's Godot voice used before this crate (`IPL_HRTFINTERPOLATION_BILINEAR`):
//!
//! - `HRTFMap::interpolatedHRIRWeights` (hrtf_map.cpp): convert the direction to
//!   interaural coordinates, bracket it between two lateral rings and, on each ring,
//!   between two elevations, and form four bilinear weights. Past the outermost ring
//!   (±80° for CIPIC) the bracket mirrors across the interaural pole.
//! - `HRTFDatabase::interpolateHRIRs` (hrtf_database.cpp): blend the four
//!   measurements' log magnitude and unwrapped phase with those weights, wrap the
//!   phase, exponentiate, and inverse-FFT to an `interp_len`-sample HRIR.
//!
//! Blending unwrapped phase interpolates each frequency's delay, so neighbours whose
//! onsets differ do not cancel the way a time-domain blend of their IRs does.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner};

use crate::Hrtf;

/// Steam's `InterauralSphericalVector3f` of a unit direction in Steam's canonical frame
/// (which is Godot's listener-local frame: +x right, +y up, +z back), in degrees:
/// lateral in [-90, 90] (+ right), elevation in [0, 360) measured from straight down.
fn interaural_degrees(d: [f32; 3]) -> (f32, f32) {
    use std::f32::consts::{FRAC_PI_2, PI};
    let azimuth = d[0].clamp(-1.0, 1.0).asin();
    let elevation = if (azimuth - FRAC_PI_2).abs() < 1e-5 || (azimuth + FRAC_PI_2).abs() < 1e-5 {
        0.0
    } else {
        (PI + d[2].atan2(d[1])) % (2.0 * PI)
    };
    (azimuth.to_degrees(), elevation.to_degrees())
}

/// `HRTFMap::lowerAndUpperBound`: indices bracketing `x` in ascending `h`, clamped.
fn bounds(h: &[f32], x: f32) -> (usize, usize) {
    let u = h.partition_point(|v| *v <= x); // std::upper_bound
    (u.saturating_sub(1), u.min(h.len() - 1))
}

/// `HRTFMap::calculateLinearInterpolationWeights`.
fn linear_weights(x: f32, x_min: f32, x_max: f32, i_min: usize, i_max: usize) -> (f32, f32) {
    if i_min == i_max && x_min == x_max {
        return (0.0, 1.0);
    }
    let w_min = (x_max - x) / (x_max - x_min);
    (w_min, 1.0 - w_min)
}

impl Hrtf {
    /// Steam's four bilinear (index, weight) pairs for unit direction `d` (Godot
    /// listener frame). Weights sum to 1; indices may repeat.
    pub fn weights(&self, d: [f32; 3]) -> ([usize; 4], [f32; 4]) {
        let (phi, theta) = interaural_degrees(d);
        let n_az = self.lateral.len();
        let (mut theta_for_min, mut theta_for_max) = (theta, theta);
        let (mut i_min, mut i_max) = bounds(&self.lateral, phi);
        let (mut phi_min, mut phi_max) = (self.lateral[i_min], self.lateral[i_max]);
        if i_min == 0 && i_max == 0 {
            phi_min += 2.0 * (-90.0 - phi_min);
            theta_for_min += 2.0 * (180.0 - theta_for_min);
        }
        if i_min == n_az - 1 && i_max == n_az - 1 {
            phi_max += 2.0 * (90.0 - phi_max);
            theta_for_max += 2.0 * (180.0 - theta_for_max);
        }
        if d[2] > 0.0 {
            std::mem::swap(&mut phi_min, &mut phi_max);
            std::mem::swap(&mut i_min, &mut i_max);
            std::mem::swap(&mut theta_for_min, &mut theta_for_max);
        }
        let p = self.per_ring;
        let ring = |i: usize| &self.elevation[i * p..(i + 1) * p];
        let (a0, a1) = bounds(ring(i_min), theta_for_min);
        let (b0, b1) = bounds(ring(i_max), theta_for_max);
        let (wp_min, wp_max) = linear_weights(phi, phi_min, phi_max, i_min, i_max);
        let (wa0, wa1) = linear_weights(theta_for_min, ring(i_min)[a0], ring(i_min)[a1], a0, a1);
        let (wb0, wb1) = linear_weights(theta_for_max, ring(i_max)[b0], ring(i_max)[b1], b0, b1);
        (
            [i_min * p + a0, i_min * p + a1, i_max * p + b0, i_max * p + b1],
            [wp_min * wa0, wp_min * wa1, wp_max * wb0, wp_max * wb1],
        )
    }
}

/// Scratch for evaluating interpolated HRIRs without allocating. One per renderer.
pub struct Evaluator {
    c2r: Arc<dyn ComplexToReal<f32>>,
    scratch: Vec<Complex<f32>>,
    mag: Vec<f32>,
    phase: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    time: Vec<f32>,
}

impl Evaluator {
    pub fn new(hrtf: &Hrtf) -> Self {
        let c2r = RealFftPlanner::<f32>::new().plan_fft_inverse(hrtf.interp_len);
        let bins = hrtf.bins();
        Evaluator {
            scratch: c2r.make_scratch_vec(),
            c2r,
            mag: vec![0.0; bins],
            phase: vec![0.0; bins],
            spectrum: vec![Complex::new(0.0, 0.0); bins],
            time: vec![0.0; hrtf.interp_len],
        }
    }

    /// Write the interpolated HRIR pair for direction `d` (unit, Godot listener frame)
    /// into `left` / `right`, each `hrtf.interp_len()` long. Allocation-free.
    pub fn evaluate(&mut self, hrtf: &Hrtf, d: [f32; 3], left: &mut [f32], right: &mut [f32]) {
        let n = hrtf.interp_len;
        assert!(left.len() == n && right.len() == n);
        let (indices, weights) = hrtf.weights(d);
        let bins = hrtf.bins();
        let inv = 1.0 / n as f32;
        for (ear, out) in [left, right].into_iter().enumerate() {
            self.mag.fill(0.0);
            self.phase.fill(0.0);
            for (&i, &w) in indices.iter().zip(&weights) {
                let at = (i * 2 + ear) * bins;
                for ((m, p), (lm, ph)) in self.mag.iter_mut().zip(self.phase.iter_mut())
                    .zip(hrtf.log_mag[at..at + bins].iter().zip(&hrtf.phase[at..at + bins]))
                {
                    *m += w * lm;
                    *p += w * ph;
                }
            }
            // wrapPhase, exp, polarToCartesian.
            for ((x, m), p) in self.spectrum.iter_mut().zip(&self.mag).zip(&self.phase) {
                let wrapped = p - 2.0 * std::f32::consts::PI * (p / (2.0 * std::f32::consts::PI)).round();
                let (s, c) = wrapped.sin_cos();
                let a = m.exp();
                *x = Complex::new(a * c, a * s);
            }
            // A real inverse FFT only reads the real part at DC and Nyquist.
            self.spectrum[0].im = 0.0;
            self.spectrum[bins - 1].im = 0.0;
            self.c2r
                .process_with_scratch(&mut self.spectrum, &mut self.time, &mut self.scratch)
                .expect("fft sizes");
            for (o, t) in out.iter_mut().zip(&self.time) {
                *o = t * inv;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::direction_from_angles;

    fn hrtf() -> Hrtf {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/data/cipic_124.phrtf");
        Hrtf::from_bytes(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn weights_sum_to_one_everywhere() {
        let h = hrtf();
        for az in (-180..180).step_by(7) {
            for el in (-85..=85).step_by(11) {
                let (_, w) = h.weights(direction_from_angles(az as f32 + 0.3, el as f32 + 0.2));
                let s: f32 = w.iter().sum();
                assert!((s - 1.0).abs() < 1e-4, "az {az} el {el}: {w:?}");
                assert!(w.iter().all(|v| v.is_finite() && *v > -1e-4));
            }
        }
    }

    #[test]
    fn grid_point_reproduces_its_measurement() {
        let h = hrtf();
        // Straight ahead: lateral 0, elevation 90 (from below) is a CIPIC point.
        let (idx, w) = h.weights(direction_from_angles(0.0, 0.0));
        let k = w.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert!((w[k] - 1.0).abs() < 1e-5, "{w:?}");
        assert_eq!(h.grid_angles(idx[k]), (0.0, 90.0));
        let mut ev = Evaluator::new(&h);
        let n = h.interp_len();
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        ev.evaluate(&h, direction_from_angles(0.0, 0.0), &mut l, &mut r);
        for (ear, out) in [&l, &r].into_iter().enumerate() {
            let want = h.ir(idx[k], ear);
            let peak = want.iter().fold(0.0f32, |a, v| a.max(v.abs()));
            for (t, v) in out.iter().enumerate() {
                let e = if t < h.taps { want[t] } else { 0.0 };
                assert!((v - e).abs() < 1e-4 * peak, "ear {ear} tap {t}: {v} vs {e}");
            }
        }
    }

    #[test]
    fn unwrap_matches_steam_on_a_linear_phase_ramp() {
        // A pure delay has phase -w*d; wrapped into (-pi, pi] it must unwrap back.
        let raw: Vec<f32> = (0..64).map(|k| {
            let p = -(k as f32) * 0.9;
            p - 2.0 * std::f32::consts::PI * (p / (2.0 * std::f32::consts::PI)).round()
        }).collect();
        let mut v = raw.clone();
        crate::format::unwrap_phase(&mut v);
        for (k, x) in v.iter().enumerate() {
            assert!((x + k as f32 * 0.9).abs() < 1e-3, "bin {k}: {x}");
        }
    }

    /// Band levels (dB) of an HRIR in 1/3-octave bands from 200 Hz to 16 kHz.
    fn bands(ir: &[f32]) -> Vec<f32> {
        let n = ir.len();
        let mut fft = RealFftPlanner::<f32>::new();
        let f = fft.plan_fft_forward(n);
        let mut t = ir.to_vec();
        let mut x = f.make_output_vec();
        f.process(&mut t, &mut x).unwrap();
        let hz = 48000.0 / n as f32;
        let mut out = Vec::new();
        let mut lo = 200.0f32;
        while lo < 16000.0 {
            let hi = lo * 2f32.powf(1.0 / 3.0);
            let (a, b) = ((lo / hz).floor() as usize, ((hi / hz).ceil() as usize).max((lo / hz).floor() as usize + 1));
            let p: f32 = x[a..b].iter().map(|c| c.norm_sqr()).sum::<f32>() / (b - a) as f32;
            out.push(10.0 * p.max(1e-20).log10());
            lo = hi;
        }
        out
    }

    #[test]
    fn small_moves_change_the_hrir_a_little() {
        // Every 0.25 degree step along azimuth (at several elevations) and along
        // elevation (at several azimuths): the largest band-level change per step.
        let h = hrtf();
        let mut ev = Evaluator::new(&h);
        let n = h.interp_len();
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        let mut prev: Option<[Vec<f32>; 2]> = None;
        let mut worst = (0.0f32, String::new());
        let mut paths: Vec<Vec<(f32, f32)>> = Vec::new();
        for el in [-30.0, -0.001, 0.0, 0.001, 20.0, 50.0] {
            paths.push((0..=1440).map(|k| (-180.0 + k as f32 * 0.25, el)).collect());
        }
        for az in [0.0, 45.0, 70.0, 90.0, -110.0, 150.0] {
            paths.push((0..=520).map(|k| (az, -45.0 + k as f32 * 0.25)).collect());
        }
        for path in paths {
            prev = None;
            for &(az, el) in &path {
                let d = crate::direction_from_angles(az, el);
                ev.evaluate(&h, d, &mut l, &mut r);
                let cur = [bands(&l), bands(&r)];
                // Steam's two known discontinuities, kept for fidelity (README): the
                // exact interaural pole, and the unmeasured gap below -45 degrees
                // interaural elevation, where it clamps to the front or back end of
                // the ring instead of blending across.
                let (lat, theta) = interaural_degrees(d);
                let known = lat.abs() > 89.0 || !(44.0..=316.0).contains(&theta);
                if known {
                    prev = None;
                    continue;
                }
                if let Some(p) = &prev {
                    let mut step = 0.0f32;
                    for ear in 0..2 {
                        for (a, b) in p[ear].iter().zip(&cur[ear]) {
                            step = step.max((a - b).abs());
                            if (a - b).abs() > worst.0 {
                                worst = ((a - b).abs(), format!("az {az} el {el} ear {ear}"));
                            }
                        }
                    }
                }
                prev = Some(cur);
            }
        }
        let _ = prev;
        eprintln!("largest band change per 0.25 deg step: {:.2} dB at {}", worst.0, worst.1);
        assert!(worst.0 < 1.5, "{worst:?}");
    }
}

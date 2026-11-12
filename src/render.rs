// SPDX-License-Identifier: Unlicense
//! Per-source binaural rendering: overlap-save FFT convolution over a shared input
//! history, re-evaluating the HRIR whenever the direction moves and crossfading
//! between the old and new filter across one block.
//!
//! Steam Audio re-evaluates every frame too; its implicit crossfade is the
//! Tukey-windowed overlap of its overlap-add convolver (a quarter frame). Here the
//! crossfade is linear over the whole block, which is at least as smooth.

use std::fmt;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::eval::Evaluator;
use crate::Hrtf;

/// Default re-evaluation threshold in degrees: well under the ~1° minimum audible
/// angle, so a moving source or head is effectively tracked every block.
pub const DEFAULT_RETARGET_DEGREES: f32 = 0.1;

#[derive(Clone, Copy, Debug)]
pub struct RendererConfig {
    /// Sample rate of the audio this renderer is fed; must equal the HRTF's.
    pub sample_rate: u32,
    /// Samples per render call (fixed; 480 = 10 ms at 48 kHz).
    pub block: usize,
    /// Source slots allocated up front.
    pub max_sources: usize,
    /// Re-evaluate the HRIR once the direction has moved more than this many degrees.
    pub retarget_degrees: f32,
}

impl Default for RendererConfig {
    fn default() -> Self {
        RendererConfig { sample_rate: 48000, block: 480, max_sources: 16, retarget_degrees: DEFAULT_RETARGET_DEGREES }
    }
}

#[derive(Debug, PartialEq)]
pub enum ConfigError {
    /// The HRTF was baked for a different rate than the stream (cues would shift).
    SampleRate { hrtf: u32, stream: u32 },
    Invalid(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::SampleRate { hrtf, stream } => {
                write!(f, "HRTF is baked at {hrtf} Hz but the stream is {stream} Hz; rebake at the stream rate")
            }
            ConfigError::Invalid(why) => write!(f, "invalid renderer config: {why}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Copy, Debug)]
pub struct SourceParams {
    /// Listener-local direction, Godot convention; any non-zero length. A non-finite
    /// direction is ignored (the last valid one is kept).
    pub direction: [f32; 3],
    /// Linear gain for this block (e.g. from [`crate::voice_gain`]); ramped from the
    /// previous block's value. A non-finite gain is ignored likewise.
    pub gain: f32,
}

struct Source {
    initialized: bool,
    /// Last `fft_len` input samples (overlap-save history).
    history: Vec<f32>,
    /// Filter spectra [set][ear], pre-scaled by 1/fft_len.
    filters: [[Vec<Complex<f32>>; 2]; 2],
    current: usize,
    dir_current: [f32; 3],
    gain_prev: f32,
}

pub struct Renderer {
    hrtf: Hrtf,
    evaluator: Evaluator,
    block: usize,
    fft_len: usize,
    /// Squared chord length between unit vectors that triggers a retarget.
    retarget_chord2: f32,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
    time: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    product: Vec<Complex<f32>>,
    r2c_scratch: Vec<Complex<f32>>,
    c2r_scratch: Vec<Complex<f32>>,
    ir: [Vec<f32>; 2],
    /// [current L, current R, next L, next R], `block` each.
    wet: [Vec<f32>; 4],
    sources: Vec<Source>,
}

const FORWARD: [f32; 3] = [0.0, 0.0, -1.0];

/// Unit direction, or None for a non-finite input. A (near-)zero vector, i.e. a
/// coincident source, maps to straight ahead as Mumble does.
fn normalized(d: [f32; 3]) -> Option<[f32; 3]> {
    if !d.iter().all(|v| v.is_finite()) {
        return None;
    }
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if !len.is_finite() {
        return None;
    }
    Some(if len < 1e-4 { FORWARD } else { [d[0] / len, d[1] / len, d[2] / len] })
}

fn chord2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (x, y, z) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    x * x + y * y + z * z
}

impl Renderer {
    pub fn new(hrtf: Hrtf, config: RendererConfig) -> Result<Self, ConfigError> {
        if config.sample_rate != hrtf.sample_rate {
            return Err(ConfigError::SampleRate { hrtf: hrtf.sample_rate, stream: config.sample_rate });
        }
        if config.block == 0 || config.max_sources == 0 {
            return Err(ConfigError::Invalid("block and max_sources must be non-zero"));
        }
        if !(config.retarget_degrees.is_finite() && config.retarget_degrees >= 0.0) {
            return Err(ConfigError::Invalid("retarget_degrees must be a non-negative number"));
        }
        let ir_len = hrtf.interp_len();
        let fft_len = (config.block + ir_len - 1).next_power_of_two();
        let bins = fft_len / 2 + 1;
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(fft_len);
        let c2r = planner.plan_fft_inverse(fft_len);
        let zero = Complex::new(0.0, 0.0);
        let sources = (0..config.max_sources)
            .map(|_| Source {
                initialized: false,
                history: vec![0.0; fft_len],
                filters: std::array::from_fn(|_| std::array::from_fn(|_| vec![zero; bins])),
                current: 0,
                dir_current: FORWARD,
                gain_prev: 0.0,
            })
            .collect();
        let half = (config.retarget_degrees.to_radians() / 2.0).sin();
        Ok(Renderer {
            evaluator: Evaluator::new(&hrtf),
            block: config.block,
            fft_len,
            retarget_chord2: 4.0 * half * half,
            time: vec![0.0; fft_len],
            spectrum: vec![zero; bins],
            product: vec![zero; bins],
            r2c_scratch: r2c.make_scratch_vec(),
            c2r_scratch: c2r.make_scratch_vec(),
            ir: [vec![0.0; ir_len], vec![0.0; ir_len]],
            wet: std::array::from_fn(|_| vec![0.0; config.block]),
            r2c,
            c2r,
            sources,
            hrtf,
        })
    }

    pub fn block(&self) -> usize {
        self.block
    }

    pub fn fft_len(&self) -> usize {
        self.fft_len
    }

    pub fn max_sources(&self) -> usize {
        self.sources.len()
    }

    pub fn hrtf(&self) -> &Hrtf {
        &self.hrtf
    }

    /// Forget a slot's history, filters and gain.
    ///
    /// The renderer cannot see time pass between calls: a slot expects one
    /// `render_source` per block of continuous input. Feed silence (or packet-loss
    /// concealment) through gaps, or call this before input resumes after any
    /// discontinuity (a talker leaving or rejoining, skipped blocks, DTX without
    /// comfort noise). Otherwise up to `fft_len` samples of stale audio and the stale
    /// gain bleed into the first resumed block.
    pub fn reset_source(&mut self, slot: usize) {
        let s = &mut self.sources[slot];
        s.initialized = false;
        s.history.fill(0.0);
    }

    /// Render one block of mono `input` for source `slot` and *add* it into
    /// `out_left` / `out_right`. All slices are `block` long. Never allocates.
    pub fn render_source(
        &mut self,
        slot: usize,
        input: &[f32],
        params: SourceParams,
        out_left: &mut [f32],
        out_right: &mut [f32],
    ) {
        let b = self.block;
        assert!(input.len() == b && out_left.len() == b && out_right.len() == b);
        let n = self.fft_len;
        let Renderer { hrtf, evaluator, r2c, c2r, time, spectrum, product, r2c_scratch, c2r_scratch, ir, wet, sources, retarget_chord2, .. } = self;
        let s = &mut sources[slot];
        let dir = normalized(params.direction).unwrap_or(s.dir_current);
        let gain = if params.gain.is_finite() { params.gain } else if s.initialized { s.gain_prev } else { 0.0 };

        let mut load = |set: &mut [Vec<Complex<f32>>; 2], dir: [f32; 3]| {
            let [l, r] = &mut *ir;
            evaluator.evaluate(hrtf, dir, l, r);
            let scale = 1.0 / n as f32;
            for (ear, taps) in [&*l, &*r].into_iter().enumerate() {
                time.fill(0.0);
                for (t, v) in time.iter_mut().zip(taps) {
                    *t = v * scale;
                }
                r2c.process_with_scratch(time, &mut set[ear], r2c_scratch).expect("fft sizes");
            }
        };

        let mut fading = false;
        if !s.initialized {
            load(&mut s.filters[s.current], dir);
            s.dir_current = dir;
            s.gain_prev = gain;
            s.initialized = true;
        } else if chord2(s.dir_current, dir) > *retarget_chord2 {
            load(&mut s.filters[1 - s.current], dir);
            fading = true;
        }

        s.history.copy_within(b.., 0);
        s.history[n - b..].copy_from_slice(input);
        time.copy_from_slice(&s.history);
        r2c.process_with_scratch(time, spectrum, r2c_scratch).expect("fft sizes");

        let sets: &[usize] = if fading { &[0, 1] } else { &[0] };
        for &k in sets {
            let set = if k == 0 { s.current } else { 1 - s.current };
            for ear in 0..2 {
                for ((p, x), h) in product.iter_mut().zip(spectrum.iter()).zip(&s.filters[set][ear]) {
                    *p = x * h;
                }
                product[0].im = 0.0;
                let last = product.len() - 1;
                product[last].im = 0.0;
                c2r.process_with_scratch(product, time, c2r_scratch).expect("fft sizes");
                wet[2 * k + ear].copy_from_slice(&time[n - b..]);
            }
        }

        let g0 = s.gain_prev;
        let dg = (gain - g0) / b as f32;
        if fading {
            let inv = 1.0 / b as f32;
            for i in 0..b {
                let a = (i + 1) as f32 * inv;
                let g = g0 + dg * i as f32;
                out_left[i] += ((1.0 - a) * wet[0][i] + a * wet[2][i]) * g;
                out_right[i] += ((1.0 - a) * wet[1][i] + a * wet[3][i]) * g;
            }
            s.current = 1 - s.current;
            s.dir_current = dir;
        } else {
            for i in 0..b {
                let g = g0 + dg * i as f32;
                out_left[i] += wet[0][i] * g;
                out_right[i] += wet[1][i] * g;
            }
        }
        s.gain_prev = gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::direction_from_angles;

    pub(crate) fn hrtf() -> Hrtf {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/data/cipic_124.phrtf");
        Hrtf::from_bytes(&std::fs::read(path).unwrap()).unwrap()
    }

    fn run(r: &mut Renderer, blocks: &[(Vec<f32>, SourceParams)]) -> [Vec<f32>; 2] {
        let b = r.block();
        let mut outs = [vec![0.0f32; blocks.len() * b], vec![0.0f32; blocks.len() * b]];
        for (k, (input, p)) in blocks.iter().enumerate() {
            let [l, rr] = &mut outs;
            r.render_source(0, input, *p, &mut l[k * b..(k + 1) * b], &mut rr[k * b..(k + 1) * b]);
        }
        outs
    }

    #[test]
    fn impulse_reproduces_measured_hrir_across_blocks() {
        let h = hrtf();
        // 0 degrees azimuth, 0 elevation is a CIPIC measurement point.
        let dir = direction_from_angles(0.0, 0.0);
        let (idx, w) = h.weights(dir);
        let k = w.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        let expect = [h.ir(idx[k], 0).to_vec(), h.ir(idx[k], 1).to_vec()];
        let mut r = Renderer::new(h, RendererConfig::default()).unwrap();
        let b = r.block();
        let at = b - 100; // straddle a block boundary
        let p = SourceParams { direction: dir, gain: 1.0 };
        let mut first = vec![0.0f32; b];
        first[at] = 1.0;
        let outs = run(&mut r, &[(first, p), (vec![0.0; b], p), (vec![0.0; b], p)]);
        for ear in 0..2 {
            for (k, e) in expect[ear].iter().enumerate() {
                assert!((outs[ear][at + k] - e).abs() < 1e-4, "ear {ear} tap {k}");
            }
            assert!(outs[ear][..at].iter().all(|v| v.abs() < 1e-5));
        }
    }

    #[test]
    fn right_source_is_louder_and_earlier_in_right_ear() {
        let h = hrtf();
        let mut ev = Evaluator::new(&h);
        let n = h.interp_len();
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        ev.evaluate(&h, direction_from_angles(70.0, 3.0), &mut l, &mut r);
        let e = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>();
        assert!(e(&r) > 4.0 * e(&l));
        let peak = |v: &[f32]| v.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
        assert!(peak(&r) + 10 < peak(&l));
    }

    #[test]
    fn rejects_a_sample_rate_mismatch() {
        let h = hrtf();
        let err = Renderer::new(h, RendererConfig { sample_rate: 44100, ..Default::default() }).err();
        assert_eq!(err, Some(ConfigError::SampleRate { hrtf: 48000, stream: 44100 }));
    }

    #[test]
    fn non_finite_first_direction_or_gain_does_not_poison_the_slot() {
        let mut r = Renderer::new(hrtf(), RendererConfig::default()).unwrap();
        let b = r.block();
        let noise: Vec<f32> = (0..b).map(|i| ((i * 7919) % 101) as f32 / 101.0 - 0.5).collect();
        let bad = SourceParams { direction: [f32::NAN, 0.0, -1.0], gain: f32::NAN };
        let good = SourceParams { direction: direction_from_angles(30.0, 0.0), gain: 1.0 };
        let outs = run(&mut r, &[(noise.clone(), bad), (noise.clone(), good), (noise, good)]);
        assert!(outs.iter().flatten().all(|v| v.is_finite()));
        // The valid direction took effect: the source is to the right.
        let e = |v: &[f32]| v[2 * b..].iter().map(|x| x * x).sum::<f32>();
        assert!(e(&outs[1]) > e(&outs[0]));
    }
}

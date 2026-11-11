//! Per-source binaural rendering: overlap-save FFT convolution with a one-block
//! crossfade between filter sets when the direction moves (Mumble's scheme).

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::eval::godot_to_sofa;
use crate::Hrtf;

/// Mumble's `kDirChangeDotThreshold`: retarget once the direction has moved ~8.1°.
pub const MUMBLE_RETARGET_DOT: f32 = 0.99;

#[derive(Clone, Copy, Debug)]
pub struct RendererConfig {
    /// Samples per render call (fixed; 480 = 10 ms at 48 kHz).
    pub block: usize,
    /// Source slots allocated up front.
    pub max_sources: usize,
    /// Re-evaluate the HRIR when dot(current, new direction) falls below this.
    pub retarget_dot: f32,
}

impl Default for RendererConfig {
    fn default() -> Self {
        RendererConfig { block: 480, max_sources: 16, retarget_dot: MUMBLE_RETARGET_DOT }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SourceParams {
    /// Listener-local direction, Godot convention; any non-zero length.
    pub direction: [f32; 3],
    /// Linear gain for this block (e.g. from [`crate::voice_gain`]); ramped from the
    /// previous block's value.
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
    dir_next: [f32; 3],
    fading: bool,
    fade_pos: usize,
    gain_prev: f32,
}

pub struct Renderer {
    hrtf: Hrtf,
    block: usize,
    fft_len: usize,
    retarget_dot: f32,
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

fn normalized(d: [f32; 3]) -> [f32; 3] {
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if len < 1e-4 {
        // Coincident source: a stable forward direction (as Mumble does) rather
        // than an undefined lookup that retargets every block.
        [0.0, 0.0, -1.0]
    } else {
        [d[0] / len, d[1] / len, d[2] / len]
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

impl Renderer {
    pub fn new(hrtf: Hrtf, config: RendererConfig) -> Self {
        assert!(config.block > 0 && config.max_sources > 0);
        let fft_len = (config.block + hrtf.taps - 1).next_power_of_two();
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
                dir_current: [0.0; 3],
                dir_next: [0.0; 3],
                fading: false,
                fade_pos: 0,
                gain_prev: 0.0,
            })
            .collect();
        Renderer {
            block: config.block,
            fft_len,
            retarget_dot: config.retarget_dot,
            time: vec![0.0; fft_len],
            spectrum: vec![zero; bins],
            product: vec![zero; bins],
            r2c_scratch: r2c.make_scratch_vec(),
            c2r_scratch: c2r.make_scratch_vec(),
            ir: [vec![0.0; hrtf.taps], vec![0.0; hrtf.taps]],
            wet: std::array::from_fn(|_| vec![0.0; config.block]),
            r2c,
            c2r,
            sources,
            hrtf,
        }
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

    /// Forget a slot's history and filters (a talker left or restarted).
    pub fn reset_source(&mut self, slot: usize) {
        let s = &mut self.sources[slot];
        s.initialized = false;
        s.fading = false;
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
        let dir = normalized(params.direction);
        let Renderer { hrtf, r2c, c2r, time, spectrum, product, r2c_scratch, c2r_scratch, ir, wet, sources, retarget_dot, .. } = self;
        let s = &mut sources[slot];

        let mut load = |set: &mut [Vec<Complex<f32>>; 2], dir: [f32; 3]| {
            let [l, r] = &mut *ir;
            hrtf.evaluate(godot_to_sofa(dir), l, r);
            let scale = 1.0 / n as f32;
            for (ear, taps) in [&*l, &*r].into_iter().enumerate() {
                time.fill(0.0);
                for (t, v) in time.iter_mut().zip(taps) {
                    *t = v * scale;
                }
                r2c.process_with_scratch(time, &mut set[ear], r2c_scratch).expect("fft sizes");
            }
        };

        if !s.initialized {
            load(&mut s.filters[s.current], dir);
            s.dir_current = dir;
            s.gain_prev = params.gain;
            s.initialized = true;
        } else if !s.fading && dot(s.dir_current, dir) < *retarget_dot {
            load(&mut s.filters[1 - s.current], dir);
            s.dir_next = dir;
            s.fading = true;
            s.fade_pos = 0;
        }

        s.history.copy_within(b.., 0);
        s.history[n - b..].copy_from_slice(input);
        time.copy_from_slice(&s.history);
        r2c.process_with_scratch(time, spectrum, r2c_scratch).expect("fft sizes");

        let sets: &[usize] = if s.fading { &[0, 1] } else { &[0] };
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
        let dg = (params.gain - g0) / b as f32;
        if s.fading {
            let inv = 1.0 / b as f32;
            for i in 0..b {
                let a = ((s.fade_pos + i) as f32 * inv).min(1.0);
                let g = g0 + dg * i as f32;
                out_left[i] += ((1.0 - a) * wet[0][i] + a * wet[2][i]) * g;
                out_right[i] += ((1.0 - a) * wet[1][i] + a * wet[3][i]) * g;
            }
            s.fade_pos += b;
            if s.fade_pos >= b {
                s.current = 1 - s.current;
                s.dir_current = s.dir_next;
                s.fading = false;
            }
        } else {
            for i in 0..b {
                let g = g0 + dg * i as f32;
                out_left[i] += wet[0][i] * g;
                out_right[i] += wet[1][i] * g;
            }
        }
        s.gain_prev = params.gain;
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

    #[test]
    fn impulse_reproduces_measured_hrir_across_blocks() {
        let h = hrtf();
        // 0 degrees azimuth, 0 elevation is a CIPIC measurement point.
        let dir = direction_from_angles(0.0, 0.0);
        let idx = h.nearest(godot_to_sofa(dir));
        let expect = [h.ir(idx, 0).to_vec(), h.ir(idx, 1).to_vec()];
        let mut r = Renderer::new(h, RendererConfig::default());
        let b = r.block();
        let at = b - 100; // straddle a block boundary
        let mut outs = [vec![0.0f32; 3 * b], vec![0.0f32; 3 * b]];
        for blk in 0..3 {
            let mut input = vec![0.0f32; b];
            if blk == 0 {
                input[at] = 1.0;
            }
            let [l, rr] = &mut outs;
            r.render_source(0, &input, SourceParams { direction: dir, gain: 1.0 },
                &mut l[blk * b..(blk + 1) * b], &mut rr[blk * b..(blk + 1) * b]);
        }
        for ear in 0..2 {
            for (k, e) in expect[ear].iter().enumerate() {
                assert!((outs[ear][at + k] - e).abs() < 1e-5, "ear {ear} tap {k}");
            }
            assert!(outs[ear][..at].iter().all(|v| v.abs() < 1e-6));
        }
    }

    #[test]
    fn right_source_is_louder_and_earlier_in_right_ear() {
        let h = hrtf();
        let idx = h.nearest(godot_to_sofa(direction_from_angles(80.0, 0.0)));
        let (l, r) = (h.ir(idx, 0), h.ir(idx, 1));
        let e = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>();
        assert!(e(r) > 4.0 * e(l));
        let peak = |v: &[f32]| v.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
        assert!(peak(r) + 10 < peak(l));
    }
}

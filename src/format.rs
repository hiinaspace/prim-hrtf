// SPDX-License-Identifier: Apache-2.0
// Derived from Steam Audio 4.8.1 (hrtf_map.cpp, hrtf_database.cpp), Copyright 2017-2023 Valve Corporation,
// Apache-2.0. Modified: ported to Rust, baked data layout, realfft. See NOTICE and LICENSE-APACHE.

//! The baked `.phrtf` format written by `tools/sofa2phrtf.py` (layout documented there),
//! plus the per-measurement log-magnitude / unwrapped-phase spectra Steam Audio's
//! interpolator works on, computed once at load.

use std::fmt;

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

const MAGIC: &[u8; 8] = b"PHRTF\0\0\x02";
const MIN_HEADER: usize = 64;

#[derive(Debug, PartialEq, Eq)]
pub enum FormatError {
    BadMagic,
    Truncated,
    Invalid(&'static str),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::BadMagic => write!(f, "not a v2 .phrtf file"),
            FormatError::Truncated => write!(f, ".phrtf file is truncated"),
            FormatError::Invalid(why) => write!(f, "invalid .phrtf: {why}"),
        }
    }
}

impl std::error::Error for FormatError {}

/// A baked HRTF on a ring-major interaural grid (Steam Audio's layout): `lateral[i]`
/// is ring i's lateral angle, `elevation[i * per_ring + j]` the interaural elevation
/// of its j-th measurement, both in degrees.
#[derive(Clone)]
pub struct Hrtf {
    pub sample_rate: u32,
    pub taps: usize,
    pub source_sha256: [u8; 32],
    pub(crate) lateral: Vec<f32>,
    pub(crate) per_ring: usize,
    pub(crate) elevation: Vec<f32>,
    /// measurements x ears x taps
    pub(crate) ir: Vec<f32>,
    /// Interpolation FFT size: `taps` rounded up to a power of two, as Steam does.
    pub(crate) interp_len: usize,
    /// measurements x ears x (interp_len / 2 + 1): ln(|H| + 1e-9) and unwrapped phase.
    pub(crate) log_mag: Vec<f32>,
    pub(crate) phase: Vec<f32>,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Steam Audio's `HRTFDatabase::unwrapPhase`, for one spectrum: deltas between raw
/// neighbouring phases, folded into [-pi, pi] (keeping +pi for positive jumps), and
/// the cumulative correction of every jump of at least pi added to what follows.
pub(crate) fn unwrap_phase(data: &mut [f32]) {
    use std::f32::consts::PI;
    let mut cumulative = 0.0f32;
    let mut prev_raw = match data.first() {
        Some(v) => *v,
        None => return,
    };
    for v in data.iter_mut().skip(1) {
        let raw = *v;
        let delta = raw - prev_raw;
        let mut equivalent = (delta + PI) - ((delta + PI) / (2.0 * PI)).floor() * (2.0 * PI) - PI;
        if equivalent == -PI && delta > 0.0 {
            equivalent = PI;
        }
        if delta.abs() >= PI {
            cumulative += equivalent - delta;
        }
        *v = raw + cumulative;
        prev_raw = raw;
    }
}

impl Hrtf {
    pub fn from_bytes(b: &[u8]) -> Result<Self, FormatError> {
        if b.len() < MIN_HEADER {
            return Err(FormatError::Truncated);
        }
        if &b[..8] != MAGIC {
            return Err(FormatError::BadMagic);
        }
        let header = u32_at(b, 8) as usize;
        let sample_rate = u32_at(b, 12);
        let taps = u32_at(b, 16) as usize;
        let rings = u32_at(b, 20) as usize;
        let per_ring = u32_at(b, 24) as usize;
        let scale = f32_at(b, 28);
        if header < MIN_HEADER || taps == 0 || rings < 2 || per_ring == 0 || !(scale.is_finite() && scale > 0.0) {
            return Err(FormatError::Invalid("bad header fields"));
        }
        let m = rings * per_ring;
        let need = header + rings * 4 + m * 4 + m * 2 * taps * 2;
        if b.len() != need {
            return Err(FormatError::Truncated);
        }
        let mut o = header;
        let lateral: Vec<f32> = (0..rings).map(|i| f32_at(b, o + 4 * i)).collect();
        o += rings * 4;
        let elevation: Vec<f32> = (0..m).map(|i| f32_at(b, o + 4 * i)).collect();
        o += m * 4;
        if lateral.windows(2).any(|w| w[0].partial_cmp(&w[1]) != Some(std::cmp::Ordering::Less)) || lateral.iter().any(|a| !(-90.0..=90.0).contains(a)) {
            return Err(FormatError::Invalid("lateral angles must ascend within [-90, 90]"));
        }
        if elevation.chunks(per_ring).any(|r| r.windows(2).any(|w| w[0].partial_cmp(&w[1]) != Some(std::cmp::Ordering::Less)))
            || elevation.iter().any(|e| !e.is_finite())
        {
            return Err(FormatError::Invalid("elevations must ascend within each ring"));
        }
        let ir: Vec<f32> = b[o..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 * scale)
            .collect();

        // HRTFDatabase::decomposeToMagnitudePhase: zero-pad each HRIR to the next power
        // of two, take ln(|X| + 1e-9) and the phase, then unwrap along frequency.
        let interp_len = taps.next_power_of_two();
        let bins = interp_len / 2 + 1;
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(interp_len);
        let mut time = vec![0.0f32; interp_len];
        let mut spec = vec![Complex::new(0.0f32, 0.0); bins];
        let mut log_mag = vec![0.0f32; m * 2 * bins];
        let mut phase = vec![0.0f32; m * 2 * bins];
        for k in 0..m * 2 {
            time.fill(0.0);
            time[..taps].copy_from_slice(&ir[k * taps..(k + 1) * taps]);
            fft.process(&mut time, &mut spec).expect("fft sizes");
            let (lm, ph) = (&mut log_mag[k * bins..(k + 1) * bins], &mut phase[k * bins..(k + 1) * bins]);
            for ((l, p), x) in lm.iter_mut().zip(ph.iter_mut()).zip(&spec) {
                *l = (x.norm() + 1e-9).ln();
                *p = x.im.atan2(x.re);
            }
            unwrap_phase(ph);
        }
        Ok(Hrtf {
            sample_rate,
            taps,
            source_sha256: b[32..64].try_into().unwrap(),
            lateral,
            per_ring,
            elevation,
            ir,
            interp_len,
            log_mag,
            phase,
        })
    }

    /// Number of measurements.
    pub fn directions(&self) -> usize {
        self.elevation.len()
    }

    /// Length of the IRs [`crate::Evaluator`] produces (a power of two >= `taps`).
    pub fn interp_len(&self) -> usize {
        self.interp_len
    }

    /// HRIR of measurement `index` for `ear` (0 = left, 1 = right).
    pub fn ir(&self, index: usize, ear: usize) -> &[f32] {
        let start = (index * 2 + ear) * self.taps;
        &self.ir[start..start + self.taps]
    }

    /// (lateral, elevation) of measurement `index`, interaural degrees.
    pub fn grid_angles(&self, index: usize) -> (f32, f32) {
        (self.lateral[index / self.per_ring], self.elevation[index])
    }

    pub(crate) fn bins(&self) -> usize {
        self.interp_len / 2 + 1
    }
}

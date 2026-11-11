//! The baked `.phrtf` format written by `tools/sofa2phrtf.py` (layout documented there).

use std::fmt;

const MAGIC: &[u8; 8] = b"PHRTF\0\0\x01";
const HEADER: usize = 56;

#[derive(Debug, PartialEq, Eq)]
pub enum FormatError {
    BadMagic,
    Truncated,
    Invalid(&'static str),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::BadMagic => write!(f, "not a v1 .phrtf file"),
            FormatError::Truncated => write!(f, ".phrtf file is truncated"),
            FormatError::Invalid(why) => write!(f, "invalid .phrtf: {why}"),
        }
    }
}

impl std::error::Error for FormatError {}

/// A baked HRTF: unit measurement directions (SOFA frame: +x front, +y left, +z up),
/// libmysofa's neighbour table, and float HRIRs at one sample rate.
#[derive(Clone)]
pub struct Hrtf {
    pub sample_rate: u32,
    pub taps: usize,
    pub source_sha256: [u8; 32],
    pub(crate) dirs: Vec<[f32; 3]>,
    pub(crate) neighbours: Vec<[i32; 6]>,
    /// directions x ears x taps
    pub(crate) ir: Vec<f32>,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

impl Hrtf {
    pub fn from_bytes(b: &[u8]) -> Result<Self, FormatError> {
        if b.len() < HEADER {
            return Err(FormatError::Truncated);
        }
        if &b[..8] != MAGIC {
            return Err(FormatError::BadMagic);
        }
        let sample_rate = u32_at(b, 8);
        let taps = u32_at(b, 12) as usize;
        let m = u32_at(b, 16) as usize;
        let scale = f32::from_le_bytes(b[20..24].try_into().unwrap());
        if taps == 0 || m == 0 || !(scale.is_finite() && scale > 0.0) {
            return Err(FormatError::Invalid("empty table or bad scale"));
        }
        let need = HEADER + m * 12 + m * 24 + m * 2 * taps * 2;
        if b.len() != need {
            return Err(FormatError::Truncated);
        }
        let mut o = HEADER;
        let f32_at = |o: &mut usize| {
            let v = f32::from_le_bytes(b[*o..*o + 4].try_into().unwrap());
            *o += 4;
            v
        };
        let dirs: Vec<[f32; 3]> = (0..m)
            .map(|_| [f32_at(&mut o), f32_at(&mut o), f32_at(&mut o)])
            .collect();
        let mut neighbours = Vec::with_capacity(m);
        for _ in 0..m {
            let mut n = [0i32; 6];
            for v in &mut n {
                *v = i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
                o += 4;
                if *v < -1 || *v >= m as i32 {
                    return Err(FormatError::Invalid("neighbour index out of range"));
                }
            }
            neighbours.push(n);
        }
        let ir = b[o..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 * scale)
            .collect();
        Ok(Hrtf {
            sample_rate,
            taps,
            source_sha256: b[24..56].try_into().unwrap(),
            dirs,
            neighbours,
            ir,
        })
    }

    pub fn directions(&self) -> usize {
        self.dirs.len()
    }

    /// HRIR of measurement `index` for `ear` (0 = left, 1 = right).
    pub fn ir(&self, index: usize, ear: usize) -> &[f32] {
        let start = (index * 2 + ear) * self.taps;
        &self.ir[start..start + self.taps]
    }
}

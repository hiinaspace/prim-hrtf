//! HRIR evaluation: a port of libmysofa's `mysofa_getfilter_float` (lookup.c,
//! interpolate.c) as Mumble's HrtfSpatializer called it, for a single-radius table.

use crate::Hrtf;

/// Godot listener-local (+x right, +y up, -z forward) to SOFA (+x front, +y left, +z up).
pub fn godot_to_sofa(d: [f32; 3]) -> [f32; 3] {
    [-d[2], -d[0], d[1]]
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (x, y, z) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    (x * x + y * y + z * z).sqrt()
}

/// libmysofa's `fequals`.
fn fequals(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.00001
}

impl Hrtf {
    /// Nearest measurement to unit SOFA-frame direction `c` (libmysofa uses a
    /// kd-tree; a linear scan over ~1k points is cheap and only runs on retarget).
    pub fn nearest(&self, c: [f32; 3]) -> usize {
        let mut best = 0;
        let mut best_d = f32::INFINITY;
        for (i, p) in self.dirs.iter().enumerate() {
            let d = dist(c, *p);
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    }

    /// Write the HRIR pair for unit SOFA-frame direction `c` into `left`/`right`
    /// (each `taps` long). Matches `mysofa_interpolate`: the nearest measurement
    /// plus, per axis pair (azimuth, elevation, radius), the closer neighbour,
    /// weighted by inverse distance. Allocation-free.
    pub fn evaluate(&self, c: [f32; 3], left: &mut [f32], right: &mut [f32]) {
        let n = self.taps;
        assert!(left.len() == n && right.len() == n);
        let nearest = self.nearest(c);
        let d = dist(c, self.dirs[nearest]);
        if fequals(d, 0.0) {
            left.copy_from_slice(self.ir(nearest, 0));
            right.copy_from_slice(self.ir(nearest, 1));
            return;
        }
        let nb = self.neighbours[nearest];
        let mut weight = 1.0 / d;
        for (o, s) in left.iter_mut().zip(self.ir(nearest, 0)) {
            *o = s * weight;
        }
        for (o, s) in right.iter_mut().zip(self.ir(nearest, 1)) {
            *o = s * weight;
        }
        for pair in 0..3 {
            let (a, b) = (nb[2 * pair], nb[2 * pair + 1]);
            let pick = match (a >= 0, b >= 0) {
                (true, true) => {
                    let (da, db) = (dist(c, self.dirs[a as usize]), dist(c, self.dirs[b as usize]));
                    if fequals(da, db) {
                        None
                    } else if da < db {
                        Some((a as usize, da))
                    } else {
                        Some((b as usize, db))
                    }
                }
                (true, false) => Some((a as usize, dist(c, self.dirs[a as usize]))),
                (false, true) => Some((b as usize, dist(c, self.dirs[b as usize]))),
                (false, false) => None,
            };
            if let Some((index, dn)) = pick {
                let w = 1.0 / dn;
                for (o, s) in left.iter_mut().zip(self.ir(index, 0)) {
                    *o += s * w;
                }
                for (o, s) in right.iter_mut().zip(self.ir(index, 1)) {
                    *o += s * w;
                }
                weight += w;
            }
        }
        let inv = 1.0 / weight;
        for v in left.iter_mut().chain(right.iter_mut()) {
            *v *= inv;
        }
    }
}

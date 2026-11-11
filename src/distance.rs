//! Prim's received-voice distance law (project/net/avatar.gd `voice_falloff`,
//! defaults from docs/TESTING.md: 150 % volume, full level within 3 m, smooth fade
//! to silence at 15 m).

pub const DEFAULT_PERCENT: f32 = 150.0;
pub const DEFAULT_NEAR_M: f32 = 3.0;
pub const DEFAULT_FAR_M: f32 = 15.0;

/// Godot's `smoothstep(from, to, x)`.
pub fn smoothstep(from: f32, to: f32, x: f32) -> f32 {
    if (from - to).abs() <= f32::EPSILON * from.abs().max(to.abs()).max(1.0) {
        return if x <= from { 0.0 } else { 1.0 };
    }
    let s = ((x - from) / (to - from)).clamp(0.0, 1.0);
    s * s * (3.0 - 2.0 * s)
}

/// Full volume near the speaker, smooth fade to actual silence.
pub fn voice_falloff(distance: f32, near: f32, far: f32) -> f32 {
    1.0 - smoothstep(near, far.max(near + 0.5), distance)
}

/// Linear gain applied to a received voice, as `avatar.gd` sets `volume_linear`.
pub fn voice_gain(distance: f32, percent: f32, near: f32, far: f32) -> f32 {
    percent.max(0.0) / 100.0 * voice_falloff(distance, near, far)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_prim_defaults() {
        let g = |d| voice_gain(d, DEFAULT_PERCENT, DEFAULT_NEAR_M, DEFAULT_FAR_M);
        assert_eq!(g(0.0), 1.5);
        assert_eq!(g(3.0), 1.5);
        assert!((g(9.0) - 0.75).abs() < 1e-6);
        assert_eq!(g(15.0), 0.0);
        assert_eq!(g(40.0), 0.0);
        // inner/outer kept at least 0.5 m apart, as avatar.gd does
        assert_eq!(voice_falloff(3.25, 3.0, 2.0), 0.5);
    }
}

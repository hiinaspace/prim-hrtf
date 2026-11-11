//! Binaural point-source renderer for Prim voice.
//!
//! HRIR evaluation is a port of Steam Audio 4.8.1's bilinear interpolation, the path
//! Prim's Godot voice used before this crate: four measurements bracketing the
//! direction on CIPIC's interaural grid, blended as log magnitude and unwrapped phase
//! (see `eval.rs`). Convolution is overlap-save over a shared per-source input history,
//! re-evaluating the HRIR whenever the direction moves more than
//! [`DEFAULT_RETARGET_DEGREES`] and crossfading over one block. The HRTF comes from a
//! baked `.phrtf` file (see `tools/sofa2phrtf.py`), so there is no SOFA/HDF5 or Steam
//! Audio dependency at runtime.
//!
//! An earlier version ported Mumble's libmysofa evaluator (time-domain blend of
//! unaligned neighbours). The 2026-11-11 review found it notches off-grid directions
//! and crossfade midpoints by up to 20 dB; see `reviews/2026-11-11-evaluator/`.
//!
//! The render call never allocates. Distance gain is Prim's (full level to 3 m,
//! smoothstep fade to silence at 15 m, 150 % default).
//!
//! Coordinates: directions passed to the renderer are listener-local in Godot's
//! convention (+x right, +y up, -z forward). They need not be normalised.

mod distance;
mod eval;
mod format;
mod render;

pub use distance::{
    smoothstep, voice_falloff, voice_gain, DEFAULT_FAR_M, DEFAULT_NEAR_M, DEFAULT_PERCENT,
};
pub use eval::Evaluator;
pub use format::{FormatError, Hrtf};
pub use render::{ConfigError, Renderer, RendererConfig, SourceParams, DEFAULT_RETARGET_DEGREES};

/// Listener-local Godot direction for a source at `azimuth_deg` (positive = to the
/// listener's right) and `elevation_deg` (positive = up).
pub fn direction_from_angles(azimuth_deg: f32, elevation_deg: f32) -> [f32; 3] {
    let (az, el) = (azimuth_deg.to_radians(), elevation_deg.to_radians());
    [az.sin() * el.cos(), el.sin(), -az.cos() * el.cos()]
}

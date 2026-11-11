//! Binaural point-source renderer for Prim voice.
//!
//! A port of the approach S used in Mumble (branches `hrtf` / `spatial-room`):
//! libmysofa-style HRIR evaluation (nearest measurement plus up to three
//! inverse-distance-weighted neighbours, in the time domain), FFT convolution,
//! and a one-block linear crossfade when the direction moves past a threshold.
//! The HRTF comes from a baked `.phrtf` file (see `tools/sofa2phrtf.py`), so
//! there is no SOFA/HDF5 or libmysofa dependency at runtime.
//!
//! Differences from the Mumble code, all deliberate:
//! - the convolver is overlap-save over a shared per-source input history, so a
//!   newly selected filter produces correct output from its first block (Mumble's
//!   FFTConvolver restarted with an empty history on every re-init);
//! - the render call never allocates (Mumble re-initialised convolvers and grew
//!   `thread_local` vectors in the mixing loop);
//! - distance gain is Prim's (full level to 3 m, smoothstep fade to silence at
//!   15 m, 150 % default), not Mumble's.
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
pub use eval::godot_to_sofa;
pub use format::{FormatError, Hrtf};
pub use render::{Renderer, RendererConfig, SourceParams, MUMBLE_RETARGET_DOT};

/// Listener-local Godot direction for a source at `azimuth_deg` (positive = to the
/// listener's right) and `elevation_deg` (positive = up).
pub fn direction_from_angles(azimuth_deg: f32, elevation_deg: f32) -> [f32; 3] {
    let (az, el) = (azimuth_deg.to_radians(), elevation_deg.to_radians());
    [az.sin() * el.cos(), el.sin(), -az.cos() * el.cos()]
}

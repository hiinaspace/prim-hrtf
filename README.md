# prim-hrtf

A small, allocation-free binaural renderer for mono voice sources. Each source gets a
direction and a gain, and the renderer mixes it into a stereo output using a baked
HRTF. It is the voice spatializer for [Prim](https://github.com/hiinaspace/prim).

- **HRTF interpolation** is a port of Steam Audio 4.8.1's: bilinear weights on the
  interaural grid, then a blend of log magnitude and unwrapped phase. Against Steam
  Audio it measures within 0.4 µs ITD and 0.65 dB ILD at every tested direction.
- **Convolution** is FFT overlap-save. The HRIR is re-evaluated when a source's
  direction moves more than 0.1°, with a one-block crossfade.
- **No runtime dependencies** beyond `realfft`. There is no SOFA, HDF5 or Steam Audio
  at runtime: the HRTF comes from a baked `.phrtf` file.
- **Cost:** 7 moving sources take about 50 µs per 10 ms block on a desktop CPU.

## Usage

```rust
use prim_hrtf::{direction_from_angles, voice_gain, Hrtf, Renderer, RendererConfig, SourceParams};
use prim_hrtf::{DEFAULT_FAR_M, DEFAULT_NEAR_M, DEFAULT_PERCENT};

let hrtf = Hrtf::from_bytes(include_bytes!("../data/cipic_124.phrtf"))?;
let mut r = Renderer::new(hrtf, RendererConfig::default())?; // 48 kHz, 480-frame blocks

// Per audio callback, for each source slot:
let params = SourceParams {
    direction: direction_from_angles(30.0, 0.0), // listener-local, Godot axes
    gain: voice_gain(distance_m, DEFAULT_PERCENT, DEFAULT_NEAR_M, DEFAULT_FAR_M),
};
r.render_source(slot, &mono_block, params, &mut out_l, &mut out_r); // adds into out_*
```

**Conventions:**
- **Directions** are listener-local, using Godot's axes (+x right, +y up, -z forward). They don't need to be normalised.
- **Bad values:** a non-finite direction or gain is ignored, and the last valid one is kept.
- **Gaps:** feed silence through them, or call `reset_source(slot)` before the source resumes.
- **Sample rate:** the HRTF must be baked at the stream's rate, or `Renderer::new` returns an error.
- **`voice_gain`** is Prim's distance law: full level to 3 m, smoothstep to silence at 15 m.

## HRTF files

`tools/sofa2phrtf.py` bakes a SOFA file into `.phrtf`. The format is 48 kHz int16
with a global scale, laid out on Steam Audio's interaural grid. The bundled file is
`data/cipic_124.phrtf` (CIPIC subject 124, the same HRTF Steam Audio ships by default):
1250 directions × 218 taps, 1.1 MiB.

## Building

```sh
nix develop -c cargo test --release
nix develop .#aarch64 -c cargo build --release --target aarch64-unknown-linux-gnu
```

The aarch64 build, meant for the Steam Frame, links but hasn't been run on the device.

## Validation

The bench (`bench/run_bench.py`) renders noise and speech through Prim's old Godot +
Steam Audio path, through Mumble's HRTF spatializer and through prim-hrtf. It then
compares ITD, ILD and per-ear band levels, and writes blind A/B packets. Numbers and
known behaviour are in [docs/validation.md](docs/validation.md). Two things matter for
integration:

- **It is about 4.7 dB louder than Steam Audio at the same gain.** Match levels when
  swapping it in.
- **It keeps Steam Audio's two discontinuities:** at the exact interaural pole, and
  when a source more than about 45° below the ear axis crosses the side plane, where
  CIPIC has no measurements.

## Licence and attribution

Original code is released under the [Unlicense](UNLICENSE). `src/eval.rs` and parts of
`src/format.rs` are ports of Steam Audio (Copyright 2017-2023 Valve Corporation) and
remain Apache-2.0. Third-party terms and notices are in [LICENSE](LICENSE) and
[NOTICE](NOTICE).

The HRTF is from the CIPIC HRTF database, subject 124. Cite it as: V. R. Algazi,
R. O. Duda, D. M. Thompson and C. Avendano, "The CIPIC HRTF Database," IEEE WASPAA
2001, pp. 99-102. Its terms are in [LICENSE](LICENSE). The bench downloads LibriSpeech
test-clean (CC BY 4.0) at run time, and none of it is stored here.

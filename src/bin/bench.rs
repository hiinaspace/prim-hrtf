//! prim-hrtf-bench: fixed-seat renders and per-callback CPU timing.
//!
//!   prim-hrtf-bench render HRTF IN.f32 OUT.f32 AZIMUTH_DEG DISTANCE_M [ELEVATION_DEG]
//!       mono f32le 48 kHz in, interleaved stereo f32le out, Prim's default voice gain
//!   prim-hrtf-bench cpu HRTF [SOURCES=7] [SECONDS=60] [moving|static]
//!       per-callback timing for SOURCES talkers at 10 ms blocks; prints JSON

use std::time::Instant;

use prim_hrtf::{
    direction_from_angles, voice_gain, Hrtf, Renderer, RendererConfig, SourceParams,
    DEFAULT_FAR_M, DEFAULT_NEAR_M, DEFAULT_PERCENT,
};

fn load(path: &str) -> Hrtf {
    Hrtf::from_bytes(&std::fs::read(path).expect("read hrtf")).expect("parse hrtf")
}

fn read_f32(path: &str) -> Vec<f32> {
    std::fs::read(path)
        .expect("read input")
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn render(args: &[String]) {
    let hrtf = load(&args[0]);
    let input = read_f32(&args[1]);
    let az: f32 = args[3].parse().unwrap();
    let distance: f32 = args[4].parse().unwrap();
    let el: f32 = args.get(5).map(|s| s.parse().unwrap()).unwrap_or(0.0);
    let mut r = Renderer::new(hrtf, RendererConfig { max_sources: 1, ..Default::default() }).unwrap();
    let b = r.block();
    let params = SourceParams {
        direction: direction_from_angles(az, el),
        gain: voice_gain(distance, DEFAULT_PERCENT, DEFAULT_NEAR_M, DEFAULT_FAR_M),
    };
    // Flush the HRIR tail with one extra block of silence.
    let blocks = input.len().div_ceil(b) + 1;
    let mut out = Vec::with_capacity(blocks * b * 2);
    let (mut block, mut l, mut rr) = (vec![0.0; b], vec![0.0; b], vec![0.0; b]);
    for k in 0..blocks {
        block.fill(0.0);
        let start = (k * b).min(input.len());
        let end = ((k + 1) * b).min(input.len());
        block[..end - start].copy_from_slice(&input[start..end]);
        l.fill(0.0);
        rr.fill(0.0);
        r.render_source(0, &block, params, &mut l, &mut rr);
        for i in 0..b {
            out.extend_from_slice(&l[i].to_le_bytes());
            out.extend_from_slice(&rr[i].to_le_bytes());
        }
    }
    std::fs::write(&args[2], out).expect("write output");
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn cpu(args: &[String]) {
    let hrtf = load(&args[0]);
    let sources: usize = args.get(1).map(|s| s.parse().unwrap()).unwrap_or(7);
    let seconds: f64 = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(60.0);
    let moving = args.get(3).map(|s| s == "moving").unwrap_or(true);
    let mut r = Renderer::new(hrtf, RendererConfig { max_sources: sources, ..Default::default() }).unwrap();
    let b = r.block();
    let callbacks = (seconds * 48000.0 / b as f64) as usize;
    // Deterministic noise input per talker.
    let mut seed = 0x9e3779b97f4a7c15u64;
    let inputs: Vec<Vec<f32>> = (0..sources)
        .map(|_| {
            (0..b * 64)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5
                })
                .collect()
        })
        .collect();
    let (mut l, mut rr) = (vec![0.0f32; b], vec![0.0f32; b]);
    let mut times = Vec::with_capacity(callbacks);
    let mut sink = 0.0f32;
    for cb in 0..callbacks {
        // Moving: listener yaws 90 deg/s (a brisk head turn), 0.9 deg per callback,
        // past the retarget threshold, so every talker re-evaluates and crossfades
        // every callback: the worst case.
        let yaw = if moving { cb as f32 * 0.9 } else { 0.0 };
        let t0 = Instant::now();
        l.fill(0.0);
        rr.fill(0.0);
        for (slot, input) in inputs.iter().enumerate() {
            let seat = slot as f32 * 360.0 / sources as f32;
            let off = (cb % 64) * b;
            let params = SourceParams {
                direction: direction_from_angles(seat - yaw, 0.0),
                gain: voice_gain(1.5, DEFAULT_PERCENT, DEFAULT_NEAR_M, DEFAULT_FAR_M),
            };
            r.render_source(slot, &input[off..off + b], params, &mut l, &mut rr);
        }
        times.push(t0.elapsed().as_secs_f64() * 1e6);
        sink += l[0] + rr[b - 1];
    }
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let mut sorted = times.clone();
    sorted.sort_by(f64::total_cmp);
    let budget = b as f64 / 48000.0 * 1e6;
    println!(
        "{{\"sources\":{sources},\"block\":{b},\"fft_len\":{},\"callbacks\":{callbacks},\"moving\":{moving},\
\"mean_us\":{mean:.2},\"p50_us\":{:.2},\"p99_us\":{:.2},\"p999_us\":{:.2},\"max_us\":{:.2},\
\"budget_us\":{budget:.0},\"mean_budget_pct\":{:.3},\"arch\":\"{}\",\"checksum\":{sink}}}",
        r.fft_len(),
        percentile(&sorted, 0.5),
        percentile(&sorted, 0.99),
        percentile(&sorted, 0.999),
        sorted[sorted.len() - 1],
        mean / budget * 100.0,
        std::env::consts::ARCH,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("render") if args.len() >= 6 => render(&args[1..]),
        Some("cpu") if args.len() >= 2 => cpu(&args[1..]),
        _ => {
            eprintln!("usage: prim-hrtf-bench render HRTF IN.f32 OUT.f32 AZ_DEG DIST_M [EL_DEG]\n       prim-hrtf-bench cpu HRTF [SOURCES] [SECONDS] [moving|static]");
            std::process::exit(2);
        }
    }
}

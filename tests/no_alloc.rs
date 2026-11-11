//! The render call must not allocate: count allocations around a run with seven
//! sources, gain changes and continuous retargeting (every source crossfading).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use prim_hrtf::{direction_from_angles, Hrtf, Renderer, RendererConfig, SourceParams};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn render_source_never_allocates() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/data/cipic_124.phrtf");
    let hrtf = Hrtf::from_bytes(&std::fs::read(path).unwrap()).unwrap();
    let mut r = Renderer::new(hrtf, RendererConfig { max_sources: 7, ..Default::default() }).unwrap();
    let b = r.block();
    let input: Vec<f32> = (0..b).map(|i| ((i * 7919) % 101) as f32 / 101.0 - 0.5).collect();
    let (mut l, mut rr) = (vec![0.0; b], vec![0.0; b]);
    let before = ALLOCS.load(Ordering::SeqCst);
    for blk in 0..400 {
        l.fill(0.0);
        rr.fill(0.0);
        for slot in 0..7 {
            // 15 degrees per block: past the retarget threshold every time.
            let az = slot as f32 * 51.0 + blk as f32 * 15.0;
            let params = SourceParams {
                direction: direction_from_angles(az, (blk % 9) as f32 * 5.0 - 20.0),
                gain: 0.5 + 0.5 * ((blk + slot) % 3) as f32,
            };
            r.render_source(slot, &input, params, &mut l, &mut rr);
        }
    }
    let after = ALLOCS.load(Ordering::SeqCst);
    assert_eq!(after - before, 0, "render_source allocated");
    assert!(l.iter().chain(&rr).all(|v| v.is_finite()));
}

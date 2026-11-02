//! Peak heap of a compile as the AstroBox plugin runs it: `compilemem SKEL...` (atlas and pages
//! next to each skeleton). Prints the peak and every allocation of 16 MiB or more, in order.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static BIG: [AtomicUsize; 64] = [const { AtomicUsize::new(0) }; 64];
static BIGN: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        if l.size() >= 16 << 20 {
            let i = BIGN.fetch_add(1, Ordering::Relaxed);
            if i < 64 {
                BIG[i].store(l.size(), Ordering::Relaxed);
            }
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn mib(n: usize) -> f64 {
    n as f64 / 1048576.0
}

fn main() {
    let mut layers = Vec::new();
    for skel in std::env::args().skip(1) {
        let p = std::path::Path::new(&skel);
        let dir = p.parent().unwrap();
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let atlas = std::fs::read_to_string(dir.join(format!("{stem}.atlas"))).unwrap();
        let mut pages = Vec::new();
        for line in atlas.lines() {
            if line.trim().ends_with(".png") {
                pages.push((line.trim().to_string(), std::fs::read(dir.join(line.trim())).unwrap().into()));
            }
        }
        layers.push(bal2d_compiler::LayerInput { name: stem, skel: std::fs::read(&skel).unwrap(), atlas, pages });
    }
    let opts = bal2d_compiler::Options { budget: 800_000, ..Default::default() };
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let (bytes, _) = bal2d_compiler::compile(&layers, &opts, None).unwrap();
    let after_compile = PEAK.load(Ordering::Relaxed);
    let mut preview = bal2d_preview::PackPreview::load(&bytes).unwrap();
    preview.advance(7.0);
    let _ = preview.render();
    println!(
        "inputs {:.1} MiB; compile peak +{:.1} MiB; with preview +{:.1} MiB; big allocations (MiB): {:?}",
        mib(base),
        mib(after_compile - base),
        mib(PEAK.load(Ordering::Relaxed) - base),
        BIG[..BIGN.load(Ordering::Relaxed).min(64)].iter().map(|b| (mib(b.load(Ordering::Relaxed)) * 10.0).round() / 10.0).collect::<Vec<_>>()
    );
}

//! Heap a pack needs on the watch: `memory PACK...`. Counts what loading the lobby keeps (skeleton
//! data, animation state), the pages (GPU format, held in RAM), and the peak while it plays.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// The largest single allocation (the watch needs one free block that large).
static BIGGEST: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let n = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
        PEAK.fetch_max(n, Ordering::Relaxed);
        BIGGEST.fetch_max(l.size(), Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn kb(n: usize) -> f64 {
    n as f64 / 1024.0
}

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let base = LIVE.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        BIGGEST.store(0, Ordering::Relaxed);
        // The skeletons alone first (the watch parses them before the pages), then the idles.
        let mut src: &[u8] = &bytes;
        let (mut lobby, pages) = bal2d_core::Lobby::load_from(&mut src).unwrap();
        let parsed = LIVE.load(Ordering::Relaxed) - base;
        let parse_peak = PEAK.load(Ordering::Relaxed) - base;
        let parse_biggest = BIGGEST.load(Ordering::Relaxed);
        let skel_raw: usize = {
            let mut s: &[u8] = &bytes;
            let idx = bal2d_pack::Index::read(&mut s).unwrap();
            idx.entries.iter().filter(|e| e.kind == bal2d_pack::KIND_SKEL).map(|e| e.raw_len as usize).sum()
        };
        for li in 0..lobby.layers.len() {
            lobby.load_idle(&mut src, li);
        }
        let after_load = LIVE.load(Ordering::Relaxed) - base;
        let load_peak = PEAK.load(Ordering::Relaxed) - base;
        let page_bytes: usize = pages.iter().map(|p| p.section_len()).sum();
        lobby.start();
        let mut list = bal2d_render::DrawList::default();
        for _ in 0..600 {
            lobby.update(1.0 / 30.0);
            lobby.notices.clear();
            for li in 0..lobby.layers.len() {
                lobby.draw_list(li, &mut list);
            }
        }
        let playing = LIVE.load(Ordering::Relaxed) - base;
        let peak = PEAK.load(Ordering::Relaxed) - base;
        println!(
            "{path}: skeletons {:.0} KB (parse peak {:.0} KB, largest block {:.0} KB, section {:.0} KB); lobby {:.0} KB after load (load peak {:.0} KB), {:.0} KB playing (peak {:.0} KB); pages {:.0} KB; total about {:.0} KB",
            kb(parsed),
            kb(parse_peak),
            kb(parse_biggest),
            kb(skel_raw),
            kb(after_load),
            kb(load_peak),
            kb(playing),
            kb(peak),
            kb(page_bytes),
            kb(peak + page_bytes)
        );
        drop((lobby, pages, list));
    }
}

//! Freeing the intro once it has played (`Lobby::release_intro`): `introfree PACK...` plays two
//! copies of each pack, one freeing its intro, and checks every frame draws the same, and
//! reports when the intro went and the memory it gave back.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (mut a, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        let (mut b, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        a.start();
        b.start();
        let (mut la, mut lb) = (bal2d_render::DrawList::default(), bal2d_render::DrawList::default());
        let (mut freed_at, mut freed) = (None, 0usize);
        let mut differ = 0usize;
        let dt = 1.0 / 30.0;
        for f in 0..(90 * 30) {
            a.update(dt);
            b.update(dt);
            a.notices.clear();
            b.notices.clear();
            let before = LIVE.load(Ordering::Relaxed);
            if a.release_intro() {
                freed = before.saturating_sub(LIVE.load(Ordering::Relaxed));
                freed_at = Some(f as f32 * dt);
            }
            for li in 0..a.layers.len() {
                a.draw_list(li, &mut la);
                b.draw_list(li, &mut lb);
                if la.positions != lb.positions || la.batches.len() != lb.batches.len() {
                    differ += 1;
                }
            }
        }
        match freed_at {
            Some(t) => println!("{path}: intro freed at {t:.1} s, {} KB back; frames that differ: {differ}", freed / 1024),
            None => println!("{path}: intro never freed; frames that differ: {differ}"),
        }
    }
}

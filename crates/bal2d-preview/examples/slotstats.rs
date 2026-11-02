//! Two estimates per pack, after the intro: `slotstats PACK`
//! * static: batches whose world positions are bit-identical to the previous frame's (their GPU
//!   calls could be reused), as a share of batches and triangles;
//! * hidden: slots that change no pixel when left out, in any of a dozen sampled frames (half
//!   resolution), as a share of batches and triangles.
use bal2d_preview::raster::{self, Camera, Framebuffer};
use bal2d_render::DrawList;
use std::collections::{HashMap, HashSet};

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut p = bal2d_preview::PackPreview::load(&bytes).unwrap();
    p.advance(15.0);
    let n = p.lobby.layers.len();
    let mut lists: Vec<DrawList> = (0..n).map(|_| DrawList::default()).collect();
    // Static batches over 60 frames.
    let mut prev: HashMap<(usize, u16, u32), Vec<u32>> = HashMap::new();
    let (mut b_all, mut b_static, mut t_all, mut t_static) = (0usize, 0usize, 0usize, 0usize);
    for _ in 0..60 {
        p.advance(1.0 / 30.0);
        let mut cur = HashMap::new();
        for li in 0..n {
            p.lobby.draw_list(li, &mut lists[li]);
            let l = &lists[li];
            for b in &l.batches {
                let v0 = b.first_vertex as usize * 2;
                let pos: Vec<u32> = l.positions[v0..v0 + b.vertex_count as usize * 2].iter().map(|f| f.to_bits()).collect();
                let key = (li, b.slot, b.attachment);
                let tris = b.index_count as usize / 3;
                b_all += 1;
                t_all += tris;
                if prev.get(&key) == Some(&pos) {
                    b_static += 1;
                    t_static += tris;
                }
                cur.insert(key, pos);
            }
        }
        prev = cur;
    }
    println!("{path}");
    println!("  static batches: {:.0}% of batches, {:.0}% of triangles", 100.0 * b_static as f64 / b_all as f64, 100.0 * t_static as f64 / t_all as f64);
    // Hidden slots over 12 frames, 2 s apart.
    let (w, h) = (p.lobby.width as usize / 2, p.lobby.height as usize / 2);
    let c = p.lobby.camera;
    let cam = Camera { cx: c.cx, cy: c.cy, scale: c.scale * 0.5 };
    let mut seen: HashSet<(usize, u16)> = HashSet::new();
    let mut all_slots: HashMap<(usize, u16), usize> = HashMap::new();
    for _ in 0..12 {
        p.advance(2.0);
        for li in 0..n {
            p.lobby.draw_list(li, &mut lists[li]);
        }
        let render = |lists: &[DrawList], skip: Option<(usize, usize)>| {
            let mut fb = Framebuffer::new(w, h, [0.0, 0.0, 0.0, 1.0]);
            for li in 0..n {
                let mut l = lists[li].clone();
                if let Some((sl, sb)) = skip {
                    if sl == li {
                        l.batches.remove(sb);
                    }
                }
                raster::draw(&mut fb, &l, p.pages(li), cam);
            }
            fb.to_rgba8()
        };
        let full = render(&lists, None);
        for li in 0..n {
            for (bi, b) in lists[li].batches.iter().enumerate() {
                let key = (li, b.slot);
                *all_slots.entry(key).or_default() += b.index_count as usize / 3;
                if seen.contains(&key) {
                    continue;
                }
                let without = render(&lists, Some((li, bi)));
                if full.chunks_exact(4).zip(without.chunks_exact(4)).filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2)).count() > 2 {
                    seen.insert(key);
                }
            }
        }
    }
    // GPU calls per slot (one more frame, the watch's clustering at 1.25 px).
    let mut calls_by_slot: HashMap<(usize, u16), usize> = HashMap::new();
    let mut paths = bal2d_render::Paths::new();
    paths.tolerance = 1.25;
    let size = (p.lobby.width, p.lobby.height);
    for li in 0..n {
        p.lobby.draw_list(li, &mut lists[li]);
        let sizes: Vec<(f32, f32)> = p.pages(li).iter().map(|t| (t.width as f32, t.height as f32)).collect();
        for b in lists[li].batches.clone() {
            let mut one = lists[li].clone();
            one.batches = vec![b];
            let mut k = 0;
            paths.draw(li, &one, (c.cx, c.cy, c.scale), size, |pg| sizes.get(pg as usize).copied(), |_| k += 1);
            *calls_by_slot.entry((li, b.slot)).or_default() += k;
        }
    }
    let calls_all: usize = calls_by_slot.values().sum();
    let calls_hidden: usize = calls_by_slot.iter().filter(|(k, _)| !seen.contains(k)).map(|(_, v)| v).sum();
    println!("  GPU calls of hidden slots: {calls_hidden} of {calls_all} per frame");
    let hidden: Vec<_> = all_slots.keys().filter(|k| !seen.contains(k)).collect();
    let ht: usize = hidden.iter().map(|k| all_slots[k]).sum();
    let at: usize = all_slots.values().sum();
    println!("  hidden slots: {} of {} ({:.0}% of drawn triangles)", hidden.len(), all_slots.len(), 100.0 * ht as f64 / at as f64);
}

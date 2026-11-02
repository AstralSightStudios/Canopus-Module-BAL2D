//! A digest of every GPU call the renderer makes over a run: `calldump PACK [FRAMES] [TOL]`.
//! Prints one hash per frame and the total calls / path words, so a change to the skinning or
//! path code can be checked to emit exactly the same calls (same paths, bounds, matrices, tints).
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let frames: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(120);
    let tol: f32 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let bytes = std::fs::read(&path).unwrap();
    let (mut lobby, pages) = bal2d_core::Lobby::load(&bytes).unwrap();
    let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
    for p in &pages {
        sizes[p.layer as usize].push((p.header.width as f32, p.header.height as f32));
    }
    let mut list = bal2d_render::DrawList::default();
    let mut paths = bal2d_render::Paths::new();
    paths.tolerance = tol;
    let (w, h) = (lobby.width, lobby.height);
    let cam = lobby.camera;
    let mut total = DefaultHasher::new();
    let (mut calls, mut words) = (0usize, 0usize);
    for f in 0..frames {
        // A tap and a pat along the way, so the interaction paths are covered too.
        if f == frames / 3 {
            lobby.touch(bal2d_core::Touch::Down, w * 0.5, h * 0.75);
            lobby.touch(bal2d_core::Touch::Up, w * 0.5, h * 0.75);
        }
        lobby.update(1.0 / 30.0);
        lobby.notices.clear();
        let mut hs = DefaultHasher::new();
        for li in 0..lobby.layers.len() {
            lobby.draw_list(li, &mut list);
            for v in &list.positions {
                v.to_bits().hash(&mut hs);
            }
            let ps = &sizes[li];
            paths.draw(li, &list, (cam.cx, cam.cy, cam.scale), (w, h), |p| ps.get(p as usize).copied(), |c| {
                c.page.hash(&mut hs);
                (c.blend as u8).hash(&mut hs);
                c.color.map(f32::to_bits).hash(&mut hs);
                c.path.hash(&mut hs);
                c.bounds.hash(&mut hs);
                c.matrix.map(f32::to_bits).hash(&mut hs);
                calls += 1;
                words += c.path.len();
            });
        }
        let d = hs.finish();
        d.hash(&mut total);
        if f % 30 == 29 {
            println!("frame {f}: {d:016x}");
        }
    }
    println!("{path}: {calls} calls, {words} path words, digest {:016x}", total.finish());
}

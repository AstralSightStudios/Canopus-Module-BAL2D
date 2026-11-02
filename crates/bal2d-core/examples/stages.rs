//! Host CPU time per frame stage: `stages PACK...` plays each pack 600 frames after the intro and
//! times the animation update, the draw-list build and the path emit (clustering + path data) at
//! the device's tolerance. Host proportions only hint at the watch's (no f64 traps, other caches).
use std::time::Instant;

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (mut lobby, pages) = bal2d_core::Lobby::load(&bytes).unwrap();
        let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
        for p in &pages {
            sizes[p.layer as usize].push((p.header.width as f32, p.header.height as f32));
        }
        let size = (lobby.width, lobby.height);
        let c = lobby.camera;
        let cam = (c.cx, c.cy, c.scale);
        for _ in 0..450 {
            lobby.update(1.0 / 30.0);
            lobby.notices.clear();
        }
        let mut list = bal2d_render::DrawList::default();
        let mut paths = bal2d_render::Paths::new();
        paths.blits = true;
        paths.refits = std::env::var("REFITS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        paths.tolerance = std::env::var("TOL").ok().and_then(|v| v.parse().ok()).unwrap_or(0.75);
        let (mut tu, mut tb, mut tp) = (0f64, 0f64, 0f64);
        let (mut calls, mut words) = (0usize, 0usize);
        let frames = 600;
        for _ in 0..frames {
            let t0 = Instant::now();
            lobby.update(1.0 / 30.0);
            lobby.notices.clear();
            let t1 = Instant::now();
            tu += (t1 - t0).as_secs_f64();
            for li in 0..lobby.layers.len() {
                let t2 = Instant::now();
                lobby.draw_list(li, &mut list);
                let t3 = Instant::now();
                paths.draw(li, &list, cam, size, |p| sizes[li].get(p as usize).copied(), |call| {
                    calls += 1;
                    words += call.path.len();
                });
                tb += (t3 - t2).as_secs_f64();
                tp += t3.elapsed().as_secs_f64();
            }
        }
        let ms = |t: f64| t * 1000.0 / frames as f64;
        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
        println!(
            "{name:14} update {:.3} ms  build {:.3} ms  paths {:.3} ms  calls {}  rects {}  path halfwords {}",
            ms(tu),
            ms(tb),
            ms(tp),
            calls / frames,
            paths.rects as usize / frames,
            words / frames
        );
    }
}

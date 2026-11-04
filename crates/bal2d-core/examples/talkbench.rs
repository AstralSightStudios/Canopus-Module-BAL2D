//! Host time of a talk's frames drawn as the watch does (steady slots from the bake, the rest
//! live) and all live: `talkbench PACK...`.
fn main() {
    for path in std::env::args().skip(1) {
        let pack = std::fs::read(&path).unwrap();
        let (mut lobby, pages) = bal2d_core::Lobby::load(&pack).unwrap();
        lobby.start();
        let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
        for p in &pages {
            sizes[p.layer as usize].push((p.header.width as f32, p.header.height as f32));
        }
        let dt = 1.0 / 30.0;
        let mut t = 0.0;
        while !lobby.idle_ready() && t < 120.0 {
            lobby.update(dt);
            lobby.notices.clear();
            t += dt;
        }
        let (w, h) = (lobby.width, lobby.height);
        let cam = lobby.camera;
        let mut list = bal2d_render::DrawList::default();
        let mut paths = bal2d_render::Paths::new();
        paths.tolerance = 1.0;
        let (mut th, mut tl, mut ch, mut cl, mut n) = (0f64, 0f64, 0usize, 0usize, 0usize);
        for _ in 0..lobby.interactions.talks.len() {
            lobby.tap();
            let mut s = 0.0;
            while s < 15.0 {
                let mask = lobby.update_with(dt, true);
                lobby.notices.clear();
                s += dt;
                if lobby.hybrid == 0 {
                    if s > 1.0 && lobby.is_idle() && lobby.is_free() {
                        break;
                    }
                    continue;
                }
                n += 1;
                for li in 0..lobby.layers.len() {
                    let ps = &sizes[li];
                    if mask & (1 << li) != 0 {
                        let t0 = std::time::Instant::now();
                        lobby.draw_idle(li, (0.0, 0.0), (w, h), &|p| ps.get(p as usize).copied(), &mut |_| ch += 1);
                        let e = t0.elapsed().as_secs_f64();
                        if lobby.hybrid & (1 << li) != 0 {
                            th += e;
                            let t1 = std::time::Instant::now();
                            lobby.draw_list(li, &mut list);
                            paths.draw(li, &list, (cam.cx, cam.cy, cam.scale), (w, h), |p| ps.get(p as usize).copied(), |_| cl += 1);
                            tl += t1.elapsed().as_secs_f64();
                        } else {
                            ch -= 0;
                        }
                    }
                }
            }
            for _ in 0..45 {
                lobby.update_with(dt, true);
                lobby.notices.clear();
            }
        }
        let k = 1.0 / n.max(1) as f64;
        println!("{path}: {n} talk frames drawn partly from the bake: {:.1} us and {:.0} calls a frame, all live {:.1} us and {:.0} calls", th * k * 1e6, ch as f64 * k, tl * k * 1e6, cl as f64 * k);
    }
}

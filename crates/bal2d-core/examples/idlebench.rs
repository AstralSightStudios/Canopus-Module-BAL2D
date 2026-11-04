//! Host time of drawing the baked idles (what the watch's CPU does per frame besides the
//! driver): `idlebench PACK... [--seconds S]`. Plays into the idle loops, then times
//! `draw_idle` for every layer, 30 frames a second.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: f32 = args.iter().position(|a| a == "--seconds").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(20.0);
    for path in args.iter().filter(|a| a.ends_with(".bal2d")) {
        let pack = std::fs::read(path).unwrap();
        let (mut lobby, pages) = bal2d_core::Lobby::load(&pack).unwrap();
        lobby.start();
        let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
        for p in &pages {
            sizes[p.layer as usize].push((p.header.width as f32, p.header.height as f32));
        }
        let mut t = 0.0;
        while !lobby.idle_ready() && t < 120.0 {
            lobby.update(1.0 / 30.0);
            lobby.notices.clear();
            t += 1.0 / 30.0;
        }
        let (w, h) = (lobby.width, lobby.height);
        let frames = (seconds * 30.0) as usize;
        let (mut calls, mut words, mut dt, mut runs) = (0usize, 0usize, 0f64, 0usize);
        for _ in 0..frames {
            lobby.update_with(1.0 / 30.0, true);
            lobby.notices.clear();
            let t0 = std::time::Instant::now();
            // Runs of calls on one page and blend (what the watch batches into one draw).
            let mut last = None;
            for li in 0..lobby.layers.len() {
                let ps = &sizes[li];
                lobby.draw_idle(li, (0.0, 0.0), (w, h), &|p| ps.get(p as usize).copied(), &mut |c| {
                    calls += 1;
                    words += c.path.len();
                    if last != Some((li, c.page, c.blend as u8)) {
                        runs += 1;
                        last = Some((li, c.page, c.blend as u8));
                    }
                });
            }
            dt += t0.elapsed().as_secs_f64();
        }
        let f = frames as f64;
        let work: Vec<f64> = (0..4).map(|k| lobby.idle.iter().flatten().map(|l| l.work[k] as f64).sum::<f64>() / f).collect();
        println!("{path}: {:.1} us/frame, {:.0} calls in {:.1} page/blend runs, {:.0} path words a frame; vertices {:.0}, clipped {:.1} ({:.0} words), covered {:.1}", dt / f * 1e6, calls as f64 / f, runs as f64 / f, words as f64 / f, work[0], work[1], work[3], work[2]);
    }
}

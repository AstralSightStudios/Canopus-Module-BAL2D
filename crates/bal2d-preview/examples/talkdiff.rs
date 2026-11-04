//! A talk drawn as the watch draws it (steady slots from the bake, the rest live) against the
//! same frames all live: `talkdiff PACK [DIR]`. Taps into each talk and compares frames every
//! 0.2 s (PSNR, pixels differing by more than 2 levels); with DIR, writes the worst pair.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut pv = bal2d_preview::PackPreview::load(&std::fs::read(&args[0]).unwrap()).unwrap();
    let mut t = 0.0;
    while !pv.lobby.idle_ready() && t < 120.0 {
        pv.advance(0.5);
        t += 0.5;
    }
    let (w, h) = pv.size();
    let talks = pv.lobby.interactions.talks.len();
    let (mut worst, mut wa, mut wb) = (f64::INFINITY, Vec::new(), Vec::new());
    let (mut frames, mut hybrid_frames, mut sum_mse, mut differing) = (0usize, 0usize, 0f64, 0usize);
    for _ in 0..talks {
        pv.lobby.tap();
        let mut s = 0.0;
        while s < 15.0 {
            pv.advance(0.2);
            s += 0.2;
            let main = pv.lobby.interactions.layer as usize;
            let hybrid = pv.lobby.layer_hybrid_ready(main);
            pv.use_idle = true;
            let a = pv.render();
            pv.use_idle = false;
            let b = pv.render();
            pv.use_idle = true;
            frames += 1;
            hybrid_frames += hybrid as usize;
            let mut mse = 0f64;
            for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
                let d = (0..3).map(|k| (p[k] as i32 - q[k] as i32).abs()).max().unwrap();
                differing += (d > 2) as usize;
                mse += (0..3).map(|k| (p[k] as f64 - q[k] as f64).powi(2)).sum::<f64>() / 3.0;
            }
            mse /= (w * h) as f64;
            sum_mse += mse;
            let psnr = if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() };
            if hybrid && psnr < worst {
                (worst, wa, wb) = (psnr, a, b);
            }
            if s > 1.0 && pv.lobby.is_idle() && pv.lobby.is_free() {
                break;
            }
        }
        pv.advance(1.5);
    }
    let mean = sum_mse / frames.max(1) as f64;
    println!(
        "{}: {frames} talk frames, {hybrid_frames} drawn partly from the bake; vs all live: mean PSNR {:.2} dB, worst {:.2} dB, {:.1} pixels a frame differ by > 2",
        args[0],
        if mean == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mean).log10() },
        worst,
        differing as f64 / frames.max(1) as f64
    );
    if let (Some(dir), false) = (args.get(1), wa.is_empty()) {
        for (tag, img) in [("hybrid", &wa), ("live", &wb)] {
            let png = bal2d_compiler::image::encode_png(&bal2d_compiler::image::Rgba { width: w as u32, height: h as u32, data: img.clone() });
            std::fs::write(format!("{dir}/talk-{tag}.png"), png).unwrap();
        }
    }
}

//! Baked idle against the live lobby: `idlecheck PACK [OUT_DIR]` plays into the idle loop, then
//! renders frames through one loop both ways (live: grouped like the watch at its finest
//! tolerance; baked: the pack's idle calls) and prints the PSNR of each, writing the worst pair
//! side by side to OUT_DIR.
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mut se = 0f64;
    let mut n = 0f64;
    for (x, y) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for k in 0..3 {
            let d = x[k] as f64 - y[k] as f64;
            se += d * d;
            n += 1.0;
        }
    }
    if se == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / (se / n)).log10() }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).unwrap();
    let mut pv = bal2d_preview::PackPreview::load(&bytes).unwrap();
    let baked_layers = pv.lobby.idle.iter().filter(|b| b.is_some()).count();
    let mut t = 0.0;
    while !pv.lobby.idle_ready() && t < 120.0 {
        pv.advance(0.5);
        t += 0.5;
    }
    println!("{} layers baked of {}; idle ready after {t:.1} s: {}", baked_layers, pv.lobby.layers.len(), pv.lobby.idle_ready());
    let period = pv.lobby.idle.iter().flatten().map(|b| b.bake.period).fold(0.0, f32::max).min(12.0);
    let (w, h) = pv.size();
    let mut worst = (99.0, Vec::new(), Vec::new(), 0.0);
    let mut sum = 0.0;
    let n = 24;
    for i in 0..n {
        pv.use_idle = false;
        let live = pv.render();
        pv.use_idle = true;
        let ready = pv.lobby.idle_ready();
        let baked = pv.render();
        let p = psnr(&live, &baked);
        sum += p;
        if p < worst.0 {
            worst = (p, live, baked, i as f32 * period / n as f32);
        }
        if !ready {
            println!("frame {i}: not idle-ready");
        }
        pv.advance(period / n as f32);
    }
    println!("PSNR baked vs live: mean {:.2} dB, worst {:.2} dB at +{:.2} s", sum / n as f64, worst.0, worst.3);
    // During a talk: the layers not talking stay on their bakes.
    pv.lobby.tap();
    let mut talk = (0.0, 0, 0);
    for _ in 0..12 {
        pv.advance(0.25);
        let ready = (0..pv.lobby.layers.len()).filter(|&i| pv.lobby.layer_idle_ready(i)).count();
        pv.use_idle = false;
        let live = pv.render();
        pv.use_idle = true;
        let mixed = pv.render();
        talk.0 += psnr(&live, &mixed);
        talk.1 += 1;
        talk.2 += ready;
    }
    println!("talk: PSNR {:.2} dB with {:.1} of {} layers on bakes", talk.0 / talk.1 as f64, talk.2 as f32 / talk.1 as f32, pv.lobby.layers.len());
    for li in 0..pv.lobby.layers.len() {
        pv.use_idle = false;
        let live = pv.render_layers(1 << li);
        pv.use_idle = true;
        let baked = pv.render_layers(1 << li);
        println!("  layer {li} alone now: {:.2} dB", psnr(&live, &baked));
        if let Some(out) = args.get(1) {
            let mut side = vec![0u8; w * 2 * h * 4];
            for y in 0..h {
                side[y * w * 8..y * w * 8 + w * 4].copy_from_slice(&live[y * w * 4..(y + 1) * w * 4]);
                side[y * w * 8 + w * 4..(y + 1) * w * 8].copy_from_slice(&baked[y * w * 4..(y + 1) * w * 4]);
            }
            let png = bal2d_compiler::image::encode_png(&bal2d_compiler::image::Rgba { width: (w * 2) as u32, height: h as u32, data: side });
            std::fs::write(format!("{out}/idlecheck-layer{li}.png"), png).unwrap();
        }
    }
    if let Some(out) = args.get(1) {
        let mut side = vec![0u8; w * 2 * h * 4];
        for y in 0..h {
            side[y * w * 8..y * w * 8 + w * 4].copy_from_slice(&worst.1[y * w * 4..(y + 1) * w * 4]);
            side[y * w * 8 + w * 4..(y + 1) * w * 8].copy_from_slice(&worst.2[y * w * 4..(y + 1) * w * 4]);
        }
        let png = bal2d_compiler::image::encode_png(&bal2d_compiler::image::Rgba { width: (w * 2) as u32, height: h as u32, data: side });
        std::fs::write(format!("{out}/idlecheck.png"), png).unwrap();
    }
}

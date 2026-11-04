//! Whether two compiles of a lobby draw their baked idles alike: `idlediff A.bal2d B.bal2d [N]
//! [DIR]` plays both into their idle loops and compares N frames 0.37 s apart (PSNR, pixels that
//! differ by more than 2 levels, the largest difference); with DIR, writes the worst pair.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let load = |p: &str| bal2d_preview::PackPreview::load(&std::fs::read(p).unwrap()).unwrap();
    let (mut a, mut b) = (load(&args[0]), load(&args[1]));
    let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(40);
    let mut t = 0.0;
    while !(a.lobby.idle_ready() && b.lobby.idle_ready()) && t < 120.0 {
        a.advance(0.5);
        b.advance(0.5);
        t += 0.5;
    }
    let (w, h) = a.size();
    let mut worst = (f64::INFINITY, 0usize, Vec::new(), Vec::new());
    let (mut sum_mse, mut differing, mut largest) = (0f64, 0usize, 0i32);
    for i in 0..n {
        let (x, y) = (a.render(), b.render());
        let mut mse = 0f64;
        for (p, q) in x.chunks_exact(4).zip(y.chunks_exact(4)) {
            let d = (0..3).map(|k| (p[k] as i32 - q[k] as i32).abs()).max().unwrap();
            largest = largest.max(d);
            differing += (d > 2) as usize;
            mse += (0..3).map(|k| (p[k] as f64 - q[k] as f64).powi(2)).sum::<f64>() / 3.0;
        }
        mse /= (w * h) as f64;
        sum_mse += mse;
        let psnr = if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() };
        if psnr < worst.0 {
            worst = (psnr, i, x, y);
        }
        a.advance(0.37);
        b.advance(0.37);
    }
    let mean = sum_mse / n as f64;
    println!(
        "idle frames {n}: mean PSNR {:.2} dB, worst {:.2} dB (frame {}), {:.1} pixels a frame differ by > 2, largest difference {largest}",
        if mean == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mean).log10() },
        worst.0,
        worst.1,
        differing as f32 / n as f32
    );
    {
        let (x, y) = (&worst.2, &worst.3);
        let mut pts = Vec::new();
        for i in 0..w * h {
            let d = (0..3).map(|k| (x[i * 4 + k] as i32 - y[i * 4 + k] as i32).abs()).max().unwrap();
            if d > 2 {
                pts.push((i % w, i / w, d));
            }
        }
        if !pts.is_empty() {
            let (x0, x1) = (pts.iter().map(|p| p.0).min().unwrap(), pts.iter().map(|p| p.0).max().unwrap());
            let (y0, y1) = (pts.iter().map(|p| p.1).min().unwrap(), pts.iter().map(|p| p.1).max().unwrap());
            println!("worst frame: {} pixels in [{x0}, {y0}, {x1}, {y1}], e.g. {:?}", pts.len(), &pts[..pts.len().min(12)]);
        }
    }
    if let Some(dir) = args.get(3) {
        for (tag, img) in [("a", &worst.2), ("b", &worst.3)] {
            let png = bal2d_compiler::image::encode_png(&bal2d_compiler::image::Rgba { width: w as u32, height: h as u32, data: img.clone() });
            std::fs::write(format!("{dir}/idlediff-{tag}.png"), png).unwrap();
        }
    }
}

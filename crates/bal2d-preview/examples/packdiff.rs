//! How close two compiles of a lobby look: `packdiff A.bal2d B.bal2d [DIR]` renders both at the same
//! moments (3, 7 and 12 s) and prints the PSNR of each frame (inf: identical); with DIR, writes
//! the frames there (a-T.png, b-T.png).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let load = |p: &str| bal2d_preview::PackPreview::load(&std::fs::read(p).unwrap()).unwrap();
    let (mut a, mut b) = (load(&args[0]), load(&args[1]));
    let mut t = 0.0;
    let mut out = Vec::new();
    for at in [3.0f32, 7.0, 12.0] {
        a.advance(at - t);
        b.advance(at - t);
        t = at;
        let (x, y) = (a.render(), b.render());
        if let Some(dir) = args.get(2) {
            let (w, h) = a.size();
            for (tag, img) in [("a", &x), ("b", &y)] {
                let png = bal2d_compiler::image::encode_png(&bal2d_compiler::image::Rgba { width: w as u32, height: h as u32, data: img.clone() });
                std::fs::write(format!("{dir}/{tag}-{at}.png"), png).unwrap();
            }
        }
        let mse: f64 = x.iter().zip(&y).map(|(p, q)| (*p as f64 - *q as f64).powi(2)).sum::<f64>() / x.len() as f64;
        out.push(if mse == 0.0 { "inf".to_string() } else { format!("{:.1}", 10.0 * (255.0f64 * 255.0 / mse).log10()) });
    }
    println!("{} dB", out.join(" / "));
}

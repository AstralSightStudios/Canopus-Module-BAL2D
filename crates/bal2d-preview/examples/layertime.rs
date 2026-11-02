//! When each layer of a pack shows during its first seconds: `layertime PACK [SECONDS]`.
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let secs: f32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(6.0);
    let bytes = std::fs::read(&path).unwrap();
    let mut p = bal2d_preview::PackPreview::load(&bytes).unwrap();
    p.gpu_tolerance = None;
    let n = p.lobby.layers.len();
    let mut t = 0.0;
    while t < secs {
        let all = p.render();
        let mut line = format!("t={t:5.2}s:");
        for li in 0..n {
            let without = p.render_layers(!(1u32 << li));
            let diff = all.chunks_exact(4).zip(without.chunks_exact(4)).filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2)).count();
            line += &format!("  layer {li}: {diff:6} px");
        }
        println!("{line}");
        p.advance(0.2);
        t += 0.2;
    }
}

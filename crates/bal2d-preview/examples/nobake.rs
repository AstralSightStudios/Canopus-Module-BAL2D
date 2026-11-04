//! The baked idle drawn without the background image (as a watch that leaves it out) against
//! the same frames with it: `nobake PACK`.
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut a = bal2d_preview::PackPreview::load(&bytes).unwrap();
    let mut b = bal2d_preview::PackPreview::load(&bytes).unwrap();
    b.lobby.bake = None;
    let mut t = 0.0;
    while !(a.lobby.idle_ready() && b.lobby.idle_ready()) && t < 120.0 {
        a.advance(0.5);
        b.advance(0.5);
        t += 0.5;
    }
    let (w, h) = a.size();
    let mut worst = f64::INFINITY;
    for _ in 0..10 {
        let (x, y) = (a.render(), b.render());
        let mse: f64 = x.chunks_exact(4).zip(y.chunks_exact(4)).map(|(p, q)| (0..3).map(|k| (p[k] as f64 - q[k] as f64).powi(2)).sum::<f64>() / 3.0).sum::<f64>() / (w * h) as f64;
        worst = worst.min(if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() });
        a.advance(0.7);
        b.advance(0.7);
    }
    println!("{path}: without the background image vs with it: worst PSNR {worst:.2} dB (idle ready {})", b.lobby.idle_ready());
}

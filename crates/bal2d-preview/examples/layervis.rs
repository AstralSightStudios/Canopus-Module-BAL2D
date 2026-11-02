//! How much each layer of a pack shows: `layervis PACK` renders the pack with all layers and with
//! each layer left out, at several moments, and counts the pixels that differ.
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut p = bal2d_preview::PackPreview::load(&bytes).unwrap();
    let n = p.lobby.layers.len();
    for t in [2.0f32, 6.0, 12.0, 20.0, 30.0] {
        let elapsed: f32 = 0.0;
        let _ = elapsed;
        p.advance(if t == 2.0 { 2.0 } else { 4.0 });
        let all = p.render();
        let mut line = format!("t~{t:4.0}s:");
        for li in 0..n {
            let without = p.render_layers(!(1u32 << li));
            let diff = all.chunks_exact(4).zip(without.chunks_exact(4)).filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2)).count();
            line += &format!("  layer {li} shows {:5.1}%", 100.0 * diff as f32 / (all.len() / 4) as f32);
        }
        println!("{line}");
    }
}

//! How many distinct tint sequences the idle loop's static slots follow: `tintpatterns PACK`
//! samples one idle loop at 30 fps and groups slots whose vertices never move by their tint
//! sequence (quantized to 8 bits).
use std::collections::HashMap;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).unwrap();
    let (mut lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
    for _ in 0..900 {
        lobby.update(1.0 / 30.0);
    }
    let mut list = bal2d_render::DrawList::default();
    let period = lobby.layers.iter().map(|l| l.state.entry(l.state.current(0).unwrap()).animation_end).fold(0.0, f32::max).min(20.0);
    let frames = (period * 30.0) as usize;
    // (layer, slot) -> (first positions, moved, tint sequence)
    let mut slots: HashMap<(usize, u16), (Vec<f32>, bool, Vec<[u8; 4]>)> = HashMap::new();
    for _ in 0..frames {
        for li in 0..lobby.layers.len() {
            lobby.draw_list(li, &mut list);
            for b in &list.batches {
                let v0 = b.first_vertex as usize * 2;
                let pos = &list.positions[v0..v0 + b.vertex_count as usize * 2];
                let e = slots.entry((li, b.slot)).or_insert_with(|| (pos.to_vec(), false, Vec::new()));
                if e.0.len() != pos.len() || e.0.iter().zip(pos).any(|(a, b)| (a - b).abs() > 0.01) {
                    e.1 = true;
                }
                e.2.push(b.color.map(|c| (c * 255.0 + 0.5) as u8));
            }
        }
        lobby.update(1.0 / 30.0);
    }
    let still: Vec<_> = slots.iter().filter(|(_, v)| !v.1).collect();
    let mut patterns: HashMap<Vec<[u8; 4]>, usize> = HashMap::new();
    for (_, v) in &still {
        *patterns.entry(v.2.clone()).or_default() += 1;
    }
    let animated = patterns.keys().filter(|p| p.iter().any(|c| *c != p[0])).count();
    let mut all: HashMap<Vec<[u8; 4]>, usize> = HashMap::new();
    for v in slots.values() {
        *all.entry(v.2.clone()).or_default() += 1;
    }
    let mut counts: Vec<usize> = all.values().copied().collect();
    counts.sort_unstable_by(|a, b| b.cmp(a));
    println!("all {} slots: {} distinct tint sequences; largest groups {:?}", slots.len(), all.len(), &counts[..counts.len().min(10)]);
    println!("{} slots drawn, {} never move; their tint sequences: {} distinct ({} change over the loop); {} frames", slots.len(), still.len(), patterns.len(), animated, frames);
}

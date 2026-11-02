//! Index patterns of 2-triangle batches: `quadidx PACK` (after the intro, one frame).
use std::collections::BTreeMap;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let (mut lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
    for _ in 0..450 {
        lobby.update(1.0 / 30.0);
        lobby.notices.clear();
    }
    let mut list = bal2d_render::DrawList::default();
    let mut pats: BTreeMap<(u32, Vec<u16>), usize> = BTreeMap::new();
    for li in 0..lobby.layers.len() {
        lobby.draw_list(li, &mut list);
        for b in &list.batches {
            if b.index_count <= 6 {
                let idx = list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize].to_vec();
                *pats.entry((b.vertex_count, idx)).or_default() += 1;
            }
        }
    }
    for ((n, idx), c) in pats {
        println!("{c:5} batches: {n} vertices, indices {idx:?}");
    }
}

//! Which slots the idle loop clips: `clipslots PACK` lists clipping attachments drawn in the idle
//! loop and the batches whose geometry is not their attachment's own (clipped).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).unwrap();
    let (mut lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
    for _ in 0..1500 {
        lobby.update(1.0 / 30.0);
    }
    let mut list = bal2d_render::DrawList::default();
    for li in 0..lobby.layers.len() {
        let mut clipped = std::collections::BTreeMap::new();
        let mut total = 0;
        for _ in 0..90 {
            lobby.draw_list(li, &mut list);
            total = list.batches.len();
            let data = &lobby.layers[li].data;
            for b in &list.batches {
                let own = bal2d_core::idle::geometry(data, b.attachment);
                let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
                let mismatch = own.is_none_or(|(_, t, v)| t != idx || v != b.vertex_count as usize);
                if mismatch {
                    *clipped.entry(data.slots[b.slot as usize].name.clone()).or_insert(0) += b.index_count / 3;
                }
            }
            lobby.update(1.0 / 30.0);
        }
        println!("layer {li}: {} batches, clipped slots (triangles over 90 frames): {:?}", total, clipped);
    }
}

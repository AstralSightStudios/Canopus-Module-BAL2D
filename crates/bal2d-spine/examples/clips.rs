//! Lists clipping attachments: `clips SKEL`.
use bal2d_spine::*;
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let d = read_skeleton(&std::fs::read(&a[0]).unwrap(), 1.0).unwrap();
    for skin in &d.skins {
        for (slot, name, id) in &skin.attachments {
            if let AttachmentKind::Clipping(c) = &d.attachments[*id as usize].kind {
                let s = *slot as usize;
                let order: Vec<usize> = (0..d.slots.len()).collect();
                let from = order.iter().position(|&x| x == s).unwrap();
                let to = c.end_slot as usize;
                println!(
                    "slot {s} {} clip {name}: {} vertices, until slot {to} {} ({} slots), weighted {}",
                    d.slots[s].name,
                    c.vertices.vertex_count(),
                    d.slots[to].name,
                    to as i64 - from as i64,
                    c.vertices.is_weighted()
                );
            }
        }
    }
}

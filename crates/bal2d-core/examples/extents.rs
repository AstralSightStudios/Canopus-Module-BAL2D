//! Screen extents of each batch in the idle loop: `extents PACK` lists batches reaching beyond
//! the +-4095 px S16 path range.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).unwrap();
    let (mut lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
    for _ in 0..600 {
        lobby.update(1.0 / 30.0);
    }
    let c = lobby.camera;
    let (w2, h2) = (lobby.width * 0.5, lobby.height * 0.5);
    let mut list = bal2d_render::DrawList::default();
    for li in 0..lobby.layers.len() {
        lobby.draw_list(li, &mut list);
        for b in &list.batches {
            let v0 = b.first_vertex as usize;
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for i in 0..b.vertex_count as usize {
                let p = [(list.positions[(v0 + i) * 2] - c.cx) * c.scale + w2, h2 - (list.positions[(v0 + i) * 2 + 1] - c.cy) * c.scale];
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
            let name = &lobby.layers[li].data.slots[b.slot as usize].name;
            if lo[0] < -4000.0 || lo[1] < -4000.0 || hi[0] > 4000.0 || hi[1] > 4000.0 || std::env::var("ALL").is_ok() {
                println!("layer {li} slot {name}: x {:.0}..{:.0} y {:.0}..{:.0} blend {:?} alpha {:.2}", lo[0], hi[0], lo[1], hi[1], b.blend, b.color[3]);
            }
        }
    }
}

//! Triangles in the draw lists at rest (after the intro): `tris PACK...`.
fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (mut lobby, _pages) = bal2d_core::Lobby::load(&bytes).unwrap();
        lobby.start();
        let mut list = bal2d_render::DrawList::default();
        let (mut tris, mut verts, mut n) = (0usize, 0usize, 0usize);
        for f in 0..30 * 30 {
            lobby.update(1.0 / 30.0);
            if f >= 30 * 14 {
                for li in 0..lobby.layers.len() {
                    lobby.draw_list(li, &mut list);
                    tris += list.triangle_count();
                    verts += list.positions.len() / 2;
                }
                n += 1;
            }
        }
        println!("{path}: {} triangles, {} vertices per frame", tris / n, verts / n);
    }
}

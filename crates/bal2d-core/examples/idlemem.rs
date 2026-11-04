//! What a pack's baked idles hold in memory, by part: `idlemem PACK...`.
fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        for (li, l) in lobby.idle.iter().enumerate() {
            let Some(l) = l else { continue };
            let b = &l.bake;
            let tris: usize = b.shapes.iter().map(|s| s.tris.capacity() * 2).sum();
            let mesh: usize = b.meshes.iter().map(|m| m.uvs.capacity() * 4 + m.tris.capacity() * 2).sum();
            println!("{path} layer {li}: {} shapes, shape triangles {} KB, meshes {} KB, stream {} KB, {}", b.shapes.len(), tris / 1024, mesh / 1024, b.stream.capacity() / 1024, l.sizes());
        }
    }
}

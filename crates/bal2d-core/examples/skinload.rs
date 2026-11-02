//! Skinning work per frame: `skinload PACK...` counts the draw list's vertices, how many come
//! from weighted meshes and their bone influences, and the triangles that reach the screen.
use bal2d_spine::AttachmentKind;

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (mut lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        for _ in 0..450 {
            lobby.update(1.0 / 30.0);
            lobby.notices.clear();
        }
        let mut list = bal2d_render::DrawList::default();
        let (mut verts, mut weighted, mut influences, mut tris) = (0usize, 0usize, 0usize, 0usize);
        for li in 0..lobby.layers.len() {
            lobby.draw_list(li, &mut list);
            let d = &lobby.layers[li].data;
            for b in &list.batches {
                verts += b.vertex_count as usize;
                tris += b.index_count as usize / 3;
                if let AttachmentKind::Mesh(m) = &d.attachments[b.attachment as usize].kind {
                    if m.vertices.is_weighted() {
                        weighted += b.vertex_count as usize;
                        influences += m.vertices.vertices.len() / 3;
                    }
                }
            }
        }
        let (mut meshes, mut checked) = (0, 0);
        for l in &lobby.layers {
            for a in &l.data.attachments {
                if let AttachmentKind::Mesh(m) = &a.kind {
                    if m.vertices.is_weighted() {
                        meshes += 1;
                        checked += m.vertices.checked as usize;
                    }
                }
            }
        }
        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
        println!("{name:14} vertices {verts:6}  weighted {weighted:6}  influences {influences:6}  triangles {tris:6}  checked {checked}/{meshes}");
    }
}

//! Clipping per frame: `clipload PACK...` counts clipping attachments applied while building the
//! draw lists over the idle loop, and the batches and triangles they produce.
fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (mut lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        let mut list = bal2d_render::DrawList::default();
        let (mut frames, mut clips, mut tris) = (0, 0, 0);
        for f in 0..1200 {
            lobby.update(1.0 / 30.0);
            lobby.notices.clear();
            if f < 900 {
                continue;
            }
            frames += 1;
            for li in 0..lobby.layers.len() {
                lobby.draw_list(li, &mut list);
                clips += list.clips;
                tris += list.indices.len() / 3;
            }
        }
        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
        println!("{name:14} clips/frame {:.1}  triangles/frame {}", clips as f32 / frames as f32, tris / frames);
    }
}

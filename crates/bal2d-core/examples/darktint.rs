//! Batches drawn with a non-black two-color tint over a run: `darktint PACK...`.
fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (mut lobby, _pages) = bal2d_core::Lobby::load(&bytes).unwrap();
        lobby.start();
        let mut list = bal2d_render::DrawList::default();
        let (mut dark, mut total, mut slots) = (0usize, 0usize, std::collections::BTreeSet::new());
        for _ in 0..30 * 30 {
            lobby.update(1.0 / 30.0);
            for li in 0..lobby.layers.len() {
                lobby.draw_list(li, &mut list);
                for b in &list.batches {
                    total += 1;
                    if let Some(d) = b.dark {
                        if d.iter().any(|c| *c > 0.004) {
                            dark += 1;
                            slots.insert((li, b.slot));
                        }
                    }
                }
            }
        }
        println!("{path}: {dark} of {total} batches with a dark color, slots {slots:?}");
    }
}

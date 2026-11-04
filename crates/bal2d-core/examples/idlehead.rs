//! Sizes of each baked idle's parts: `idlehead PACK...` (section, header before the stream,
//! stream, pieces).
fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        for (li, e) in lobby.idle_entries.iter().enumerate() {
            let (Some(e), Some(l)) = (e, lobby.idle.get(li).and_then(|i| i.as_ref())) else { continue };
            let stream = l.bake.stream_len();
            println!("{path} layer {li}: section {} KB, header {} KB, stream {} KB in {} pieces", e.raw_len / 1024, (e.raw_len as usize - stream) / 1024, stream / 1024, l.bake.stream.len());
        }
    }
}

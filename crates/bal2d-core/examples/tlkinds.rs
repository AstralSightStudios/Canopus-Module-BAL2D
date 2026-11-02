//! Timeline kinds of the idle animations: `tlkinds PACK...`.
use bal2d_spine::Timeline;
use std::collections::BTreeMap;

fn kind(t: &Timeline) -> String {
    format!("{t:?}").split(|c: char| !c.is_alphanumeric()).next().unwrap_or("?").to_string()
}

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
        for (li, l) in lobby.layers.iter().enumerate() {
            for a in &l.data.animations {
                if a.name != "Idle_01" {
                    continue;
                }
                let mut k: BTreeMap<String, usize> = BTreeMap::new();
                for t in &a.timelines {
                    *k.entry(kind(t)).or_default() += 1;
                }
                println!("{name:14} L{li} {} timelines: {k:?}", a.timelines.len());
            }
        }
    }
}

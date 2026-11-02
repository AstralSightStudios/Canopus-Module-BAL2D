//! Curve shapes of the idle animation: `curvestats PACK...` counts, per timeline kind, timelines,
//! constant ones, keys and Bezier segments (what apply evaluates every frame).
use bal2d_spine::Timeline;
use std::collections::BTreeMap;

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
        for (li, l) in lobby.layers.iter().enumerate() {
            for a in l.data.animations.iter().filter(|a| a.name == "Idle_01") {
                let mut k: BTreeMap<String, [usize; 4]> = BTreeMap::new();
                for t in &a.timelines {
                    let kind = format!("{t:?}").split(|c: char| !c.is_alphanumeric()).next().unwrap_or("?").to_string();
                    let c = match t {
                        Timeline::SlotColor { curve, .. } | Timeline::Bone { curve, .. } => Some(curve),
                        _ => None,
                    };
                    let e = k.entry(kind).or_default();
                    e[0] += 1;
                    if let Some(c) = c {
                        e[1] += c.constant as usize;
                        e[2] += c.frame_count();
                        e[3] += (0..c.frame_count()).filter(|&f| c.curves[f] >= 2.0).count();
                    }
                }
                println!("{name} L{li} duration {:.1}s: [timelines, constant, keys, bezier segments] {k:?}", a.duration);
            }
        }
    }
}

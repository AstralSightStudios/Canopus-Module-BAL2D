//! Constant deforms: `deforms2 PACK...` finds, per layer, the (slot, attachment) deform timelines
//! whose keys are all equal, and whether every animation of the layer has one with the same values.
use bal2d_spine::Timeline;
use std::collections::HashMap;

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
        for (li, l) in lobby.layers.iter().enumerate() {
            let d = &l.data;
            // (slot, attachment) -> per animation: Some(constant values) / None (moving)
            let mut seen: HashMap<(u32, u32), Vec<(usize, Option<Vec<f32>>)>> = HashMap::new();
            for (ai, a) in d.animations.iter().enumerate() {
                for t in &a.timelines {
                    if let Timeline::Deform { slot, attachment, vertices, .. } = t {
                        let constant = vertices.iter().all(|k| k == &vertices[0]);
                        seen.entry((*slot as u32, *attachment)).or_default().push((ai, constant.then(|| vertices[0].clone())));
                    }
                }
            }
            let anims = d.animations.len();
            let (mut everywhere_same, mut floats, mut partial) = (0, 0, 0);
            for v in seen.values() {
                let all_const = v.iter().all(|(_, c)| c.is_some());
                let same = all_const && v.iter().all(|(_, c)| c == &v[0].1);
                if same && v.len() == anims {
                    everywhere_same += 1;
                    floats += v[0].1.as_ref().unwrap().len();
                } else if all_const {
                    partial += 1;
                }
            }
            if !seen.is_empty() {
                println!("{name:14} L{li}: {anims} animations, {} deformed attachments; constant & identical in every animation: {everywhere_same} ({floats} floats); constant but not everywhere/same: {partial}", seen.len());
            }
        }
    }
}

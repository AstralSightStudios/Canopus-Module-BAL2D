//! Deform timeline sizes: `deforms PACK...` sums, over every animation's deform timelines, the
//! floats per key (what apply interpolates each frame) and the span that ever changes (the
//! first to last float that differs between any two keys or from the rest value).
use bal2d_spine::Timeline;

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
        for (li, l) in lobby.layers.iter().enumerate() {
            let d = &l.data;
            for a in &d.animations {
                let (mut n, mut floats, mut span, mut moving) = (0, 0usize, 0usize, 0usize);
                for t in &a.timelines {
                    if let Timeline::Deform { vertices, .. } = t {
                        n += 1;
                        let len = vertices[0].len();
                        floats += len;
                        // Rest value: the first key (a span that never changes is constant).
                        let (mut lo, mut hi) = (usize::MAX, 0usize);
                        for k in vertices.iter() {
                            for i in 0..len {
                                if (k[i] - vertices[0][i]).abs() > 1e-6 {
                                    lo = lo.min(i);
                                    hi = hi.max(i + 1);
                                }
                            }
                        }
                        if lo < hi {
                            span += hi - lo;
                            moving += 1;
                        }
                    }
                }
                if n > 0 && (a.name.contains("Idle") || a.name.contains("idle")) {
                    println!("{name:14} L{li} {:24} deform timelines {n:3} ({moving:3} moving)  floats/frame {floats:6}  changing span {span:6}", a.name);
                }
            }
        }
    }
}

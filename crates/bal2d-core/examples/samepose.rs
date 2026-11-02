//! Checks that two packs of the same lobby pose identically: `samepose A B [SECONDS]` plays both
//! (with taps and a pat) and compares every drawn slot's vertices and colour, frame by frame.
//! Slots only one pack draws (culled, baked) are counted, not compared.
use bal2d_core::{Lobby, Touch};
use bal2d_render::DrawList;

fn slots(l: &Lobby, out: &mut Vec<Vec<(u16, Vec<f32>, [f32; 4])>>) {
    out.clear();
    let mut list = DrawList::default();
    for li in 0..l.layers.len() {
        let d = &l.layers[li];
        bal2d_render::build(&d.data, &d.skeleton, &mut list);
        out.push(
            list.batches
                .iter()
                .map(|b| {
                    let v0 = b.first_vertex as usize * 2;
                    (b.slot, list.positions[v0..v0 + b.vertex_count as usize * 2].to_vec(), b.color)
                })
                .collect(),
        );
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (mut x, _) = Lobby::load(&std::fs::read(&a[0]).unwrap()).unwrap();
    let (mut y, _) = Lobby::load(&std::fs::read(&a[1]).unwrap()).unwrap();
    let secs: f32 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(40.0);
    let head = x.grab_box().map(|(bx, by, w, h)| (bx + w / 2.0, by + h / 2.0)).unwrap_or((168.0, 80.0));
    let (mut sa, mut sb) = (Vec::new(), Vec::new());
    let (mut max_v, mut max_c, mut compared, mut only) = (0f32, 0f32, 0usize, 0usize);
    let frames = (secs * 30.0) as usize;
    for f in 0..frames {
        let t = f as f32 / 30.0;
        for l in [&mut x, &mut y] {
            if (t - 12.0).abs() < 0.01 {
                l.touch(Touch::Down, 168.0, 400.0);
            }
            if (t - 12.1).abs() < 0.01 {
                l.touch(Touch::Up, 168.0, 400.0);
            }
            if (t - 25.0).abs() < 0.01 {
                l.touch(Touch::Down, head.0, head.1);
            }
            if t > 25.0 && t < 27.0 {
                l.touch(Touch::Move, head.0 + 30.0 * ((t - 25.0) * 3.0).sin(), head.1);
            }
            if (t - 27.0).abs() < 0.01 {
                l.touch(Touch::Up, head.0, head.1);
            }
            l.update(1.0 / 30.0);
            l.notices.clear();
        }
        slots(&x, &mut sa);
        slots(&y, &mut sb);
        for (la, lb) in sa.iter().zip(&sb) {
            for (s, va, ca) in la {
                match lb.iter().find(|e| e.0 == *s) {
                    Some((_, vb, cb)) if va.len() == vb.len() => {
                        compared += 1;
                        for (p, q) in va.iter().zip(vb) {
                            max_v = max_v.max((p - q).abs());
                        }
                        for k in 0..4 {
                            max_c = max_c.max((ca[k] - cb[k]).abs());
                        }
                    }
                    _ => only += 1,
                }
            }
        }
    }
    println!("{frames} frames: {compared} slot draws compared, max vertex delta {max_v:.5}, max colour delta {max_c:.5}; {only} drawn by only one pack");
}

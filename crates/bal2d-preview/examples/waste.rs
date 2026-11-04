//! Fill the GPU spends on pixels a call leaves unchanged (texel alpha and colour ~0), per slot, in
//! the baked idle: `waste PACK [FRAMES]`. Averages FRAMES idle frames 0.25 s apart.
use std::collections::HashMap;

use bal2d_preview::raster;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).unwrap();
    let frames: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(4);
    let mut pv = bal2d_preview::PackPreview::load(&bytes).unwrap();
    let mut t = 0.0;
    while !pv.lobby.idle_ready() && t < 120.0 {
        pv.advance(0.5);
        t += 0.5;
    }
    let (w, h) = pv.size();
    let screen = (w * h) as f32;
    // slot -> (covered, empty, calls)
    let mut by_slot: HashMap<String, (usize, usize, usize)> = HashMap::new();
    // slot -> pixels hidden by opaque pixels of later calls (any layer)
    let mut hidden: HashMap<String, usize> = HashMap::new();
    let mut hidden_total = 0usize;
    let (mut total, mut empty_total) = (0usize, 0usize);
    for _ in 0..frames {
        // (name, pixels (index, opaque))
        let mut frame_calls: Vec<(String, Vec<(u32, bool)>)> = Vec::new();
        for li in 0..pv.lobby.layers.len() {
            let mut per_call = Vec::new();
            pv.idle_calls(li, |call, tex| {
                let m = call.matrix;
                let det = m[0] * m[4] - m[1] * m[3];
                let (mut n, mut e) = (0usize, 0usize);
                let mut px = Vec::new();
                if det.abs() > 1e-12 {
                    let (pw, ph) = (tex.width as f32, tex.height as f32);
                    let inv = [m[4] / det / pw, -m[1] / det / pw, -m[3] / det / ph, m[0] / det / ph];
                    let add = call.blend == bal2d_spine::BlendMode::Additive;
                    raster::fill_polygons(&raster::polygons(call.path), (w, h), |x, y| {
                        let (dx, dy) = (x as f32 + 0.5 - m[2], y as f32 + 0.5 - m[5]);
                        let s = tex.sample(inv[0] * dx + inv[1] * dy, inv[2] * dx + inv[3] * dy);
                        let c = call.color;
                        let v = if add { (s[0] * c[0]).max(s[1] * c[1]).max(s[2] * c[2]) * c[3] } else { s[3] * c[3] };
                        n += 1;
                        let opaque = !add && call.blend == bal2d_spine::BlendMode::Normal && s[3] * c[3] >= 254.5 / 255.0;
                        px.push(((y * w + x) as u32, opaque));
                        if v < 0.5 / 255.0 {
                            e += 1;
                        }
                    });
                }
                per_call.push((call.tag, n, e, px));
            });
            let cur: Vec<u16> = pv.lobby.idle[li].as_ref().unwrap().current_shapes();
            for (i, (_, n, e, px)) in per_call.into_iter().enumerate() {
                let name = if i < cur.len() {
                    let l = pv.lobby.idle[li].as_ref().unwrap();
                    let slot = l.bake.shapes[cur[i] as usize].slot as usize;
                    format!("L{li} {}", pv.lobby.layers[li].data.slots[slot].name)
                } else {
                    format!("L{li} ?")
                };
                let s = by_slot.entry(name.clone()).or_default();
                s.0 += n;
                s.1 += e;
                s.2 += 1;
                total += n;
                empty_total += e;
                frame_calls.push((name, px));
            }
        }
        let mut covered = vec![false; w * h];
        for (name, px) in frame_calls.iter().rev() {
            let mut hid = 0;
            for &(i, _) in px {
                if covered[i as usize] {
                    hid += 1;
                }
            }
            for &(i, o) in px {
                if o {
                    covered[i as usize] = true;
                }
            }
            *hidden.entry(name.clone()).or_default() += hid;
            hidden_total += hid;
        }
        pv.advance(0.25);
    }
    let f = frames as f32;
    let mut v: Vec<_> = by_slot.into_iter().collect();
    v.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    for (name, (n, e, c)) in v.iter().take(20) {
        println!("  {name:<40} {:.2} screens, {:.2} empty ({:.0}%), {:.1} calls", *n as f32 / screen / f, *e as f32 / screen / f, 100.0 * *e as f32 / (*n).max(1) as f32, *c as f32 / f);
    }
    let mut hv: Vec<_> = hidden.into_iter().collect();
    hv.sort_by(|a, b| b.1.cmp(&a.1));
    println!("hidden under later opaque pixels:");
    for (name, n) in hv.iter().take(12) {
        println!("  {name:<40} {:.2} screens", *n as f32 / screen / f);
    }
    println!("total {:.2} screens/frame, {:.2} of them empty, {:.2} hidden", total as f32 / screen / f, empty_total as f32 / screen / f, hidden_total as f32 / screen / f);
}

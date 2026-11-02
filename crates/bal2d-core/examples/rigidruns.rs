//! Calls a compile-time composite of rigid runs could save: `rigidruns PACK [TOL]` samples the
//! idle loop at 15 fps and finds runs of consecutive batches (same blend) that each move as one
//! affine within TOL and keep their tint, and whose affines stay in a fixed relation to the
//! run's first one (so the run could be drawn from one pre-composited image with one affine).
use std::collections::HashMap;

type M = [f32; 6];

fn ls(texel: &[[f32; 2]], screen: &[[f32; 2]]) -> Option<M> {
    let n = texel.len() as f64;
    let (mu, mv) = texel.iter().fold((0.0, 0.0), |a, t| (a.0 + t[0] as f64 / n, a.1 + t[1] as f64 / n));
    let (mut suu, mut suv, mut svv, mut sux, mut svx, mut suy, mut svy, mut sx, mut sy) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    for (t, s) in texel.iter().zip(screen) {
        let (u, v) = (t[0] as f64 - mu, t[1] as f64 - mv);
        let (x, y) = (s[0] as f64, s[1] as f64);
        suu += u * u;
        suv += u * v;
        svv += v * v;
        sux += u * x;
        svx += v * x;
        suy += u * y;
        svy += v * y;
        sx += x;
        sy += y;
    }
    let det = suu * svv - suv * suv;
    if !(det > 1e-9) {
        return None;
    }
    let a = (sux * svv - svx * suv) / det;
    let b = (svx * suu - sux * suv) / det;
    let d = (suy * svv - svy * suv) / det;
    let e = (svy * suu - suy * suv) / det;
    Some([a as f32, b as f32, (sx / n - a * mu - b * mv) as f32, d as f32, e as f32, (sy / n - d * mu - e * mv) as f32])
}

fn at(m: &M, p: [f32; 2]) -> [f32; 2] {
    [m[0] * p[0] + m[1] * p[1] + m[2], m[3] * p[0] + m[4] * p[1] + m[5]]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tol: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.75);
    let bytes = std::fs::read(&args[0]).unwrap();
    let (mut lobby, pages) = bal2d_core::Lobby::load(&bytes).unwrap();
    let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
    for p in &pages {
        sizes[p.layer as usize].push((p.header.width as f32, p.header.height as f32));
    }
    for _ in 0..1200 {
        lobby.update(1.0 / 30.0);
    }
    let c = lobby.camera;
    let (w2, h2) = (lobby.width * 0.5, lobby.height * 0.5);
    let period = lobby.layers.iter().map(|l| l.state.entry(l.state.current(0).unwrap()).animation_end).fold(0.0, f32::max).min(12.0);
    let frames = (period * 15.0) as usize;
    let mut list = bal2d_render::DrawList::default();
    // Per frame: ordered batch keys with (blend, tint, texels, screen).
    for li in 0..lobby.layers.len() {
        let mut seq: Vec<Vec<((u16, u32), u8, [u32; 4], Vec<[f32; 2]>, Vec<[f32; 2]>)>> = Vec::new();
        for _ in 0..frames {
            lobby.draw_list(li, &mut list);
            let mut f = Vec::new();
            for b in &list.batches {
                let Some(&(pw, ph)) = sizes[li].get(b.page as usize) else { continue };
                let v0 = b.first_vertex as usize;
                let n = b.vertex_count as usize;
                let s: Vec<[f32; 2]> = (0..n).map(|i| [(list.positions[(v0 + i) * 2] - c.cx) * c.scale + w2, h2 - (list.positions[(v0 + i) * 2 + 1] - c.cy) * c.scale]).collect();
                if s.iter().all(|p| p[0] < 0.0 || p[1] < 0.0 || p[0] > 2.0 * w2 || p[1] > 2.0 * h2) {
                    continue;
                }
                let t: Vec<[f32; 2]> = (0..n).map(|i| [list.uvs[(v0 + i) * 2] * pw, list.uvs[(v0 + i) * 2 + 1] * ph]).collect();
                f.push(((b.slot, b.attachment), b.blend as u8, b.color.map(f32::to_bits), t, s));
            }
            seq.push(f);
            lobby.update(1.0 / 15.0);
        }
        // Rigid per frame: one affine within tol; record it.
        let mut rigid: HashMap<(u16, u32), Option<Vec<M>>> = HashMap::new();
        let mut tint: HashMap<(u16, u32), Option<[u32; 4]>> = HashMap::new();
        for f in &seq {
            for (k, _, col, t, s) in f {
                let e = rigid.entry(*k).or_insert(Some(Vec::new()));
                let m = ls(t, s).filter(|m| t.iter().zip(s).all(|(tt, ss)| {
                    let q = at(m, *tt);
                    (q[0] - ss[0]).abs() <= tol && (q[1] - ss[1]).abs() <= tol
                }));
                match (e.as_mut(), m) {
                    (Some(v), Some(m)) => v.push(m),
                    _ => *e = None,
                }
                let te = tint.entry(*k).or_insert(Some(*col));
                if te.is_some_and(|c| c != *col) {
                    *te = None;
                }
            }
        }
        // Runs in the first frame's order: consecutive rigid, constant-tint batches whose affine
        // relative to the run's first stays fixed (checked at the batch's texel box corners).
        let first = &seq[0];
        let (mut runs, mut members) = (0usize, 0usize);
        let mut i = 0;
        while i < first.len() {
            let k0 = first[i].0;
            let ok0 = rigid.get(&k0).is_some_and(|r| r.as_ref().is_some_and(|v| v.len() == frames)) && tint[&k0].is_some();
            if !ok0 {
                i += 1;
                continue;
            }
            let r0 = rigid[&k0].as_ref().unwrap().clone();
            let mut j = i + 1;
            while j < first.len() {
                let k = first[j].0;
                if first[j].1 != first[i].1 || !rigid.get(&k).is_some_and(|r| r.as_ref().is_some_and(|v| v.len() == frames)) || tint[&k].is_none() {
                    break;
                }
                let r = rigid[&k].as_ref().unwrap();
                // B's texels through B's affine vs through a fixed map from frame 0: screen_B(f) =
                // A(f) * A(0)^-1 * B(0).
                let corners = &first[j].3;
                let inv = |m: &M| {
                    let det = m[0] * m[4] - m[1] * m[3];
                    let (a, b, c2, d) = (m[4] / det, -m[1] / det, -m[3] / det, m[0] / det);
                    [a, b, -(a * m[2] + b * m[5]), c2, d, -(c2 * m[2] + d * m[5])]
                };
                let a0i = inv(&r0[0]);
                let fixed = (0..frames).all(|f| {
                    corners.iter().all(|t| {
                        let want = at(&r[f], *t);
                        let got = at(&r0[f], at(&a0i, at(&r[0], *t)));
                        (want[0] - got[0]).abs() <= tol && (want[1] - got[1]).abs() <= tol
                    })
                });
                if !fixed {
                    break;
                }
                j += 1;
            }
            if j - i > 1 {
                runs += 1;
                members += j - i;
            }
            i = j;
        }
        println!("layer {li}: {} batches; {} rigid runs of {} batches could save {} calls/frame", first.len(), runs, members, members - runs);
    }
}

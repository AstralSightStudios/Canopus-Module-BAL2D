//! Size and calls of a baked idle: `idlebake PACK [TOL] [FPS] [SLACK] [MAXLOOP]` samples each
//! layer's idle loop (loops longer than MAXLOOP s are sampled at a third of FPS) and groups
//! every frame the way a baked idle would store it: an attachment's groups are kept while each
//! still fits within TOL; a group that no longer does is regrouped on its own; the attachment is
//! regrouped whole once it has SLACK more groups than fresh grouping gives. Shapes are stored as
//! triangle lists (2 bytes per triangle), calls as shape id + i16 matrix delta-coded against
//! the same shape in the previous frame.
use std::collections::HashMap;

use bal2d_render::Tri;

fn quant(m: &[f32; 6]) -> [i16; 6] {
    let mut q = [0i16; 6];
    for k in 0..6 {
        q[k] = if k == 2 || k == 5 { (m[k] * 16.0).round() as i16 } else { (m[k] * 8192.0).round() as i16 };
    }
    q
}

fn groups_of(tris: &[Tri], members: &[u32], tol: f32) -> Vec<Vec<u32>> {
    let sub: Vec<Tri> = members.iter().map(|&i| tris[i as usize]).collect();
    let (mut order, mut out) = (Vec::new(), Vec::new());
    bal2d_render::cluster(&sub, tol, &mut order, &mut out);
    out.iter().map(|c| order[c.start as usize..(c.start + c.count) as usize].iter().map(|&k| members[k as usize]).collect()).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tol: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.75);
    let fps: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15.0);
    let slack: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(2);
    let maxloop: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(12.0);
    let bytes = std::fs::read(&args[0]).unwrap();
    let (mut lobby, pages) = bal2d_core::Lobby::load(&bytes).unwrap();
    let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
    for p in &pages {
        sizes[p.layer as usize].push((p.header.width as f32, p.header.height as f32));
    }
    for _ in 0..3000 {
        lobby.update(1.0 / 30.0);
        if lobby.bake_active() {
            break;
        }
    }
    let c = lobby.camera;
    let (w2, h2) = (lobby.width * 0.5, lobby.height * 0.5);
    let name = std::path::Path::new(&args[0]).file_stem().unwrap().to_string_lossy().to_string();
    let loops: Vec<f32> = lobby
        .layers
        .iter()
        .map(|l| l.state.current(0).and_then(|e| l.state.entry(e).animation).map_or(0.0, |a| l.data.animations[a].duration))
        .collect();
    let mut list = bal2d_render::DrawList::default();
    let mut total = 0usize;
    for li in 0..lobby.layers.len() {
        let period = loops[li];
        if period <= 0.0 {
            continue;
        }
        let rate = if period > maxloop { fps / 3.0 } else { fps };
        let frames = (period * rate).round().max(1.0) as usize;
        let mut t = 0.0;
        while t < period {
            lobby.update(1.0 / 30.0);
            t += 1.0 / 30.0;
        }
        let dt = period / frames as f32;
        let mut prev: HashMap<(u16, u32), Vec<Vec<u32>>> = HashMap::new();
        let mut shapes: HashMap<((u16, u32), Vec<u32>), u16> = HashMap::new();
        let mut shape_data: Vec<u8> = Vec::new();
        let mut last_m: HashMap<u16, [i16; 6]> = HashMap::new();
        let (mut calls, mut fresh_calls) = (0usize, 0usize);
        let mut data: Vec<u8> = Vec::new();
        for _ in 0..frames {
            lobby.draw_list(li, &mut list);
            let mut frame = Vec::new();
            let mut frame_calls = 0u16;
            let mut this_m: HashMap<u16, [i16; 6]> = HashMap::new();
            for b in &list.batches {
                let Some(&(pw, ph)) = sizes[li].get(b.page as usize) else { continue };
                let v0 = b.first_vertex as usize;
                let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
                let p = |i: usize| [(list.positions[(v0 + i) * 2] - c.cx) * c.scale + w2, h2 - (list.positions[(v0 + i) * 2 + 1] - c.cy) * c.scale];
                let tx = |i: usize| [list.uvs[(v0 + i) * 2] * pw, list.uvs[(v0 + i) * 2 + 1] * ph];
                let tris: Vec<Tri> = idx
                    .chunks_exact(3)
                    .map(|x| {
                        let v = [x[0] as usize, x[1] as usize, x[2] as usize];
                        Tri { s: v.map(p), t: v.map(tx), v: v.map(|k| k as u32) }
                    })
                    .collect();
                if tris.iter().all(|t| t.s.iter().all(|q| q[0] < 0.0 || q[1] < 0.0 || q[0] > w2 * 2.0 || q[1] > h2 * 2.0)) {
                    continue;
                }
                let key = (b.slot, b.attachment);
                let all: Vec<u32> = (0..tris.len() as u32).collect();
                let fresh = groups_of(&tris, &all, tol);
                fresh_calls += fresh.len();
                let groups = match prev.get(&key).filter(|g| g.iter().flatten().all(|&i| (i as usize) < tris.len())) {
                    Some(old) => {
                        let mut out = Vec::new();
                        for g in old {
                            let ok = tris[g[0] as usize].affine().is_some_and(|m| g.iter().all(|&i| tris[i as usize].fits(&m, tol)));
                            if ok {
                                out.push(g.clone());
                            } else {
                                out.extend(groups_of(&tris, g, tol));
                            }
                        }
                        if out.len() > fresh.len() + slack { fresh } else { out }
                    }
                    None => fresh,
                };
                for g in &groups {
                    let mut sorted = g.clone();
                    sorted.sort_unstable();
                    let n = shapes.len() as u16;
                    let id = *shapes.entry((key, sorted.clone())).or_insert_with(|| {
                        shape_data.extend_from_slice(&(sorted.len() as u16).to_le_bytes());
                        let mut last = 0i32;
                        for &i in &sorted {
                            shape_data.extend_from_slice(&((i as i32 - last) as i16).to_le_bytes());
                            last = i as i32;
                        }
                        n
                    });
                    let m = quant(&tris[g[0] as usize].affine().unwrap_or([0.0; 6]));
                    let base = last_m.get(&id).copied().unwrap_or([0; 6]);
                    frame.extend_from_slice(&id.to_le_bytes());
                    for k in 0..6 {
                        frame.extend_from_slice(&m[k].wrapping_sub(base[k]).to_le_bytes());
                    }
                    this_m.insert(id, m);
                    frame_calls += 1;
                }
                prev.insert(key, groups);
            }
            last_m = this_m;
            calls += frame_calls as usize;
            data.extend_from_slice(&frame_calls.to_le_bytes());
            data.extend_from_slice(&frame);
            lobby.update(dt);
        }
        let fz = miniz_oxide::deflate::compress_to_vec(&data, 9).len();
        let sz = miniz_oxide::deflate::compress_to_vec(&shape_data, 9).len();
        total += fz + sz;
        println!(
            "{name:14} layer {li}: {frames} frames ({period:.1} s @ {rate:.0}) | calls/frame {:.0} (fresh {:.0}) | shapes {} ({} KB deflated) | frames {} KB deflated",
            calls as f32 / frames as f32,
            fresh_calls as f32 / frames as f32,
            shapes.len(),
            sz / 1024,
            fz / 1024
        );
    }
    println!("{name:14} total {} KB", total / 1024);
}

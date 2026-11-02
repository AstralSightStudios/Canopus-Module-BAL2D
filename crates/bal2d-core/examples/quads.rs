//! How many GPU calls are plain image rectangles: `quads PACK [TOL]` counts, per frame after the
//! intro, the clusters whose triangles exactly cover one axis-aligned texel rectangle (a region
//! attachment, or a mesh quad) and could be drawn as an image blit instead of a path.
use bal2d_render::{DrawList, Tri};

fn tris(list: &DrawList, b: &bal2d_render::Batch, cam: (f32, f32, f32), size: (f32, f32), page: (f32, f32)) -> Vec<Tri> {
    let (w2, h2) = (size.0 * 0.5, size.1 * 0.5);
    let (cx, cy, s) = cam;
    let v0 = b.first_vertex as usize;
    let sc: Vec<[f32; 2]> = (v0..v0 + b.vertex_count as usize).map(|v| [(list.positions[v * 2] - cx) * s + w2, h2 - (list.positions[v * 2 + 1] - cy) * s]).collect();
    let tx: Vec<[f32; 2]> = (v0..v0 + b.vertex_count as usize).map(|v| [list.uvs[v * 2] * page.0, list.uvs[v * 2 + 1] * page.1]).collect();
    let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
    idx.chunks_exact(3)
        .filter_map(|t| {
            let (a, bb, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
            let xs = [sc[a][0], sc[bb][0], sc[c][0]];
            let ys = [sc[a][1], sc[bb][1], sc[c][1]];
            let vis = !(xs.iter().cloned().fold(f32::MIN, f32::max) < 0.0 || ys.iter().cloned().fold(f32::MIN, f32::max) < 0.0 || xs.iter().cloned().fold(f32::MAX, f32::min) > size.0 || ys.iter().cloned().fold(f32::MAX, f32::min) > size.1);
            vis.then(|| Tri { s: [sc[a], sc[bb], sc[c]], t: [tx[a], tx[bb], tx[c]], v: [a as u32, bb as u32, c as u32] })
        })
        .collect()
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let tol: f32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0.75);
    let bytes = std::fs::read(&path).unwrap();
    let (mut lobby, pages) = bal2d_core::Lobby::load(&bytes).unwrap();
    let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
    for p in &pages {
        sizes[p.layer as usize].push((p.header.width as f32, p.header.height as f32));
    }
    let size = (lobby.width, lobby.height);
    let cam = lobby.camera;
    let cam = (cam.cx, cam.cy, cam.scale);
    let mut list = DrawList::default();
    for _ in 0..450 {
        lobby.update(1.0 / 30.0);
        lobby.notices.clear();
    }
    let (mut calls, mut quads, mut quad_tris, mut tris_total) = (0, 0, 0, 0);
    let mut order = Vec::new();
    let mut out = Vec::new();
    for li in 0..lobby.layers.len() {
        lobby.draw_list(li, &mut list);
        for b in &list.batches {
            let Some(&page) = sizes[li].get(b.page as usize) else { continue };
            let t = tris(&list, b, cam, size, page);
            if t.is_empty() {
                continue;
            }
            bal2d_render::cluster(&t, tol, &mut order, &mut out);
            calls += out.len();
            tris_total += t.len();
            for c in &out {
                let members: Vec<&Tri> = order[c.start as usize..(c.start + c.count) as usize].iter().map(|&i| &t[i as usize]).collect();
                if bal2d_render::texel_rect(&members.iter().map(|t| **t).collect::<Vec<_>>()).is_some() {
                    quads += 1;
                    quad_tris += members.len();
                }
            }
        }
    }
    println!("{path}: {calls} calls, {quads} texel rectangles ({quad_tris} of {tris_total} triangles)");
}

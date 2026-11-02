//! Where the GPU calls come from: `batches PACK [TOL]` counts, per frame after the intro, the
//! batches (attachments drawn), the clusters (calls) and how many batches give a single call, and
//! how many calls runs of consecutive batches with the same page, blend and tint would need if
//! they were clustered together.
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
    let tol: f32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1.25);
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
    let (mut batches, mut calls, mut single, mut merged_calls, mut runs) = (0, 0, 0, 0, 0);
    let mut order = Vec::new();
    let mut out = Vec::new();
    for li in 0..lobby.layers.len() {
        lobby.draw_list(li, &mut list);
        let mut run: Vec<Tri> = Vec::new();
        let mut key = None;
        let flush = |run: &mut Vec<Tri>, merged_calls: &mut usize, order: &mut Vec<u32>, out: &mut Vec<bal2d_render::Cluster>| {
            if !run.is_empty() {
                bal2d_render::cluster(run, tol, order, out);
                *merged_calls += out.len();
                run.clear();
            }
        };
        for b in &list.batches {
            let Some(&page) = sizes[li].get(b.page as usize) else { continue };
            let t = tris(&list, b, cam, size, page);
            if t.is_empty() {
                continue;
            }
            batches += 1;
            bal2d_render::cluster(&t, tol, &mut order, &mut out);
            calls += out.len();
            single += (out.len() == 1) as usize;
            let k = (b.page, b.blend as u8, b.color.map(f32::to_bits));
            if key != Some(k) {
                flush(&mut run, &mut merged_calls, &mut order, &mut out);
                runs += 1;
                key = Some(k);
            }
            run.extend(t);
        }
        flush(&mut run, &mut merged_calls, &mut order, &mut out);
    }
    println!("{path}: {batches} batches -> {calls} calls ({single} batches with one call); {runs} same-key runs -> {merged_calls} calls if merged");
}

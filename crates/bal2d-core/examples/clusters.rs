//! Clustering cost and outcome per strategy: `clusters PACK [TOL]`.
//!
//! For frames after the intro, every batch's visible triangles are grouped by
//! * greedy: the renderer's global greedy pass (every pending triangle tested per cluster);
//! * grow: a cluster grows from its seed across shared edges only, then the greedy test runs
//!   over the triangles left (bounded);
//! and the fit tests (the cost on the watch) and clusters (GPU calls) are counted.
use bal2d_render::{DrawList, Tri};
use std::collections::HashMap;

fn tris_of(list: &DrawList, b: &bal2d_render::Batch, cam: (f32, f32, f32), size: (f32, f32), page: (f32, f32)) -> Vec<Tri> {
    let (w2, h2) = (size.0 * 0.5, size.1 * 0.5);
    let (cx, cy, s) = cam;
    let v0 = b.first_vertex as usize;
    let mut screen = Vec::new();
    let mut texel = Vec::new();
    for v in v0..v0 + b.vertex_count as usize {
        screen.push([(list.positions[v * 2] - cx) * s + w2, h2 - (list.positions[v * 2 + 1] - cy) * s]);
        texel.push([list.uvs[v * 2] * page.0, list.uvs[v * 2 + 1] * page.1]);
    }
    let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
    let mut out = Vec::new();
    for t in idx.chunks_exact(3) {
        let (a, bb, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
        let xs = [screen[a][0], screen[bb][0], screen[c][0]];
        let ys = [screen[a][1], screen[bb][1], screen[c][1]];
        if xs.iter().cloned().fold(f32::MIN, f32::max) < 0.0
            || ys.iter().cloned().fold(f32::MIN, f32::max) < 0.0
            || xs.iter().cloned().fold(f32::MAX, f32::min) > size.0
            || ys.iter().cloned().fold(f32::MAX, f32::min) > size.1
        {
            continue;
        }
        out.push(Tri { s: [screen[a], screen[bb], screen[c]], t: [texel[a], texel[bb], texel[c]], v: [a as u32, bb as u32, c as u32] });
    }
    out
}

fn greedy(tris: &[Tri], tol: f32) -> (usize, usize) {
    let mut pending: Vec<u32> = (0..tris.len() as u32).collect();
    let (mut checks, mut clusters) = (0, 0);
    while let Some(&first) = pending.first() {
        let Some(m) = tris[first as usize].affine() else {
            pending.remove(0);
            continue;
        };
        clusters += 1;
        let mut rest = Vec::new();
        for &i in &pending[1..] {
            checks += 1;
            if !tris[i as usize].fits(&m, tol) {
                rest.push(i);
            }
        }
        pending = rest;
    }
    (checks, clusters)
}

/// Grows each cluster across shared edges from its seed; then, if `sweep`, tests the remaining
/// pending triangles once more like greedy (so the result can only have fewer or equal clusters
/// than growth alone).
fn grow(tris: &[Tri], tol: f32, sweep: bool) -> (usize, usize) {
    let mut by_edge: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for (i, t) in tris.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t.v[k], t.v[(k + 1) % 3]);
            by_edge.entry((a.min(b), a.max(b))).or_default().push(i as u32);
        }
    }
    let mut done = vec![false; tris.len()];
    let (mut checks, mut clusters) = (0, 0);
    let mut tested = vec![usize::MAX; tris.len()];
    for seed in 0..tris.len() {
        if done[seed] {
            continue;
        }
        let Some(m) = tris[seed].affine() else {
            done[seed] = true;
            continue;
        };
        clusters += 1;
        done[seed] = true;
        let mut stack = vec![seed as u32];
        while let Some(t) = stack.pop() {
            let tv = tris[t as usize].v;
            for k in 0..3 {
                let (a, b) = (tv[k], tv[(k + 1) % 3]);
                for &n in &by_edge[&(a.min(b), a.max(b))] {
                    let n = n as usize;
                    if done[n] || tested[n] == clusters {
                        continue;
                    }
                    tested[n] = clusters;
                    checks += 1;
                    if tris[n].fits(&m, tol) {
                        done[n] = true;
                        stack.push(n as u32);
                    }
                }
            }
        }
        if sweep {
            for n in 0..tris.len() {
                if !done[n] && tested[n] != clusters {
                    checks += 1;
                    if tris[n].fits(&m, tol) {
                        done[n] = true;
                    }
                }
            }
        }
    }
    (checks, clusters)
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
    let mut list = DrawList::default();
    for _ in 0..450 {
        lobby.update(1.0 / 30.0);
        lobby.notices.clear();
    }
    let mut tot = [[0usize; 2]; 3];
    let mut tris_total = 0;
    let mut biggest = 0;
    for _ in 0..30 {
        lobby.update(1.0 / 30.0);
        lobby.notices.clear();
        for li in 0..lobby.layers.len() {
            lobby.draw_list(li, &mut list);
            for b in &list.batches {
                let Some(&page) = sizes[li].get(b.page as usize) else { continue };
                let tris = tris_of(&list, b, (cam.cx, cam.cy, cam.scale), size, page);
                tris_total += tris.len();
                biggest = biggest.max(tris.len());
                for (k, (c, n)) in [greedy(&tris, tol), grow(&tris, tol, false), grow(&tris, tol, true)].into_iter().enumerate() {
                    tot[k][0] += c;
                    tot[k][1] += n;
                }
            }
        }
    }
    println!("{path}: tol {tol}; per frame {} visible triangles (largest batch {biggest})", tris_total / 30);
    for (k, name) in ["greedy", "grow", "grow+sweep"].iter().enumerate() {
        println!("  {name:11} {:7} fit tests  {:4} clusters", tot[k][0] / 30, tot[k][1] / 30);
    }
}

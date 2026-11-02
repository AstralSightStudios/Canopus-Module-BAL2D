//! Calls for greedy clustering with and without least-squares refits: `refit PACK... [--tol T]`.
//! A refit fits one affine to all of a cluster's corners and scans again; it is kept when more
//! triangles fit it (within the same tolerance) than fitted the seed triangle's own affine.
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

/// Least-squares texel -> screen affine over the corners of `members`.
fn fit(tris: &[Tri], members: &[usize]) -> Option<[f32; 6]> {
    // Centred for conditioning.
    let n = (members.len() * 3) as f64;
    let (mut mu, mut mv) = (0f64, 0f64);
    for &i in members {
        for k in 0..3 {
            mu += tris[i].t[k][0] as f64;
            mv += tris[i].t[k][1] as f64;
        }
    }
    mu /= n;
    mv /= n;
    let (mut suu, mut suv, mut svv, mut sux, mut svx, mut suy, mut svy, mut sx, mut sy) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    for &i in members {
        for k in 0..3 {
            let u = tris[i].t[k][0] as f64 - mu;
            let v = tris[i].t[k][1] as f64 - mv;
            let (x, y) = (tris[i].s[k][0] as f64, tris[i].s[k][1] as f64);
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
    }
    let det = suu * svv - suv * suv;
    if det.abs() < 1e-9 {
        return None;
    }
    let a = (sux * svv - svx * suv) / det;
    let b = (svx * suu - sux * suv) / det;
    let d = (suy * svv - svy * suv) / det;
    let e = (svy * suu - suy * suv) / det;
    let c = sx / n - a * mu - b * mv;
    let f = sy / n - d * mu - e * mv;
    Some([a as f32, b as f32, c as f32, d as f32, e as f32, f as f32])
}

fn cluster(tris: &[Tri], tol: f32, refits: usize) -> usize {
    let mut pending: Vec<usize> = (0..tris.len()).collect();
    let mut calls = 0;
    while !pending.is_empty() {
        let Some(m0) = tris[pending[0]].affine() else {
            pending.remove(0);
            continue;
        };
        let mut members: Vec<usize> = pending.iter().copied().filter(|&i| tris[i].fits(&m0, tol)).collect();
        if members.is_empty() {
            members.push(pending[0]);
        }
        for _ in 0..refits {
            let Some(m) = fit(tris, &members) else { break };
            let more: Vec<usize> = pending.iter().copied().filter(|&i| tris[i].fits(&m, tol)).collect();
            if more.len() > members.len() {
                members = more;
            } else {
                break;
            }
        }
        calls += 1;
        let taken: std::collections::HashSet<usize> = members.into_iter().collect();
        pending.retain(|i| !taken.contains(i));
    }
    calls
}

/// Greedy, scanning only the first `window` remaining triangles for each cluster; returns
/// (calls, candidate checks).
fn cluster_window(tris: &[Tri], tol: f32, window: usize) -> (usize, usize) {
    let mut pending: Vec<usize> = (0..tris.len()).collect();
    let (mut calls, mut checks) = (0, 0);
    while !pending.is_empty() {
        let Some(m0) = tris[pending[0]].affine() else {
            pending.remove(0);
            continue;
        };
        let mut rest = Vec::new();
        for (j, &i) in pending.iter().enumerate().skip(1) {
            if j <= window {
                checks += 1;
                if tris[i].fits(&m0, tol) {
                    continue;
                }
            }
            rest.push(i);
        }
        calls += 1;
        pending = rest;
    }
    (calls, checks)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tol: f32 = args.iter().position(|a| a == "--tol").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    for path in args.iter().filter(|a| a.ends_with(".bal2d")) {
        let bytes = std::fs::read(path).unwrap();
        let (mut lobby, pages) = bal2d_core::Lobby::load(&bytes).unwrap();
        let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
        for p in &pages {
            sizes[p.layer as usize].push((p.header.width as f32, p.header.height as f32));
        }
        let size = (lobby.width, lobby.height);
        let c = lobby.camera;
        let cam = (c.cx, c.cy, c.scale);
        let mut list = DrawList::default();
        let mut totals = [0usize; 3];
        let windows = [usize::MAX, 512, 256, 128, 64];
        let mut wt = [(0usize, 0usize); 5];
        for frame in 0..600 {
            lobby.update(1.0 / 30.0);
            lobby.notices.clear();
            if frame < 450 || frame % 15 != 0 {
                continue;
            }
            for li in 0..lobby.layers.len() {
                lobby.draw_list(li, &mut list);
                for b in &list.batches {
                    let Some(&page) = sizes[li].get(b.page as usize) else { continue };
                    let t = tris(&list, b, cam, size, page);
                    for (r, tot) in totals.iter_mut().enumerate() {
                        *tot += cluster(&t, tol, r);
                    }
                    for (k, w) in windows.iter().enumerate() {
                        let (c, ch) = cluster_window(&t, tol, *w);
                        wt[k].0 += c;
                        wt[k].1 += ch;
                    }
                }
            }
        }
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
        println!("{name:14} tol {tol}: greedy {}  1 refit {}  2 refits {}", totals[0] / 10, totals[1] / 10, totals[2] / 10);
        let w: Vec<String> = windows.iter().zip(&wt).map(|(w, (c, ch))| format!("w{}: {} calls {}k checks", if *w == usize::MAX { 0 } else { *w }, c / 10, ch / 10000)).collect();
        println!("{:14} {}", "", w.join(" | "));
    }
}

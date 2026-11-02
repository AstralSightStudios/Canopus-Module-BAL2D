//! How far meshes could be simplified for the watch: `decimate PACK [TOL]` samples the idle
//! loop and the first talk, then collapses interior vertices into a neighbour (half-edge
//! collapse) while every original vertex stays within TOL target pixels of where the full mesh
//! puts it, in every sampled frame. Prints visible triangles and vertices before and after.
use std::collections::{BTreeMap, BTreeSet};

struct Mesh {
    texels: Vec<[f32; 2]>,
    tris: Vec<[usize; 3]>,
    frames: Vec<Vec<[f32; 2]>>,
}

fn cross(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn bary(t: [[f32; 2]; 3], p: [f32; 2]) -> Option<[f32; 3]> {
    let d = cross(t[0], t[1], t[2]);
    if d.abs() < 1e-9 {
        return None;
    }
    let a = cross(p, t[1], t[2]) / d;
    let b = cross(t[0], p, t[2]) / d;
    let c = 1.0 - a - b;
    let e = -1e-4;
    (a >= e && b >= e && c >= e).then_some([a, b, c])
}

/// Greedy collapses; returns the remaining triangles.
fn simplify(m: &Mesh, tol: f32) -> Vec<[usize; 3]> {
    let n = m.texels.len();
    let mut tris = m.tris.clone();
    // Boundary vertices (on an edge used once) stay.
    let mut edges: BTreeMap<(usize, usize), u32> = BTreeMap::new();
    for t in &tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let mut fixed = vec![false; n];
    for (&(a, b), &c) in &edges {
        if c == 1 {
            fixed[a] = true;
            fixed[b] = true;
        }
    }
    let mut removed = vec![false; n];
    loop {
        let mut changed = false;
        for u in 0..n {
            if fixed[u] || removed[u] {
                continue;
            }
            let ring: Vec<usize> = (0..tris.len()).filter(|&i| tris[i].contains(&u)).collect();
            if ring.is_empty() {
                continue;
            }
            let nbrs: BTreeSet<usize> = ring.iter().flat_map(|&i| tris[i]).filter(|&w| w != u).collect();
            // Test points: removed vertices and u inside the ring.
            let mut lo = [f32::MAX; 2];
            let mut hi = [f32::MIN; 2];
            for &i in &ring {
                for w in tris[i] {
                    lo = [lo[0].min(m.texels[w][0]), lo[1].min(m.texels[w][1])];
                    hi = [hi[0].max(m.texels[w][0]), hi[1].max(m.texels[w][1])];
                }
            }
            let pts: Vec<usize> = (0..n)
                .filter(|&w| (removed[w] || w == u) && (lo[0]..=hi[0]).contains(&m.texels[w][0]) && (lo[1]..=hi[1]).contains(&m.texels[w][1]))
                .filter(|&w| ring.iter().any(|&i| bary(tris[i].map(|k| m.texels[k]), m.texels[w]).is_some()))
                .collect();
            let mut best: Option<(f32, usize)> = None;
            for &v in &nbrs {
                let new: Vec<[usize; 3]> = ring.iter().filter(|&&i| !tris[i].contains(&v)).map(|&i| tris[i].map(|k| if k == u { v } else { k })).collect();
                // No flips or slivers in texel space.
                let ok = ring.iter().filter(|&&i| !tris[i].contains(&v)).zip(&new).all(|(&i, t)| {
                    let a0 = cross(m.texels[tris[i][0]], m.texels[tris[i][1]], m.texels[tris[i][2]]);
                    let a1 = cross(m.texels[t[0]], m.texels[t[1]], m.texels[t[2]]);
                    a0.signum() == a1.signum() && a1.abs() > 0.5
                });
                if !ok {
                    continue;
                }
                let mut worst = 0f32;
                for &w in &pts {
                    let Some((t, b)) = new.iter().find_map(|t| bary(t.map(|k| m.texels[k]), m.texels[w]).map(|b| (t, b))) else {
                        worst = f32::MAX;
                        break;
                    };
                    for f in &m.frames {
                        let x = b[0] * f[t[0]][0] + b[1] * f[t[1]][0] + b[2] * f[t[2]][0];
                        let y = b[0] * f[t[0]][1] + b[1] * f[t[1]][1] + b[2] * f[t[2]][1];
                        worst = worst.max((x - f[w][0]).abs().max((y - f[w][1]).abs()));
                    }
                    if worst > tol {
                        break;
                    }
                }
                if worst <= tol && best.is_none_or(|b| worst < b.0) {
                    best = Some((worst, v));
                }
            }
            if let Some((_, v)) = best {
                tris = tris.iter().filter(|t| !(t.contains(&u) && t.contains(&v))).map(|t| t.map(|k| if k == u { v } else { k })).collect();
                removed[u] = true;
                changed = true;
            }
        }
        if !changed {
            return tris;
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tol: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let bytes = std::fs::read(&args[0]).unwrap();
    let (mut lobby, pages) = bal2d_core::Lobby::load(&bytes).unwrap();
    let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
    for p in &pages {
        sizes[p.layer as usize].push((p.header.width as f32, p.header.height as f32));
    }
    for _ in 0..900 {
        lobby.update(1.0 / 30.0);
        if lobby.is_idle() {
            break;
        }
    }
    let c = lobby.camera;
    let (w2, h2) = (lobby.width * 0.5, lobby.height * 0.5);
    let mut meshes: BTreeMap<(usize, u16, u32), Mesh> = BTreeMap::new();
    let mut list = bal2d_render::DrawList::default();
    let mut visible_tris = 0usize;
    let mut sampled = 0usize;
    let mut sample = |lobby: &bal2d_core::Lobby, meshes: &mut BTreeMap<(usize, u16, u32), Mesh>, visible: &mut usize| {
        for li in 0..lobby.layers.len() {
            lobby.draw_list(li, &mut list);
            for b in &list.batches {
                let Some(&(pw, ph)) = sizes[li].get(b.page as usize) else { continue };
                let v0 = b.first_vertex as usize;
                let n = b.vertex_count as usize;
                let pos: Vec<[f32; 2]> = (0..n).map(|i| [(list.positions[(v0 + i) * 2] - c.cx) * c.scale + w2, h2 - (list.positions[(v0 + i) * 2 + 1] - c.cy) * c.scale]).collect();
                if pos.iter().all(|p| p[0] < 0.0) || pos.iter().all(|p| p[1] < 0.0) || pos.iter().all(|p| p[0] > lobby.width) || pos.iter().all(|p| p[1] > lobby.height) {
                    continue;
                }
                *visible += b.index_count as usize / 3;
                let m = meshes.entry((li, b.slot, b.attachment)).or_insert_with(|| Mesh {
                    texels: (0..n).map(|i| [list.uvs[(v0 + i) * 2] * pw, list.uvs[(v0 + i) * 2 + 1] * ph]).collect(),
                    tris: list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize].chunks_exact(3).map(|x| [x[0] as usize, x[1] as usize, x[2] as usize]).collect(),
                    frames: Vec::new(),
                });
                if m.texels.len() == n {
                    m.frames.push(pos);
                }
            }
        }
    };
    let main = (lobby.interactions.layer as usize).min(lobby.layers.len() - 1);
    let idle = lobby.layers[main].data.find_animation(&lobby.interactions.idle).map_or(3.0, |i| lobby.layers[main].data.animations[i].duration);
    let mut t = 0.0;
    while t < idle {
        sample(&lobby, &mut meshes, &mut visible_tris);
        sampled += 1;
        lobby.update(1.0 / 15.0);
        t += 1.0 / 15.0;
    }
    let idle_frames = sampled;
    lobby.tap();
    for _ in 0..(15 * 8) {
        sample(&lobby, &mut meshes, &mut visible_tris);
        sampled += 1;
        lobby.update(1.0 / 15.0);
    }
    let (mut before, mut after, mut vb, mut va) = (0usize, 0usize, 0usize, 0usize);
    let t0 = std::time::Instant::now();
    for m in meshes.values() {
        let s = simplify(m, tol);
        // Weighted by how often the mesh is on screen.
        let w = m.frames.len();
        before += m.tris.len() * w;
        after += s.len() * w;
        vb += m.texels.len() * w;
        va += s.iter().flatten().collect::<BTreeSet<_>>().len() * w;
    }
    let name = std::path::Path::new(&args[0]).file_stem().unwrap().to_string_lossy().to_string();
    let f = sampled as f32;
    println!(
        "{name:14} tol {tol}: {idle_frames}+{} frames; triangles/frame {:.0} -> {:.0} ({:.0}%), vertices/frame {:.0} -> {:.0}; {:.1}s",
        sampled - idle_frames,
        before as f32 / f,
        after as f32 / f,
        100.0 * after as f32 / before as f32,
        vb as f32 / f,
        va as f32 / f,
        t0.elapsed().as_secs_f32()
    );
}

//! How many `fits` tests the greedy clustering makes per frame, against assigning each triangle
//! in one pass (most recently joined cluster tried first): `fitcount PACK [TOL]`.
use bal2d_render::Tri;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tol: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1.0);
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
    let mut list = bal2d_render::DrawList::default();
    let (mut greedy, mut onepass, mut tris_n, mut clusters_n, mut clusters_1, mut batches) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut big = Vec::new();
    let frames = 60;
    for _ in 0..frames {
        for li in 0..lobby.layers.len() {
            lobby.draw_list(li, &mut list);
            for b in &list.batches {
                let Some(&(pw, ph)) = sizes[li].get(b.page as usize) else { continue };
                let v0 = b.first_vertex as usize;
                let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
                let p = |i: usize| [(list.positions[(v0 + i) * 2] - c.cx) * c.scale + w2, h2 - (list.positions[(v0 + i) * 2 + 1] - c.cy) * c.scale];
                let t = |i: usize| [list.uvs[(v0 + i) * 2] * pw, list.uvs[(v0 + i) * 2 + 1] * ph];
                let tris: Vec<Tri> = idx
                    .chunks_exact(3)
                    .map(|x| {
                        let v = [x[0] as usize, x[1] as usize, x[2] as usize];
                        Tri { s: v.map(p), t: v.map(t), v: v.map(|k| k as u32) }
                    })
                    .filter(|tr| tr.s.iter().any(|q| q[0] >= 0.0 && q[1] >= 0.0 && q[0] <= lobby.width && q[1] <= lobby.height))
                    .collect();
                if tris.is_empty() {
                    continue;
                }
                batches += 1;
                tris_n += tris.len() as u64;
                // Greedy as `Paths::group`: every round scans what is left.
                let mut pending: Vec<usize> = (0..tris.len()).collect();
                let mut k = 0;
                while !pending.is_empty() {
                    let Some(m) = tris[pending[0]].affine() else {
                        pending.remove(0);
                        continue;
                    };
                    greedy += pending.len() as u64 - 1;
                    pending = pending[1..].iter().copied().filter(|&i| !tris[i].fits(&m, tol)).collect();
                    k += 1;
                }
                clusters_n += k;
                if tris.len() > 100 {
                    big.push((tris.len(), k));
                }
                // One pass, last joined first.
                let mut ms: Vec<[f32; 6]> = Vec::new();
                let mut last = 0usize;
                for tr in &tris {
                    if ms.is_empty() {
                        if let Some(m) = tr.affine() {
                            ms.push(m);
                        }
                        continue;
                    }
                    onepass += 1;
                    if tr.fits(&ms[last], tol) {
                        continue;
                    }
                    let mut found = false;
                    for (j, m) in ms.iter().enumerate() {
                        if j == last {
                            continue;
                        }
                        onepass += 1;
                        if tr.fits(m, tol) {
                            last = j;
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        if let Some(m) = tr.affine() {
                            ms.push(m);
                            last = ms.len() - 1;
                        }
                    }
                }
                clusters_1 += ms.len() as u64;
            }
        }
        lobby.update(1.0 / 30.0);
    }
    let f = frames as u64;
    big.sort();
    let name = std::path::Path::new(&args[0]).file_stem().unwrap().to_string_lossy().to_string();
    println!(
        "{name:14} batches {} tris {} clusters {} (one pass {}) | fits tests/frame: greedy {} one-pass {} | biggest batches (tris, clusters) {:?}",
        batches / f,
        tris_n / f,
        clusters_n / f,
        clusters_1 / f,
        greedy / f,
        onepass / f,
        &big[big.len().saturating_sub(4)..]
    );
}

//! Can the idle loop be drawn from fixed groups? `staticclusters PACK [TOL_STATIC] [TOL_MERGE]
//! [FPS]` samples one idle loop and splits each attachment's triangles into groups that one
//! affine (the seed triangle's) places within TOL_STATIC in every sampled frame. Per frame,
//! groups are then merged into calls: within an attachment greedily, across consecutive
//! attachments with the same page/blend/tint when the call's affine places the group's texel
//! box corners within TOL_MERGE of where its own affine does. Prints calls per frame against
//! the dynamic clustering, and how many vertices would need skinning (seed vertices only).
use std::collections::BTreeMap;

type M = [f32; 6];

fn affine(t: [[f32; 2]; 3], s: [[f32; 2]; 3]) -> Option<M> {
    bal2d_render::Tri { s, t, v: [0, 1, 2] }.affine()
}

fn at(m: &M, p: [f32; 2]) -> [f32; 2] {
    [m[0] * p[0] + m[1] * p[1] + m[2], m[3] * p[0] + m[4] * p[1] + m[5]]
}

struct Group {
    seed: [usize; 3],
    tris: Vec<[usize; 3]>,
    corners: [[f32; 2]; 4],
}

struct Track {
    texels: Vec<[f32; 2]>,
    indices: Vec<u16>,
    frames: Vec<Option<Vec<[f32; 2]>>>,
    groups: Vec<Group>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tol_s: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let tol_m: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let fps: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(15.0);
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
    lobby.update(0.5);
    let main = (lobby.interactions.layer as usize).min(lobby.layers.len() - 1);
    let idle = {
        let m = &lobby.layers[main];
        m.data.find_animation(&lobby.interactions.idle).map_or(3.0, |i| m.data.animations[i].duration)
    };
    let frames = (idle * fps).ceil() as usize;
    let c = lobby.camera;
    let (w2, h2) = (lobby.width * 0.5, lobby.height * 0.5);
    let mut tracks: BTreeMap<(usize, u16, u32), Track> = BTreeMap::new();
    // Per frame: the batches in draw order (track, page, blend, tint bits).
    let mut seqs: Vec<Vec<((usize, u16, u32), u32, u8, [u32; 4])>> = Vec::new();
    let mut list = bal2d_render::DrawList::default();
    let mut paths = bal2d_render::Paths::new();
    paths.tolerance = tol_s + tol_m;
    let mut dynamic_calls = 0usize;
    let mut vertices = 0usize;
    for f in 0..frames {
        let mut seq = Vec::new();
        for li in 0..lobby.layers.len() {
            lobby.draw_list(li, &mut list);
            paths.draw(li, &list, (c.cx, c.cy, c.scale), (lobby.width, lobby.height), |p| sizes[li].get(p as usize).copied(), |_| dynamic_calls += 1);
            for b in &list.batches {
                let Some(&(pw, ph)) = sizes[li].get(b.page as usize) else { continue };
                let v0 = b.first_vertex as usize;
                let n = b.vertex_count as usize;
                vertices += n;
                let pos: Vec<[f32; 2]> = (0..n).map(|i| [(list.positions[(v0 + i) * 2] - c.cx) * c.scale + w2, h2 - (list.positions[(v0 + i) * 2 + 1] - c.cy) * c.scale]).collect();
                // Off screen altogether: skipped as the renderer does.
                if pos.iter().all(|p| p[0] < 0.0) || pos.iter().all(|p| p[1] < 0.0) || pos.iter().all(|p| p[0] > lobby.width) || pos.iter().all(|p| p[1] > lobby.height) {
                    continue;
                }
                let key = (li, b.slot, b.attachment);
                let t = tracks.entry(key).or_insert_with(|| Track {
                    texels: (0..n).map(|i| [list.uvs[(v0 + i) * 2] * pw, list.uvs[(v0 + i) * 2 + 1] * ph]).collect(),
                    indices: list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize].to_vec(),
                    frames: vec![None; frames],
                    groups: Vec::new(),
                });
                if t.texels.len() == n {
                    t.frames[f] = Some(pos);
                    seq.push((key, b.page, b.blend as u8, b.color.map(f32::to_bits)));
                }
            }
        }
        seqs.push(seq);
        lobby.update(1.0 / fps);
    }
    // Static groups.
    let mut anchors = 0usize;
    for t in tracks.values_mut() {
        let present: Vec<&Vec<[f32; 2]>> = t.frames.iter().flatten().collect();
        let tris: Vec<[usize; 3]> = t.indices.chunks_exact(3).map(|x| [x[0] as usize, x[1] as usize, x[2] as usize]).collect();
        let mut pending: Vec<usize> = (0..tris.len()).collect();
        let mut seeds = std::collections::BTreeSet::new();
        while !pending.is_empty() {
            let seed = tris[pending[0]];
            let tex = seed.map(|v| t.texels[v]);
            let ms: Vec<Option<M>> = present.iter().map(|p| affine(tex, seed.map(|v| p[v]))).collect();
            let fits = |tri: [usize; 3]| {
                present.iter().zip(&ms).all(|(p, m)| match m {
                    None => false,
                    Some(m) => tri.iter().all(|&v| {
                        let q = at(m, t.texels[v]);
                        (q[0] - p[v][0]).abs() <= tol_s && (q[1] - p[v][1]).abs() <= tol_s
                    }),
                })
            };
            let mut members = vec![pending[0]];
            let mut rest = Vec::new();
            for &i in &pending[1..] {
                if fits(tris[i]) {
                    members.push(i);
                } else {
                    rest.push(i);
                }
            }
            let mut lo = [f32::MAX; 2];
            let mut hi = [f32::MIN; 2];
            for &i in &members {
                for v in tris[i] {
                    lo = [lo[0].min(t.texels[v][0]), lo[1].min(t.texels[v][1])];
                    hi = [hi[0].max(t.texels[v][0]), hi[1].max(t.texels[v][1])];
                }
            }
            seeds.extend(seed);
            t.groups.push(Group { seed, tris: members.iter().map(|&i| tris[i]).collect(), corners: [lo, [hi[0], lo[1]], hi, [lo[0], hi[1]]] });
            pending = rest;
        }
        anchors += seeds.len();
    }
    // Per frame merging.
    let close = |a: &M, b: &M, corners: &[[f32; 2]; 4]| {
        corners.iter().all(|&p| {
            let (x, y) = (at(a, p), at(b, p));
            (x[0] - y[0]).abs() <= tol_m && (x[1] - y[1]).abs() <= tol_m
        })
    };
    let (mut unmerged, mut within, mut across) = (0usize, 0usize, 0usize);
    let mut worst = 0f32;
    for (f, seq) in seqs.iter().enumerate() {
        // The open call: (page, blend, tint, affine).
        let mut open: Option<(u32, u8, [u32; 4], M)> = None;
        for (key, page, blend, tint) in seq {
            let t = &tracks[key];
            let p = t.frames[f].as_ref().unwrap();
            let gm: Vec<Option<M>> = t.groups.iter().map(|g| affine(g.seed.map(|v| t.texels[v]), g.seed.map(|v| p[v]))).collect();
            unmerged += t.groups.len();
            // Within the attachment: greedy, the first unplaced group's affine as the call's.
            let mut left: Vec<usize> = (0..t.groups.len()).filter(|&i| gm[i].is_some()).collect();
            let mut calls: Vec<(M, Vec<usize>)> = Vec::new();
            // The previous attachment's last call may take this attachment's groups first.
            if let Some((op, ob, ot, om)) = open.filter(|o| o.0 == *page && o.1 == *blend && o.2 == *tint) {
                let _ = (op, ob, ot);
                let (take, keep): (Vec<usize>, Vec<usize>) = left.iter().partition(|&&i| close(&om, gm[i].as_ref().unwrap(), &t.groups[i].corners));
                if !take.is_empty() {
                    calls.push((om, take));
                    across += 0; // continues the open call: no new call
                }
                left = keep;
            }
            let continued = !calls.is_empty();
            while !left.is_empty() {
                let m = gm[left[0]].unwrap();
                let (take, keep): (Vec<usize>, Vec<usize>) = left.iter().partition(|&&i| close(&m, gm[i].as_ref().unwrap(), &t.groups[i].corners));
                calls.push((m, take));
                left = keep;
            }
            within += calls.len() - continued as usize;
            // Exact error of the calls (all vertices of their groups).
            for (m, gs) in &calls {
                for &g in gs {
                    for tri in &t.groups[g].tris {
                        for &v in tri {
                            let q = at(m, t.texels[v]);
                            worst = worst.max((q[0] - p[v][0]).abs().max((q[1] - p[v][1]).abs()));
                        }
                    }
                }
            }
            open = calls.last().map(|(m, _)| (*page, *blend, *tint, *m));
        }
    }
    let _ = across;
    let name = std::path::Path::new(&args[0]).file_stem().unwrap().to_string_lossy().to_string();
    let n = frames as f32;
    println!(
        "{name:14} {frames} frames | dynamic {:.0} calls (tol {:.1}) | static groups {:.0}/frame, merged {:.0}/frame, worst error {worst:.2} px | vertices {:.0}/frame, anchors {anchors}",
        dynamic_calls as f32 / n,
        tol_s + tol_m,
        unmerged as f32 / n,
        within as f32 / n,
        vertices as f32 / n
    );
}

//! Mesh simplification for the watch's screen: lobby meshes are built for a phone or a monitor,
//! and drawn on a 336 x 480 band many of their triangles cover a few pixels. Every triangle costs
//! the watch skinning, grouping, path data and driver copying each frame, so interior vertices
//! whose removal the screen would not show are collapsed into a neighbour (half-edge collapse).
//!
//! "Would not show" is measured on the simulation: the world positions of every mesh vertex in
//! the frames the lobby plays are recorded ([`Samples`]), and a collapse is kept only if, in
//! every recorded frame, every point of the original mesh (its vertices, edge midpoints and
//! triangle centroids) stays within the tolerance of where the simplified mesh puts the same
//! texel. The outline (boundary vertices) is never touched, so the texture's footprint is the
//! same; collapses that would fold a triangle over in texture space are refused.

use bal2d_spine::*;

/// Most frames kept per attachment: past this, every other one is dropped and only every
/// second offered frame is taken from then on.
const CAP: usize = 480;

/// World positions of mesh vertices over the simulation, per attachment.
pub struct Samples {
    /// Per attachment: vertices per frame (0: nothing recorded, `usize::MAX`: unusable).
    verts: Vec<usize>,
    /// Per attachment: frames of (x, y) per vertex, concatenated.
    frames: Vec<Vec<f32>>,
    stride: Vec<u32>,
    tick: Vec<u32>,
    /// Smallest move (world units) that makes a frame worth keeping.
    eps: f32,
}

impl Samples {
    pub fn new(attachments: usize, eps: f32) -> Samples {
        Samples { verts: vec![0; attachments], frames: vec![Vec::new(); attachments], stride: vec![1; attachments], tick: vec![0; attachments], eps }
    }

    /// Attachment `id` drawn with world positions `pos` (x, y per vertex, the mesh's own).
    pub fn offer(&mut self, id: usize, pos: &[f32]) {
        let n = pos.len() / 2;
        match self.verts[id] {
            usize::MAX => return,
            0 => self.verts[id] = n,
            v if v != n => {
                self.verts[id] = usize::MAX;
                self.frames[id] = Vec::new();
                return;
            }
            _ => {}
        }
        self.tick[id] += 1;
        if self.tick[id] % self.stride[id] != 0 {
            return;
        }
        let f = &mut self.frames[id];
        if f.len() >= pos.len() {
            let last = &f[f.len() - pos.len()..];
            if last.iter().zip(pos).all(|(a, b)| (a - b).abs() < self.eps) {
                return;
            }
        }
        f.extend_from_slice(pos);
        if f.len() / pos.len() > CAP {
            let kept: Vec<f32> = f.chunks_exact(pos.len()).step_by(2).flatten().copied().collect();
            *f = kept;
            self.stride[id] *= 2;
        }
    }

    /// The frames of `id` after its vertices were renumbered (`map`: old -> new).
    pub fn remap(&mut self, id: usize, map: &[Option<u16>]) {
        let n = self.verts[id];
        if n == 0 || n == usize::MAX || map.len() != n {
            return;
        }
        let m = map.iter().filter(|x| x.is_some()).count();
        let mut out = vec![0f32; self.frames[id].len() / n * m];
        for (src, dst) in self.frames[id].chunks_exact(n * 2).zip(out.chunks_exact_mut(m * 2)) {
            for (old, new) in map.iter().enumerate() {
                if let Some(new) = new {
                    dst[*new as usize * 2] = src[old * 2];
                    dst[*new as usize * 2 + 1] = src[old * 2 + 1];
                }
            }
        }
        self.frames[id] = out;
        self.verts[id] = m;
    }

}

/// Simplifies every eligible mesh of `d` within `tol` world units; `skip` marks attachments to
/// leave alone (shared geometry, clipped batches). Returns (triangles, vertices) removed.
pub fn simplify_meshes(d: &mut SkeletonData, samples: &Samples, tol: f32, skip: &[bool]) -> (usize, usize) {
    let (mut tris, mut verts) = (0, 0);
    for id in 0..d.attachments.len() {
        if skip.get(id).copied().unwrap_or(true) {
            continue;
        }
        let AttachmentKind::Mesh(m) = &d.attachments[id].kind else { continue };
        let n = m.vertices.vertex_count();
        if samples.verts[id] != n || samples.frames[id].is_empty() || m.region_uvs.len() != n * 2 {
            continue;
        }
        let texels: Vec<[f32; 2]> = m.region_uvs.chunks_exact(2).map(|p| [p[0], p[1]]).collect();
        let old: Vec<[u16; 3]> = m.triangles.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
        let Some(new) = plan(&texels, &old, &samples.frames[id], tol) else { continue };
        let flat: Vec<u16> = new.iter().flatten().copied().collect();
        if let Some((map, kept)) = crate::trim::rebuild(d, id as u32, &flat) {
            tris += old.len() - kept;
            verts += map.iter().filter(|m| m.is_none()).count();
        }
    }
    (tris, verts)
}

fn area(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Barycentric weights of `p` in `t`, when it lies inside (with a little slack).
fn bary(t: [[f32; 2]; 3], p: [f32; 2]) -> Option<[f32; 3]> {
    let d = area(t[0], t[1], t[2]);
    if d == 0.0 {
        return None;
    }
    let a = area(p, t[1], t[2]) / d;
    let b = area(t[0], p, t[2]) / d;
    let c = 1.0 - a - b;
    const E: f32 = -1e-4;
    (a >= E && b >= E && c >= E).then_some([a, b, c])
}

/// A point of the original mesh: a weighted sum of up to three of its vertices.
struct Point {
    tex: [f32; 2],
    of: [u16; 3],
    w: [f32; 3],
}

/// The simplified triangle list (original vertex numbers), or `None` when nothing could go.
fn plan(texels: &[[f32; 2]], tris: &[[u16; 3]], frames: &[f32], tol: f32) -> Option<Vec<[u16; 3]>> {
    let n = texels.len();
    let nf = frames.len() / (n * 2);
    if nf == 0 || tris.len() < 4 {
        return None;
    }
    let pos = |f: usize, v: usize| -> [f32; 2] { [frames[(f * n + v) * 2], frames[(f * n + v) * 2 + 1]] };
    // Boundary (and non-manifold) vertices stay.
    let mut edges: Vec<(u16, u16)> = tris.iter().flat_map(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])]).map(|(a, b)| (a.min(b), a.max(b))).collect();
    edges.sort_unstable();
    let mut fixed = vec![false; n];
    let mut i = 0;
    let mut unique = Vec::new();
    while i < edges.len() {
        let mut j = i;
        while j < edges.len() && edges[j] == edges[i] {
            j += 1;
        }
        if j - i != 2 {
            fixed[edges[i].0 as usize] = true;
            fixed[edges[i].1 as usize] = true;
        }
        unique.push(edges[i]);
        i = j;
    }
    // Test points: vertices, edge midpoints, centroids.
    let mut points: Vec<Point> = (0..n).map(|v| Point { tex: texels[v], of: [v as u16; 3], w: [1.0, 0.0, 0.0] }).collect();
    for &(a, b) in &unique {
        let (ta, tb) = (texels[a as usize], texels[b as usize]);
        points.push(Point { tex: [(ta[0] + tb[0]) * 0.5, (ta[1] + tb[1]) * 0.5], of: [a, b, b], w: [0.5, 0.5, 0.0] });
    }
    for t in tris {
        let p = t.map(|v| texels[v as usize]);
        points.push(Point { tex: [(p[0][0] + p[1][0] + p[2][0]) / 3.0, (p[0][1] + p[1][1] + p[2][1]) / 3.0], of: *t, w: [1.0 / 3.0; 3] });
    }
    let mut cur: Vec<[u16; 3]> = tris.to_vec();
    let mut alive = vec![true; cur.len()];
    // Triangles round each vertex (indices into `cur`; dead ones filtered when read).
    let mut around: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (i, t) in cur.iter().enumerate() {
        for &v in t {
            around[v as usize].push(i as u32);
        }
    }
    let mut removed = 0;
    loop {
        let mut changed = false;
        for u in 0..n {
            if fixed[u] {
                continue;
            }
            let ring: Vec<u32> = around[u].iter().copied().filter(|&i| alive[i as usize] && cur[i as usize].contains(&(u as u16))).collect();
            if ring.len() < 3 {
                continue;
            }
            // One orientation all round, or the ring is left alone.
            let sign = area(texels[cur[ring[0] as usize][0] as usize], texels[cur[ring[0] as usize][1] as usize], texels[cur[ring[0] as usize][2] as usize]).signum();
            if sign == 0.0 || ring.iter().any(|&i| {
                let t = cur[i as usize];
                area(texels[t[0] as usize], texels[t[1] as usize], texels[t[2] as usize]).signum() != sign
            }) {
                continue;
            }
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            let mut nbrs: Vec<u16> = Vec::new();
            for &i in &ring {
                for v in cur[i as usize] {
                    let t = texels[v as usize];
                    lo = [lo[0].min(t[0]), lo[1].min(t[1])];
                    hi = [hi[0].max(t[0]), hi[1].max(t[1])];
                    if v as usize != u && !nbrs.contains(&v) {
                        nbrs.push(v);
                    }
                }
            }
            // The points the ring covers.
            let inside: Vec<usize> = (0..points.len())
                .filter(|&p| {
                    let t = points[p].tex;
                    t[0] >= lo[0] && t[0] <= hi[0] && t[1] >= lo[1] && t[1] <= hi[1]
                })
                .filter(|&p| ring.iter().any(|&i| bary(cur[i as usize].map(|v| texels[v as usize]), points[p].tex).is_some()))
                .collect();
            let mut best: Option<(f32, u16)> = None;
            'cand: for &v in &nbrs {
                let new: Vec<[u16; 3]> = ring
                    .iter()
                    .map(|&i| cur[i as usize])
                    .filter(|t| !t.contains(&v))
                    .map(|t| t.map(|k| if k as usize == u { v } else { k }))
                    .collect();
                for t in &new {
                    let a = area(texels[t[0] as usize], texels[t[1] as usize], texels[t[2] as usize]);
                    if a.signum() != sign || a.abs() < 1e-9 {
                        continue 'cand;
                    }
                }
                let limit = best.map_or(tol, |b| b.0.min(tol));
                let mut worst = 0f32;
                for &p in &inside {
                    let pt = &points[p];
                    let Some((t, b)) = new.iter().find_map(|t| bary(t.map(|k| texels[k as usize]), pt.tex).map(|b| (*t, b))) else {
                        continue 'cand;
                    };
                    for f in 0..nf {
                        let (a0, a1, a2) = (pos(f, t[0] as usize), pos(f, t[1] as usize), pos(f, t[2] as usize));
                        let (o0, o1, o2) = (pos(f, pt.of[0] as usize), pos(f, pt.of[1] as usize), pos(f, pt.of[2] as usize));
                        let x = b[0] * a0[0] + b[1] * a1[0] + b[2] * a2[0] - (pt.w[0] * o0[0] + pt.w[1] * o1[0] + pt.w[2] * o2[0]);
                        let y = b[0] * a0[1] + b[1] * a1[1] + b[2] * a2[1] - (pt.w[0] * o0[1] + pt.w[1] * o1[1] + pt.w[2] * o2[1]);
                        worst = worst.max(x.abs()).max(y.abs());
                        if worst > limit {
                            continue 'cand;
                        }
                    }
                }
                best = Some((worst, v));
            }
            let Some((_, v)) = best else { continue };
            for &i in &ring {
                let t = &mut cur[i as usize];
                if t.contains(&v) {
                    alive[i as usize] = false;
                } else {
                    *t = t.map(|k| if k as usize == u { v } else { k });
                    around[v as usize].push(i);
                }
            }
            around[u].clear();
            fixed[u] = true;
            removed += 1;
            changed = true;
        }
        if !changed {
            break;
        }
    }
    (removed > 0).then(|| cur.iter().zip(&alive).filter(|(_, a)| **a).map(|(t, _)| *t).collect())
}

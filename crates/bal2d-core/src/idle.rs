//! Baked idle playback. The compiler plays each layer's idle loop and stores it as GPU calls
//! (`bal2d_pack::IdleBake`): per frame, which triangles of which attachment go together and the
//! affine that draws them, the frames ten or so a second. While nothing but the idle loop
//! plays, the lobby draws those calls (matrices interpolated between frames) instead of
//! animating, skinning and grouping: the watch's CPU then only writes paths and calls the
//! driver.

use alloc::vec::Vec;

use bal2d_pack::{IdleBake, IdleCall, IdleCursor, IdleShape};
use bal2d_render::{path_coord, Call, DrawList, Paths, PATH_CLOSE, PATH_LINE, PATH_MOVE};
use bal2d_spine::*;

/// One layer's baked idle with its decoding state.
pub struct IdleLayer {
    pub bake: IdleBake,
    cursor: IdleCursor,
    /// Frame `frame` decoded, and the frame after it (`next`, empty after the last).
    frame: Option<u16>,
    cur: Vec<IdleCall>,
    next: Vec<IdleCall>,
    /// Per shape: its call in `next` (`u16::MAX`: none).
    next_of: Vec<u16>,
    /// Per shape: the page and blend it draws with, and its attachment's triangles are
    /// `shapes[s].tris` of the attachment (checked at load).
    geo: Vec<(u32, BlendMode)>,
    /// Per shape: the vertices it is drawn with (`verts[vert_at[s]..vert_at[s + 1]]`): those of
    /// its outline, or of all its triangles when it has none.
    verts: Vec<u16>,
    vert_at: Vec<u32>,
    /// Per shape: the outline of its triangles (`outline[outline_at[s]..outline_at[s + 1]]`,
    /// closed loops of vertices, `u16::MAX` after each), empty when it is drawn as triangles. One
    /// polygon is far less path data than its triangles (the driver copies every byte into an
    /// uncached command buffer), fewer edges for the GPU, and no seams inside it.
    outline: Vec<u16>,
    outline_at: Vec<u32>,
    path: Vec<i16>,
    /// Per vertex of the attachment being drawn: summed positions (path units) and count, then
    /// the averaged position.
    acc: Vec<[f32; 3]>,
    fixed: Vec<[i16; 2]>,
    mats: Vec<[f32; 6]>,
    /// Per call of the run: its bounds (path units, a margin wider than its vertices); the
    /// vertices the run touched.
    boxes: Vec<[i16; 4]>,
    touched: Vec<u16>,
    /// Per live run: the slots left out of its draw list; and the list and grouping it uses.
    live_skip: Vec<Vec<bool>>,
    live_list: DrawList,
    live_paths: Paths,
}

impl IdleLayer {
    /// The bake for a layer of `data`, when every shape names a drawable attachment of a slot
    /// and triangles it has.
    pub fn new(bake: IdleBake, data: &SkeletonData) -> Option<IdleLayer> {
        let n = bake.shapes.len();
        let total: usize = bake.shapes.iter().map(|s| s.tris.len() * 3).sum();
        // Fallible: the watch loads this last, into whatever memory is left.
        let (mut geo, mut all, mut vert_at, mut outline, mut outline_at) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        geo.try_reserve_exact(n).ok()?;
        all.try_reserve_exact(total).ok()?;
        vert_at.try_reserve_exact(n + 1).ok()?;
        outline_at.try_reserve_exact(n + 1).ok()?;
        let mut used: Vec<u16> = Vec::new();
        let mut scratch = OutlineScratch::default();
        vert_at.push(0u32);
        outline_at.push(0u32);
        for s in &bake.shapes {
            let slot = data.slots.get(s.slot as usize)?;
            let (page, tris, verts) = geometry(data, s.attachment)?;
            if s.tris.iter().any(|&t| (t as usize + 1) * 3 > tris.len()) || tris.iter().any(|&v| v as usize >= verts) {
                return None;
            }
            geo.push((page, slot.blend));
            used.clear();
            let start = outline.len();
            if outline_of(tris, &s.tris, &mut scratch, &mut outline) {
                used.extend(outline[start..].iter().copied().filter(|&v| v != u16::MAX));
            } else {
                outline.truncate(start);
                for &t in &s.tris {
                    used.extend_from_slice(&tris[t as usize * 3..t as usize * 3 + 3]);
                }
            }
            outline_at.push(outline.len() as u32);
            used.sort_unstable();
            used.dedup();
            all.extend_from_slice(&used);
            vert_at.push(all.len() as u32);
        }
        all.shrink_to_fit();
        outline.shrink_to_fit();
        let live_skip: Vec<Vec<bool>> = bake
            .live
            .iter()
            .map(|run| {
                let mut skip = alloc::vec![true; data.slots.len()];
                for &sl in run {
                    if let Some(x) = skip.get_mut(sl as usize) {
                        *x = false;
                    }
                }
                skip
            })
            .collect();
        Some(IdleLayer {
            cursor: IdleCursor::new(n),
            frame: None,
            cur: Vec::new(),
            next: Vec::new(),
            next_of: alloc::vec![u16::MAX; n],
            geo,
            verts: all,
            vert_at,
            outline,
            outline_at,
            path: Vec::new(),
            acc: Vec::new(),
            fixed: Vec::new(),
            mats: Vec::new(),
            boxes: Vec::new(),
            touched: Vec::new(),
            live_skip,
            live_list: DrawList::default(),
            live_paths: {
                let mut p = Paths::new();
                p.tolerance = 1.0;
                p
            },
            bake,
        })
    }

    /// Frame `f` in `cur` and `f + 1` in `next`. Frames are deltas of the ones before: moving
    /// forward decodes the frames in between, moving back starts over.
    fn seek(&mut self, f: u16) -> bool {
        let last = self.bake.frames - 1;
        if self.frame == Some(f) {
            return true;
        }
        match self.frame {
            Some(g) if f == g + 1 && self.cursor.frame == f + 1 => core::mem::swap(&mut self.cur, &mut self.next),
            Some(g) if f > g && self.cursor.frame <= f => {
                if !self.decode_to(f) {
                    return false;
                }
            }
            _ => {
                self.cursor.rewind();
                if !self.decode_to(f) {
                    return false;
                }
            }
        }
        self.next.clear();
        if f < last && self.cursor.next(&self.bake, &mut self.next).is_none() {
            self.frame = None;
            return false;
        }
        self.next_of.iter_mut().for_each(|x| *x = u16::MAX);
        for (i, c) in self.next.iter().enumerate() {
            if let Some(x) = self.next_of.get_mut(c.shape as usize) {
                *x = i as u16;
            }
        }
        self.frame = Some(f);
        true
    }

    /// The shapes of the frame drawn last, in call order (for tools).
    pub fn current_shapes(&self) -> Vec<u16> {
        self.cur.iter().map(|c| c.shape).collect()
    }

    /// Decodes on into `cur` until it holds frame `f`.
    fn decode_to(&mut self, f: u16) -> bool {
        loop {
            match self.cursor.next(&self.bake, &mut self.cur) {
                Some(g) if g == f => return true,
                Some(g) if g < f => {}
                _ => {
                    self.frame = None;
                    return false;
                }
            }
        }
    }

    /// Emits the calls at loop time `tau` (seconds from the bake's first frame), for a target
    /// of `size` whose top-left is `origin` on the pack's screen. Returns the triangles drawn.
    ///
    /// A call's affine places its own triangles within the bake's tolerance, but a vertex two
    /// calls share would land in two places (a hairline gap or overlap along the seam): every
    /// vertex is put where the calls using it place it on average, so neighbouring paths meet
    /// exactly. The texture keeps each call's own affine, as on the live path.
    /// Whether the layer's skeleton must still be posed every frame (its live runs need it).
    pub fn needs_pose(&self) -> bool {
        !self.bake.live.is_empty()
    }

    /// `sk` (posed) and `cam` (skeleton -> target, as `Paths::draw` takes it) are for the live
    /// runs.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        data: &SkeletonData,
        sk: &Skeleton,
        cam: (f32, f32, f32),
        tau: f32,
        origin: (f32, f32),
        size: (f32, f32),
        view: [f32; 4],
        page_size: &dyn Fn(u32) -> Option<(f32, f32)>,
        emit: &mut dyn FnMut(&Call),
    ) -> u32 {
        let b = &self.bake;
        let pos = tau * b.rate;
        let f = (pos as i32).clamp(0, b.frames as i32 - 1) as u16;
        let alpha = (pos - f as f32).clamp(0.0, 1.0);
        if !self.seek(f) {
            return 0;
        }
        // Calls wholly outside the part drawn (path units) build no path.
        let (vx0, vy0, lx, ly) = (path_coord(view[0]), path_coord(view[1]), path_coord(view[2]), path_coord(view[3]));
        let k = bal2d_render::PATH_SCALE;
        let mut triangles = 0;
        let mut i = 0;
        while i < self.cur.len() {
            if self.cur[i].shape == bal2d_pack::IDLE_LIVE {
                let run = self.cur[i].color as usize;
                i += 1;
                if let Some(skip) = self.live_skip.get(run) {
                    bal2d_render::build_masked(data, sk, &mut self.live_list, Some(skip));
                    let before = self.live_paths.triangles;
                    self.live_paths.draw(0, &self.live_list, cam, size, page_size, |c| emit(c));
                    triangles += self.live_paths.triangles - before;
                }
                continue;
            }
            // A run of calls of one attachment (they are consecutive: one batch).
            let att = self.bake.shapes.get(self.cur[i].shape as usize).map(|s| (s.slot, s.attachment));
            let mut j = i + 1;
            while j < self.cur.len() && self.cur[j].shape != bal2d_pack::IDLE_LIVE && self.bake.shapes.get(self.cur[j].shape as usize).map(|s| (s.slot, s.attachment)) == att {
                j += 1;
            }
            let run = i..j;
            i = j;
            let Some((_, att)) = att else { continue };
            let Some((page, tris, verts)) = geometry(data, att) else { continue };
            let Some((pw, ph)) = page_size(page) else { continue };
            let uvs = uvs(data, att);
            if uvs.len() < verts * 2 {
                continue;
            }
            // Each call's texel -> target affine, the vertices' summed positions (and which
            // were touched), and the call's bounds from its own placement, a margin wider: the
            // averaging below moves a shared vertex by far less (the bounds cull and size the
            // GPU's tessellation window, which may be larger than the path).
            self.mats.clear();
            self.boxes.clear();
            self.touched.clear();
            if self.acc.len() < verts {
                self.acc.resize(verts, [0.0; 3]);
            }
            let margin = 2.0 * k;
            for c in &self.cur[run.clone()] {
                let shape = &self.bake.shapes[c.shape as usize];
                let m1 = c.m1.or_else(|| self.next.get(*self.next_of.get(c.shape as usize)? as usize).map(|n| n.m0)).unwrap_or(c.m0);
                let mut p = [0f32; 6];
                for q in 0..6 {
                    p[q] = c.m0[q] as f32 + (m1[q] as f32 - c.m0[q] as f32) * alpha;
                }
                let mut m = matrix(shape, &p);
                m[2] -= origin.0;
                m[5] -= origin.1;
                // uv -> path units.
                let u = [m[0] * pw * k, m[1] * ph * k, m[2] * k, m[3] * pw * k, m[4] * ph * k, m[5] * k];
                let vs = &self.verts[self.vert_at[c.shape as usize] as usize..self.vert_at[c.shape as usize + 1] as usize];
                let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                let acc = &mut self.acc[..verts];
                for &v in vs {
                    let v = v as usize;
                    // SAFETY: `IdleLayer::new` checked every vertex of every shape against the
                    // attachment's vertex count `verts`; `uvs` holds `verts` pairs (above).
                    let (x, y, a) = unsafe { (*uvs.get_unchecked(v * 2), *uvs.get_unchecked(v * 2 + 1), acc.get_unchecked_mut(v)) };
                    let (px, py) = (u[0] * x + u[1] * y + u[2], u[3] * x + u[4] * y + u[5]);
                    x0 = x0.min(px);
                    x1 = x1.max(px);
                    y0 = y0.min(py);
                    y1 = y1.max(py);
                    if a[2] == 0.0 {
                        self.touched.push(v as u16);
                    }
                    a[0] += px;
                    a[1] += py;
                    a[2] += 1.0;
                }
                self.boxes.push([(x0 - margin) as i16, (y0 - margin) as i16, (x1 + margin) as i16, (y1 + margin) as i16]);
                self.mats.push(m);
            }
            // Averaged once per vertex used (most by one call: no division then).
            if self.fixed.len() < verts {
                self.fixed.resize(verts, [0; 2]);
            }
            for &v in &self.touched {
                let (a, f) = (&mut self.acc[v as usize], &mut self.fixed[v as usize]);
                *f = if a[2] > 1.5 {
                    let w = 1.0 / a[2];
                    [(a[0] * w) as i16, (a[1] * w) as i16]
                } else {
                    [a[0] as i16, a[1] as i16]
                };
                *a = [0.0; 3];
            }
            for ((c, m), bx) in self.cur[run].iter().zip(&self.mats).zip(&self.boxes) {
                let shape = &self.bake.shapes[c.shape as usize];
                let path = &mut self.path;
                path.clear();
                let loops = &self.outline[self.outline_at[c.shape as usize] as usize..self.outline_at[c.shape as usize + 1] as usize];
                path.reserve(if loops.is_empty() { shape.tris.len() * 10 + 2 } else { loops.len() * 3 + 2 });
                let (lo, hi) = ([bx[0], bx[1]], [bx[2], bx[3]]);
                if hi[0] < vx0 || hi[1] < vy0 || lo[0] > lx || lo[1] > ly {
                    continue;
                }
                if loops.is_empty() {
                    for &t in &shape.tris {
                        let t = t as usize * 3;
                        let (p, q, r) = (self.fixed[tris[t] as usize], self.fixed[tris[t + 1] as usize], self.fixed[tris[t + 2] as usize]);
                        path.extend_from_slice(&[PATH_MOVE as i16, p[0], p[1], PATH_LINE as i16, q[0], q[1], PATH_LINE as i16, r[0], r[1], PATH_CLOSE as i16]);
                    }
                } else {
                    let mut first = true;
                    for &v in loops {
                        if v == u16::MAX {
                            path.push(PATH_CLOSE as i16);
                            first = true;
                            continue;
                        }
                        let q = self.fixed[v as usize];
                        path.extend_from_slice(&[if first { PATH_MOVE } else { PATH_LINE } as i16, q[0], q[1]]);
                        first = false;
                    }
                }
                if path.is_empty() {
                    continue;
                }
                path.push(0);
                if path.len() % 2 == 1 {
                    path.push(0);
                }
                let col = self.bake.colors.get(c.color as usize).copied().unwrap_or(u32::MAX);
                let color = [(col & 0xFF) as f32 / 255.0, (col >> 8 & 0xFF) as f32 / 255.0, (col >> 16 & 0xFF) as f32 / 255.0, (col >> 24) as f32 / 255.0];
                triangles += shape.tris.len() as u32;
                let (_, blend) = self.geo[c.shape as usize];
                emit(&Call { page, blend, color, path, bounds: [lo[0], lo[1], hi[0], hi[1]], matrix: *m, rect: None, tag: 1 << 31 | c.shape as u32 });
            }
        }
        triangles
    }
}

/// Reused buffers for `outline_of`.
#[derive(Default)]
struct OutlineScratch {
    edges: Vec<(u16, u16, bool)>,
    boundary: Vec<(u16, u16)>,
    used: Vec<bool>,
}

/// Appends to `out` the outline of the triangles `sel` (indices into `tris`, an attachment's
/// triangles): closed loops of vertices, `u16::MAX` after each, every edge directed as in its
/// triangle, so the loops filled non-zero cover exactly the triangles' union (holes included,
/// mirrored or not). False (and `out` partly written) when the triangles do not make one: an edge
/// shared the same way round or by more than two (folded or inconsistently wound), or loops
/// that do not close.
fn outline_of(tris: &[u16], sel: &[u16], s: &mut OutlineScratch, out: &mut Vec<u16>) -> bool {
    s.edges.clear();
    for &t in sel {
        let t = t as usize * 3;
        let v = [tris[t], tris[t + 1], tris[t + 2]];
        for k in 0..3 {
            let (a, b) = (v[k], v[(k + 1) % 3]);
            if a == b {
                return false;
            }
            s.edges.push((a.min(b), a.max(b), a < b));
        }
    }
    s.edges.sort_unstable();
    s.boundary.clear();
    let mut i = 0;
    while i < s.edges.len() {
        let mut j = i + 1;
        while j < s.edges.len() && (s.edges[j].0, s.edges[j].1) == (s.edges[i].0, s.edges[i].1) {
            j += 1;
        }
        match j - i {
            1 => {
                let (lo, hi, up) = s.edges[i];
                s.boundary.push(if up { (lo, hi) } else { (hi, lo) });
            }
            2 if s.edges[i].2 != s.edges[i + 1].2 => {}
            _ => return false,
        }
        i = j;
    }
    // Chain the directed boundary edges into loops (sorted by start: the next edge out of a
    // vertex is found by binary search).
    s.boundary.sort_unstable();
    s.used.clear();
    s.used.resize(s.boundary.len(), false);
    for e in 0..s.boundary.len() {
        if s.used[e] {
            continue;
        }
        let (start, mut at) = s.boundary[e];
        s.used[e] = true;
        out.push(start);
        let mut steps = 0;
        while at != start {
            let k = s.boundary.partition_point(|x| x.0 < at);
            let Some(n) = (k..s.boundary.len()).take_while(|&n| s.boundary[n].0 == at).find(|&n| !s.used[n]) else { return false };
            s.used[n] = true;
            out.push(at);
            at = s.boundary[n].1;
            steps += 1;
            if steps > s.boundary.len() {
                return false;
            }
        }
        out.push(u16::MAX);
    }
    !s.boundary.is_empty()
}

/// The texel -> screen affine of fixed-point parameters `p` (interpolated, so in f32).
pub fn matrix(s: &IdleShape, p: &[f32; 6]) -> [f32; 6] {
    IdleBake::matrix(s, p)
}

/// An attachment's page, triangles and vertex count as the draw list builds it.
pub fn geometry(data: &SkeletonData, id: u32) -> Option<(u32, &[u16], usize)> {
    match &data.attachments.get(id as usize)?.kind {
        AttachmentKind::Region(r) => Some((r.region?.page, &bal2d_render::QUAD_INDICES[..], 4)),
        AttachmentKind::Mesh(m) => (m.uvs.len() == m.vertices.vertex_count() * 2).then_some((m.region?.page, &m.triangles[..], m.vertices.vertex_count())),
        _ => None,
    }
}

fn uvs(data: &SkeletonData, id: u32) -> &[f32] {
    match &data.attachments[id as usize].kind {
        AttachmentKind::Region(r) => &r.uvs[..],
        AttachmentKind::Mesh(m) => &m.uvs[..],
        _ => &[],
    }
}

/// `x` modulo `p` (positive `p`), in [0, p).
pub fn wrap(x: f32, p: f32) -> f32 {
    let n = (x / p) as i64 as f32;
    let mut t = x - n * p;
    if t < 0.0 {
        t += p;
    }
    if t >= p { 0.0 } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Triangles of a `n` x `n` grid of quads (vertices row-major, (n + 1)^2), all one way round.
    fn grid(n: u16, skip: &[(u16, u16)]) -> Vec<u16> {
        let w = n + 1;
        let mut t = Vec::new();
        for y in 0..n {
            for x in 0..n {
                if skip.contains(&(x, y)) {
                    continue;
                }
                let (a, b, c, d) = (y * w + x, y * w + x + 1, (y + 1) * w + x + 1, (y + 1) * w + x);
                t.extend_from_slice(&[a, b, c, a, c, d]);
            }
        }
        t
    }

    fn loops(tris: &[u16]) -> Option<Vec<Vec<u16>>> {
        let sel: Vec<u16> = (0..(tris.len() / 3) as u16).collect();
        let mut out = Vec::new();
        if !outline_of(tris, &sel, &mut OutlineScratch::default(), &mut out) {
            return None;
        }
        Some(out.split(|&v| v == u16::MAX).filter(|l| !l.is_empty()).map(|l| l.to_vec()).collect())
    }

    #[test]
    fn a_grid_has_one_outline_around_its_border() {
        let l = loops(&grid(2, &[])).unwrap();
        assert_eq!(l.len(), 1);
        let mut v = l[0].clone();
        v.sort_unstable();
        // The 8 border vertices of a 3 x 3 vertex grid; the centre (4) is inside.
        assert_eq!(v, [0, 1, 2, 3, 5, 6, 7, 8]);
    }

    #[test]
    fn a_hole_is_a_second_loop_the_other_way_round() {
        let l = loops(&grid(3, &[(1, 1)])).unwrap();
        assert_eq!(l.len(), 2);
        let area = |l: &[u16]| -> i32 {
            let p = |v: u16| ((v % 4) as i32, (v / 4) as i32);
            (0..l.len()).map(|i| {
                let (a, b) = (p(l[i]), p(l[(i + 1) % l.len()]));
                a.0 * b.1 - b.0 * a.1
            }).sum()
        };
        let (a0, a1) = (area(&l[0]), area(&l[1]));
        assert!(a0.signum() == -a1.signum(), "outer and hole wind opposite ways: {a0} {a1}");
        assert_eq!(a0.abs() + a1.abs(), 2 * (9 + 1));
    }

    #[test]
    fn inconsistent_or_folded_triangles_have_no_outline() {
        // The same edge used the same way round twice (one triangle flipped).
        assert!(loops(&[0, 1, 2, 0, 2, 3, 0, 3, 2]).is_none());
        // A degenerate triangle.
        assert!(loops(&[0, 0, 1]).is_none());
    }
}

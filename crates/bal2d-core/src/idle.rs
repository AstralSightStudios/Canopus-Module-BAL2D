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
    /// Per shape: its triangle count (its triangles are kept only when it has no outline).
    tri_count: Vec<u16>,
    /// A clipped call's path before clipping, the visible rectangles in path units, and the
    /// clipper's buffers.
    raw: Vec<i16>,
    rects: [Vec<[i16; 4]>; 3],
    clip: ClipScratch,
    /// Per region: per row of 16-pixel cells, a bit per cell some visible rectangle meets (none
    /// for an empty region). A call whose box meets none of them is wholly covered.
    cells: [Vec<u32>; 3],
    /// Work counters (tools): vertices placed, calls clipped, calls left out as covered.
    pub work: [u32; 4],
    /// Per slot: drawn from the bake during a talk (`IdleBake::steady`, less the live runs'
    /// slots); empty when the bake has none. A talk's frame draws the rest live, in runs of
    /// consecutive slots of the draw order (`hybrid_runs`, positions), interleaved with the
    /// baked calls by draw order (`pos_of`).
    steady: Vec<bool>,
    pos_of: Vec<u16>,
    hybrid_runs: Vec<(u16, u16)>,
    hybrid_skip: Vec<bool>,
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
    /// Per shape: the box of its vertices' texture coordinates (u0, v0, u1, v1): its affine
    /// maps it onto a parallelogram holding the whole shape, whose corners bound the call.
    uv_box: Vec<[f32; 4]>,
    /// Per live run: the slots left out of its draw list; and the list and grouping it uses.
    live_skip: Vec<Vec<bool>>,
    live_list: DrawList,
    live_paths: Paths,
}

impl IdleLayer {
    /// The bake for a layer of `data`, when every shape names a drawable attachment of a slot
    /// and triangles it has.
    pub fn new(mut bake: IdleBake, data: &SkeletonData) -> Option<IdleLayer> {
        let n = bake.shapes.len();
        // Fallible: the watch loads this last, into whatever memory is left.
        let (mut geo, mut all, mut vert_at, mut outline, mut outline_at, mut tri_count) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        geo.try_reserve_exact(n).ok()?;
        vert_at.try_reserve_exact(n + 1).ok()?;
        outline_at.try_reserve_exact(n + 1).ok()?;
        tri_count.try_reserve_exact(n).ok()?;
        let mut used: Vec<u16> = Vec::new();
        let mut uv_box: Vec<[f32; 4]> = Vec::new();
        uv_box.try_reserve_exact(n).ok()?;
        let mut scratch = OutlineScratch::default();
        vert_at.push(0u32);
        outline_at.push(0u32);
        for m in &bake.meshes {
            let (_, _, verts) = geometry(data, m.attachment)?;
            if m.uvs.len() < verts * 2 || m.tris.iter().any(|&v| v as usize * 2 >= m.uvs.len()) {
                return None;
            }
        }
        // Outlines.
        for s in &bake.shapes {
            let slot = data.slots.get(s.slot as usize)?;
            let (page, tris, verts) = geometry_in(&bake, data, s.attachment)?;
            let uv = uvs_in(&bake, data, s.attachment);
            if uv.len() < verts * 2 || s.tris.iter().any(|&t| (t as usize + 1) * 3 > tris.len()) || tris.iter().any(|&v| v as usize >= verts) {
                return None;
            }
            geo.push((page, slot.blend));
            let start = outline.len();
            if !outline_of(tris, &s.tris, &mut scratch, &mut outline) {
                outline.truncate(start);
            }
            outline_at.push(outline.len() as u32);
            tri_count.push(s.tris.len() as u16);
        }
        drop(scratch);
        if !bake.meshes.is_empty() {
            let (o, at) = straighten(&bake, data, &outline, &outline_at)?;
            outline = o;
            outline_at = at;
        }
        // The vertices each shape is drawn with, and their texture box. A shape with an outline
        // needs its triangles no more.
        for (i, s) in bake.shapes.iter_mut().enumerate() {
            let (_, tris, _) = geometry_in_parts(&bake.meshes, data, s.attachment)?;
            used.clear();
            let loops = &outline[outline_at[i] as usize..outline_at[i + 1] as usize];
            if loops.is_empty() {
                for &t in &s.tris {
                    used.extend_from_slice(&tris[t as usize * 3..t as usize * 3 + 3]);
                }
            } else {
                used.extend(loops.iter().copied().filter(|&v| v != u16::MAX));
                s.tris = Vec::new();
            }
            used.sort_unstable();
            used.dedup();
            let uv = uvs_in_parts(&bake.meshes, data, s.attachment);
            let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for &v in &used {
                let (x, y) = (uv[v as usize * 2], uv[v as usize * 2 + 1]);
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
            uv_box.push(b);
            all.try_reserve(used.len()).ok()?;
            all.extend_from_slice(&used);
            vert_at.push(all.len() as u32);
        }
        // A finer mesh's triangles, once every shape of it has an outline.
        for m in &mut bake.meshes {
            if bake.shapes.iter().all(|s| s.attachment != m.attachment || s.tris.is_empty()) {
                m.tris = Vec::new();
            }
        }
        all.shrink_to_fit();
        outline.shrink_to_fit();
        // Slots that stay on the bake during a talk: not those of live runs.
        let mut steady: Vec<bool> = Vec::new();
        if !bake.steady.is_empty() {
            steady = (0..data.slots.len()).map(|i| bake.steady.get(i / 8).is_some_and(|b| b >> (i % 8) & 1 != 0)).collect();
            for run in &bake.live {
                for &sl in run {
                    if let Some(x) = steady.get_mut(sl as usize) {
                        *x = false;
                    }
                }
            }
        }
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
            tri_count,
            verts: all,
            vert_at,
            outline,
            outline_at,
            raw: Vec::new(),
            rects: Default::default(),
            clip: ClipScratch::default(),
            cells: core::array::from_fn(|i| cells_of(&bake.regions[i])),
            work: [0; 4],
            steady,
            pos_of: Vec::new(),
            hybrid_runs: Vec::new(),
            hybrid_skip: Vec::new(),
            path: Vec::new(),
            acc: Vec::new(),
            fixed: Vec::new(),
            mats: Vec::new(),
            boxes: Vec::new(),
            touched: Vec::new(),
            uv_box,
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

    /// The clustering tolerance of what is drawn live (`Lobby::live_tolerance`).
    pub fn live_paths_tolerance(&mut self, t: f32) {
        self.live_paths.tolerance = t;
    }

    /// Memory held besides the bake, by part (for tools).
    pub fn sizes(&self) -> alloc::string::String {
        alloc::format!(
            "outline {} KB, verts {} KB, per-shape {} KB",
            self.outline.capacity() * 2 / 1024,
            self.verts.capacity() * 2 / 1024,
            (self.vert_at.capacity() * 4 + self.outline_at.capacity() * 4 + self.uv_box.capacity() * 16 + self.geo.capacity() * 8 + self.next_of.capacity() * 2) / 1024
        )
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
    /// runs. `above`: every layer above is drawn from its bake this frame (what they always
    /// cover can be clipped away); `clip`: clip the shapes marked for it (else only calls wholly
    /// covered are left out); `hybrid`: a talk plays, `sk` is posed, and only the steady slots
    /// come from the bake (`IdleBake::steady`), the rest is drawn live.
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
        above: bool,
        clip: bool,
        hybrid: bool,
        page_size: &dyn Fn(u32) -> Option<(f32, f32)>,
        emit: &mut dyn FnMut(&Call),
    ) -> u32 {
        let hybrid = hybrid && !self.steady.is_empty();
        // The visible rectangles in path units of this target.
        let k = bal2d_render::PATH_SCALE;
        for (out, reg) in self.rects.iter_mut().zip(&self.bake.regions) {
            out.clear();
            out.extend(reg.iter().map(|q| {
                let f = |v: i16, o: f32| path_coord(v as f32 - o);
                [f(q[0], origin.0), f(q[1], origin.1), f(q[2], origin.0), f(q[3], origin.1)]
            }));
        }
        // In a talk's frame the layer's own later calls may be live and move: no "later" cover.
        let clip_mask = if hybrid { 0 } else { bal2d_pack::IDLE_CLIP_LATER } | if above { bal2d_pack::IDLE_CLIP_ABOVE } else { 0 };
        // The region a shape's calls must meet to show (index into `rects`), by its bits.
        let has: [bool; 3] = core::array::from_fn(|i| !self.bake.regions[i].is_empty() && !(hybrid && i != 1));
        let shows_in = move |clip: u8| -> Option<usize> {
            let before = clip & bal2d_pack::IDLE_BEFORE != 0 && has[0];
            match (before, above && has[1]) {
                (true, true) => Some(2),
                (true, false) => Some(0),
                (false, true) => Some(1),
                _ => None,
            }
        };
        let b = &self.bake;
        let pos = tau * b.rate;
        let f = (pos as i32).clamp(0, b.frames as i32 - 1) as u16;
        let alpha = (pos - f as f32).clamp(0.0, 1.0);
        if !self.seek(f) {
            return 0;
        }
        // Calls wholly outside the part drawn (path units) build no path.
        let (vx0, vy0, lx, ly) = (path_coord(view[0]), path_coord(view[1]), path_coord(view[2]), path_coord(view[3]));
        let mut triangles = 0;
        // A talk's frame: the runs of slots drawn live, by draw-order position.
        let mut next_run = 0;
        if hybrid {
            self.pos_of.clear();
            self.pos_of.resize(data.slots.len(), u16::MAX);
            self.hybrid_runs.clear();
            let mut open: Option<u16> = None;
            for (p, &slot) in sk.draw_order.iter().enumerate() {
                if let Some(x) = self.pos_of.get_mut(slot as usize) {
                    *x = p as u16;
                }
                let live = !self.steady.get(slot as usize).copied().unwrap_or(false);
                match (live, open) {
                    (true, None) => open = Some(p as u16),
                    (false, Some(a)) => {
                        self.hybrid_runs.push((a, p as u16));
                        open = None;
                    }
                    _ => {}
                }
            }
            if let Some(a) = open {
                self.hybrid_runs.push((a, sk.draw_order.len() as u16));
            }
        }
        let mut i = 0;
        while i < self.cur.len() {
            if hybrid {
                if self.cur[i].shape == bal2d_pack::IDLE_LIVE {
                    i += 1;
                    continue;
                }
                let slot = self.bake.shapes.get(self.cur[i].shape as usize).map_or(0, |s| s.slot);
                let p = self.pos_of.get(slot as usize).copied().unwrap_or(u16::MAX);
                while next_run < self.hybrid_runs.len() && self.hybrid_runs[next_run].0 < p {
                    triangles += self.draw_run(data, sk, cam, size, page_size, emit, next_run);
                    next_run += 1;
                }
                if !self.steady.get(slot as usize).copied().unwrap_or(false) {
                    i += 1;
                    continue;
                }
            }
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
            let Some((page, tris, verts)) = geometry_in(&self.bake, data, att) else { continue };
            let Some((pw, ph)) = page_size(page) else { continue };
            let uvs = uvs_in(&self.bake, data, att);
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
                let v = c.shape as usize;
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
                let vs = &self.verts[self.vert_at[v] as usize..self.vert_at[v + 1] as usize];
                self.work[0] += vs.len() as u32;
                let acc = &mut self.acc[..verts];
                for &v in vs {
                    let v = v as usize;
                    // SAFETY: `IdleLayer::new` checked every vertex of every shape against the
                    // attachment's vertex count `verts`; `uvs` holds `verts` pairs (above).
                    let (x, y, a) = unsafe { (*uvs.get_unchecked(v * 2), *uvs.get_unchecked(v * 2 + 1), acc.get_unchecked_mut(v)) };
                    if a[2] == 0.0 {
                        self.touched.push(v as u16);
                    }
                    a[0] += u[0] * x + u[1] * y + u[2];
                    a[1] += u[3] * x + u[4] * y + u[5];
                    a[2] += 1.0;
                }
                // The texture box's corners, placed: they bound every vertex of the call.
                let tb = self.uv_box[v];
                let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for (x, y) in [(tb[0], tb[1]), (tb[2], tb[1]), (tb[0], tb[3]), (tb[2], tb[3])] {
                    let (px, py) = (u[0] * x + u[1] * y + u[2], u[3] * x + u[4] * y + u[5]);
                    x0 = x0.min(px);
                    x1 = x1.max(px);
                    y0 = y0.min(py);
                    y1 = y1.max(py);
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
                let v = c.shape as usize;
                let sel = &shape.tris;
                if self.tri_count[v] == 0 {
                    continue;
                }
                let path = &mut self.path;
                path.clear();
                let loops = &self.outline[self.outline_at[v] as usize..self.outline_at[v + 1] as usize];
                path.reserve(if loops.is_empty() { sel.len() * 10 + 2 } else { loops.len() * 3 + 2 });
                let (mut lo, mut hi) = ([bx[0], bx[1]], [bx[2], bx[3]]);
                if hi[0] < vx0 || hi[1] < vy0 || lo[0] > lx || lo[1] > ly {
                    continue;
                }
                // Wholly under what is always covered: no path, no driver call.
                if let Some(ri) = shows_in(shape.clip) {
                    let px = |v: i16, o: f32| (v as f32 / k + o) as i32;
                    if !meets(&self.cells[ri], [px(lo[0], origin.0), px(lo[1], origin.1), px(hi[0], origin.0), px(hi[1], origin.1)]) {
                        self.work[2] += 1;
                        continue;
                    }
                }
                if loops.is_empty() {
                    for &t in sel {
                        let t = t as usize * 3;
                        let (p, q, r) = (self.fixed[tris[t] as usize], self.fixed[tris[t + 1] as usize], self.fixed[tris[t + 2] as usize]);
                        path.extend_from_slice(&[PATH_MOVE as i16, p[0], p[1], PATH_LINE as i16, q[0], q[1], PATH_LINE as i16, r[0], r[1], PATH_CLOSE as i16]);
                    }
                } else {
                    // At most three elements per loop entry, room reserved above: written
                    // without a capacity check each.
                    let fixed = &self.fixed[..verts];
                    let room = &mut path.spare_capacity_mut()[..loops.len() * 3];
                    let mut n = 0;
                    let mut op = PATH_MOVE as i16;
                    for &v in loops {
                        if v == u16::MAX {
                            room[n].write(PATH_CLOSE as i16);
                            n += 1;
                            op = PATH_MOVE as i16;
                            continue;
                        }
                        // SAFETY: outline vertices are the attachment's (checked at load) and
                        // `n + 3 <= 3 * entries seen <= room.len()`.
                        unsafe {
                            let q = *fixed.get_unchecked(v as usize);
                            room.get_unchecked_mut(n).write(op);
                            room.get_unchecked_mut(n + 1).write(q[0]);
                            room.get_unchecked_mut(n + 2).write(q[1]);
                        }
                        n += 3;
                        op = PATH_LINE as i16;
                    }
                    // SAFETY: the first `n` elements were written.
                    unsafe { path.set_len(n) };
                }
                if path.is_empty() {
                    continue;
                }
                let which = if clip { shape.clip & clip_mask & 3 } else { 0 };
                if which != 0 && !self.rects[which as usize - 1].is_empty() {
                    let rects = &self.rects[which as usize - 1];
                    if !rects.iter().any(|r| r[0] <= lo[0] && r[1] <= lo[1] && r[2] >= hi[0] && r[3] >= hi[1]) {
                        core::mem::swap(path, &mut self.raw);
                        path.clear();
                        self.work[1] += 1;
                        self.work[3] += self.raw.len() as u32;
                        let b = clip_to(&self.raw, rects, [lo[0], lo[1], hi[0], hi[1]], path, &mut self.clip);
                        if path.is_empty() {
                            continue;
                        }
                        // The bounds of what is left, the margin kept.
                        lo = [lo[0].max(b[0] - 2 * k as i16), lo[1].max(b[1] - 2 * k as i16)];
                        hi = [hi[0].min(b[2] + 2 * k as i16), hi[1].min(b[3] + 2 * k as i16)];
                    }
                }
                path.push(0);
                if path.len() % 2 == 1 {
                    path.push(0);
                }
                let col = self.bake.colors.get(c.color as usize).copied().unwrap_or(u32::MAX);
                let color = [(col & 0xFF) as f32 / 255.0, (col >> 8 & 0xFF) as f32 / 255.0, (col >> 16 & 0xFF) as f32 / 255.0, (col >> 24) as f32 / 255.0];
                triangles += self.tri_count[v] as u32;
                let (_, blend) = self.geo[c.shape as usize];
                emit(&Call { page, blend, color, path, bounds: [lo[0], lo[1], hi[0], hi[1]], matrix: *m, rect: None, tag: 1 << 31 | c.shape as u32 });
            }
        }
        while hybrid && next_run < self.hybrid_runs.len() {
            triangles += self.draw_run(data, sk, cam, size, page_size, emit, next_run);
            next_run += 1;
        }

        triangles
    }

    /// Draws live the slots `only` marks (as the draw list builds them). Returns its triangles.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_slots_live(&mut self, data: &SkeletonData, sk: &Skeleton, cam: (f32, f32, f32), size: (f32, f32), page_size: &dyn Fn(u32) -> Option<(f32, f32)>, emit: &mut dyn FnMut(&Call), only: &[bool]) -> u32 {
        self.hybrid_skip.clear();
        self.hybrid_skip.extend(only.iter().map(|&x| !x));
        bal2d_render::build_masked(data, sk, &mut self.live_list, Some(&self.hybrid_skip));
        let before = self.live_paths.triangles;
        self.live_paths.draw(0, &self.live_list, cam, size, page_size, |c| emit(c));
        self.live_paths.triangles - before
    }

    /// Draws live the slots of a talk frame's run `k` (`hybrid_runs`). Returns its triangles.
    #[allow(clippy::too_many_arguments)]
    fn draw_run(&mut self, data: &SkeletonData, sk: &Skeleton, cam: (f32, f32, f32), size: (f32, f32), page_size: &dyn Fn(u32) -> Option<(f32, f32)>, emit: &mut dyn FnMut(&Call), k: usize) -> u32 {
        let (a, b) = self.hybrid_runs[k];
        self.hybrid_skip.clear();
        self.hybrid_skip.resize(data.slots.len(), true);
        for &slot in &sk.draw_order[a as usize..b as usize] {
            if let Some(x) = self.hybrid_skip.get_mut(slot as usize) {
                *x = false;
            }
        }
        bal2d_render::build_masked(data, sk, &mut self.live_list, Some(&self.hybrid_skip));
        let before = self.live_paths.triangles;
        self.live_paths.draw(0, &self.live_list, cam, size, page_size, |c| emit(c));
        self.live_paths.triangles - before
    }
}

/// Cells (16 pixels) some rectangle of `rects` meets, per row (see `IdleLayer::cells`).
fn cells_of(rects: &[[i16; 4]]) -> Vec<u32> {
    let rows = rects.iter().map(|r| (r[3].max(0) as usize).div_ceil(16)).max().unwrap_or(0).min(64);
    let mut g = alloc::vec![0u32; rows];
    for r in rects {
        let (c0, c1) = ((r[0].max(0) as usize / 16).min(31), ((r[2] - 1).max(0) as usize / 16).min(31));
        let bits = (u32::MAX >> (31 - c1)) & (u32::MAX << c0);
        for row in g.iter_mut().take((r[3] - 1).max(0) as usize / 16 + 1).skip(r[1].max(0) as usize / 16) {
            *row |= bits;
        }
    }
    g
}

/// Whether the box (pixels, inclusive) meets a marked cell of `g`; true for an empty grid (no
/// region) and for anything past the grid's right or bottom (not known covered).
fn meets(g: &[u32], b: [i32; 4]) -> bool {
    if g.is_empty() || b[2] >= 512 || b[3] >= g.len() as i32 * 16 {
        return true;
    }
    let (x0, y0, x1, y1) = (b[0].max(0), b[1].max(0), b[2].max(0), b[3].max(0));
    let (c0, c1) = ((x0 / 16) as u32, (x1 / 16) as u32);
    if c1 > 31 {
        return true;
    }
    let bits = (u32::MAX >> (31 - c1)) & (u32::MAX << c0);
    g[(y0 / 16) as usize..=(y1 / 16) as usize].iter().any(|r| r & bits != 0)
}

/// Whether convex loop `pts` (up to 8 points, any winding) holds rectangle `r` whole: its
/// winding then (true: positive area in path units, x right, y down), else None.
fn holds(pts: &[[i16; 2]], r: &[i16; 4]) -> Option<bool> {
    if pts.len() > 8 {
        return None;
    }
    let n = pts.len();
    let mut area = 0i64;
    for j in 0..n {
        let (p, q) = (pts[j], pts[(j + 1) % n]);
        area += p[0] as i64 * q[1] as i64 - q[0] as i64 * p[1] as i64;
    }
    if area == 0 {
        return None;
    }
    let sign = area.signum();
    let corners = [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])];
    for j in 0..n {
        let (p, q) = (pts[j], pts[(j + 1) % n]);
        let (ex, ey) = ((q[0] - p[0]) as i32, (q[1] - p[1]) as i32);
        for &(x, y) in &corners {
            // Every corner on the inner side of every edge (convex: the loop's area sign).
            let c = ex * (y as i32 - p[1] as i32) - ey * (x as i32 - p[0] as i32);
            if (c as i64) * sign < 0 {
                return None;
            }
        }
    }
    Some(sign > 0)
}

/// Reused buffers for `clip_to`.
#[derive(Default)]
struct ClipScratch {
    pts: Vec<[i16; 2]>,
    /// Per loop: its points `pts[a..b]` and box.
    loops: Vec<(u32, u32, [i16; 4])>,
    a: Vec<[f32; 2]>,
    b: Vec<[f32; 2]>,
}

/// Appends to `out` the closed loops of `path` (MOVE x y LINE x y ... CLOSE, path units) clipped
/// to each of `rects` the box `bx` (x0, y0, x1, y1) of the path meets, loop by loop
/// (Sutherland-Hodgman: a loop's winding number inside a rectangle is kept, so holes fill as
/// before; rectangles may overlap, the GPU fills non-zero). A loop inside a rectangle is copied,
/// one apart from it skipped. Returns the box of what was written.
#[inline(never)]
fn clip_to(path: &[i16], rects: &[[i16; 4]], bx: [i16; 4], out: &mut Vec<i16>, s: &mut ClipScratch) -> [i16; 4] {
    let (mv, ln, cl) = (PATH_MOVE as i16, PATH_LINE as i16, PATH_CLOSE as i16);
    s.pts.clear();
    s.loops.clear();
    let mut i = 0;
    while i < path.len() {
        let start = s.pts.len() as u32;
        let mut b = [i16::MAX, i16::MAX, i16::MIN, i16::MIN];
        while i + 2 < path.len() && (path[i] == mv || path[i] == ln) {
            if path[i] == mv && s.pts.len() as u32 > start {
                break;
            }
            let (x, y) = (path[i + 1], path[i + 2]);
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            s.pts.push([x, y]);
            i += 3;
        }
        if i < path.len() && path[i] == cl {
            i += 1;
        } else if s.pts.len() as u32 == start {
            i += 1;
            continue;
        }
        if s.pts.len() as u32 - start >= 3 {
            s.loops.push((start, s.pts.len() as u32, b));
        } else {
            s.pts.truncate(start as usize);
        }
    }
    let mut lo = [i16::MAX; 2];
    let mut hi = [i16::MIN; 2];
    let mut emit = |out: &mut Vec<i16>, x: i16, y: i16, first: bool| {
        out.extend_from_slice(&[if first { mv } else { ln }, x, y]);
        lo = [lo[0].min(x), lo[1].min(y)];
        hi = [hi[0].max(x), hi[1].max(y)];
    };
    for r in rects {
        if r[2] <= bx[0] || r[3] <= bx[1] || r[0] > bx[2] || r[1] > bx[3] {
            continue;
        }
        for &(a0, a1, lb) in &s.loops {
            if lb[2] <= r[0] || lb[3] <= r[1] || lb[0] >= r[2] || lb[1] >= r[3] {
                continue;
            }
            let pts = &s.pts[a0 as usize..a1 as usize];
            if lb[0] >= r[0] && lb[1] >= r[1] && lb[2] <= r[2] && lb[3] <= r[3] {
                for (j, p) in pts.iter().enumerate() {
                    emit(out, p[0], p[1], j == 0);
                }
                out.push(cl);
                continue;
            }
            // A convex loop holding the whole rectangle (a background quad): the rectangle, wound
            // as the loop is.
            if let Some(ccw) = holds(pts, r) {
                let c = if ccw { [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])] } else { [(r[0], r[1]), (r[0], r[3]), (r[2], r[3]), (r[2], r[1])] };
                for (j, &(x, y)) in c.iter().enumerate() {
                    emit(out, x, y, j == 0);
                }
                out.push(cl);
                continue;
            }
            s.a.clear();
            s.a.extend(pts.iter().map(|p| [p[0] as f32, p[1] as f32]));
            // Against x >= x0, x <= x1, y >= y0, y <= y1 (only the sides the loop crosses).
            for (axis, edge, keep_ge) in [(0usize, r[0], true), (0, r[2], false), (1, r[1], true), (1, r[3], false)] {
                let crosses = if keep_ge { lb[axis] < edge } else { lb[axis + 2] > edge };
                if !crosses || s.a.is_empty() {
                    continue;
                }
                let e = edge as f32;
                s.b.clear();
                let n = s.a.len();
                for j in 0..n {
                    let (p, q) = (s.a[j], s.a[if j + 1 == n { 0 } else { j + 1 }]);
                    let (pi, qi) = if keep_ge { (p[axis] >= e, q[axis] >= e) } else { (p[axis] <= e, q[axis] <= e) };
                    if pi {
                        s.b.push(p);
                    }
                    if pi != qi {
                        let o = 1 - axis;
                        let t = (e - p[axis]) / (q[axis] - p[axis]);
                        let mut x = [0f32; 2];
                        x[axis] = e;
                        x[o] = p[o] + (q[o] - p[o]) * t;
                        s.b.push(x);
                    }
                }
                core::mem::swap(&mut s.a, &mut s.b);
            }
            if s.a.len() < 3 {
                continue;
            }
            for (j, p) in s.a.iter().enumerate() {
                let r = |v: f32| (if v >= 0.0 { v + 0.5 } else { v - 0.5 }) as i16;
                emit(out, r(p[0]), r(p[1]), j == 0);
            }
            out.push(cl);
        }
    }
    [lo[0], lo[1], hi[0], hi[1]]
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

/// An attachment's page, triangles and vertex count in a bake: its [`bal2d_pack::IdleMesh`]
/// when the bake has one, else its own.
pub fn geometry_in<'a>(bake: &'a IdleBake, data: &'a SkeletonData, id: u32) -> Option<(u32, &'a [u16], usize)> {
    geometry_in_parts(&bake.meshes, data, id)
}

fn mesh_of(meshes: &[bal2d_pack::IdleMesh], id: u32) -> Option<&bal2d_pack::IdleMesh> {
    meshes.binary_search_by_key(&id, |m| m.attachment).ok().map(|i| &meshes[i])
}

fn geometry_in_parts<'a>(meshes: &'a [bal2d_pack::IdleMesh], data: &'a SkeletonData, id: u32) -> Option<(u32, &'a [u16], usize)> {
    let own = geometry(data, id)?;
    Some(match mesh_of(meshes, id) {
        Some(m) => (own.0, &m.tris[..], m.uvs.len() / 2),
        None => own,
    })
}

/// An attachment's texture coordinates in a bake (see [`geometry_in`]).
pub fn uvs_in<'a>(bake: &'a IdleBake, data: &'a SkeletonData, id: u32) -> &'a [f32] {
    uvs_in_parts(&bake.meshes, data, id)
}

fn uvs_in_parts<'a>(meshes: &'a [bal2d_pack::IdleMesh], data: &'a SkeletonData, id: u32) -> &'a [f32] {
    match mesh_of(meshes, id) {
        Some(m) => &m.uvs[..],
        None => uvs(data, id),
    }
}

/// The outlines with the split points of finer meshes left out where an outline runs straight
/// through them and no other shape of the same frame has them (`IdleMesh::solo`): a split
/// attachment's border would otherwise carry every split point; one a neighbour shares stays,
/// so neighbours keep meeting exactly.
fn straighten(bake: &IdleBake, data: &SkeletonData, outline: &[u16], outline_at: &[u32]) -> Option<(Vec<u16>, Vec<u32>)> {
    let n = bake.shapes.len();
    let mut own: Vec<usize> = Vec::with_capacity(bake.meshes.len());
    for m in &bake.meshes {
        own.push(geometry(data, m.attachment)?.2);
    }
    let mesh_index = |att: u32| bake.meshes.binary_search_by_key(&att, |m| m.attachment).ok();
    let mut out: Vec<u16> = Vec::new();
    out.try_reserve(outline.len()).ok()?;
    let mut at: Vec<u32> = Vec::new();
    at.try_reserve_exact(n + 1).ok()?;
    at.push(0);
    let mut lp: Vec<u16> = Vec::new();
    for (i, s) in bake.shapes.iter().enumerate() {
        let loops = &outline[outline_at[i] as usize..outline_at[i + 1] as usize];
        let Some(mi) = mesh_index(s.attachment) else {
            out.extend_from_slice(loops);
            at.push(out.len() as u32);
            continue;
        };
        let uv = &bake.meshes[mi].uvs;
        let p = |v: u16| (uv[v as usize * 2], uv[v as usize * 2 + 1]);
        for l in loops.split(|&v| v == u16::MAX).filter(|l| !l.is_empty()) {
            lp.clear();
            let k = l.len();
            for j in 0..k {
                let v = l[j];
                let next = l[(j + 1) % k];
                let prev = lp.last().copied().unwrap_or(l[(j + k - 1) % k]);
                let solo = bake.meshes[mi].solo.get(v as usize / 8).is_some_and(|b| b >> (v % 8) & 1 != 0);
                let droppable = v as usize >= own[mi] && solo && {
                    // On the segment prev -> next (in texture space, so on the screen too).
                    let (a, b, c) = (p(prev), p(v), p(next));
                    let (ux, uy, wx, wy) = (b.0 - a.0, b.1 - a.1, c.0 - a.0, c.1 - a.1);
                    let cross = ux * wy - uy * wx;
                    let dot = ux * wx + uy * wy;
                    cross * cross <= 1e-10 * (ux * ux + uy * uy) * (wx * wx + wy * wy) && dot > 0.0 && dot < wx * wx + wy * wy
                };
                if !droppable {
                    lp.push(v);
                }
            }
            if lp.len() < 3 {
                lp.clear();
                lp.extend_from_slice(l);
            }
            out.extend_from_slice(&lp);
            out.push(u16::MAX);
        }
        at.push(out.len() as u32);
    }
    Some((out, at))
}

/// An attachment's page, triangles and vertex count as the draw list builds it.
pub fn geometry(data: &SkeletonData, id: u32) -> Option<(u32, &[u16], usize)> {
    match &data.attachments.get(id as usize)?.kind {
        AttachmentKind::Region(r) => Some((r.region?.page, &bal2d_render::QUAD_INDICES[..], 4)),
        AttachmentKind::Mesh(m) => (m.uvs.len() == m.vertices.vertex_count() * 2).then_some((m.region?.page, &m.triangles[..], m.vertices.vertex_count())),
        _ => None,
    }
}

/// An attachment's own texture coordinates (0..1 of its page).
pub fn uvs(data: &SkeletonData, id: u32) -> &[f32] {
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
    use alloc::vec;

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

    /// Pixel centres a path fills (non-zero rule), on a `w` x `h` grid of path units / 8.
    fn filled(path: &[i16], w: i32, h: i32) -> Vec<bool> {
        let mut loops: Vec<Vec<[f32; 2]>> = Vec::new();
        let mut i = 0;
        while i < path.len() {
            match path[i] as u32 {
                PATH_MOVE => {
                    loops.push(vec![[path[i + 1] as f32, path[i + 2] as f32]]);
                    i += 3;
                }
                PATH_LINE => {
                    loops.last_mut().unwrap().push([path[i + 1] as f32, path[i + 2] as f32]);
                    i += 3;
                }
                _ => i += 1,
            }
        }
        let mut out = vec![false; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let (px, py) = ((x as f32 + 0.5) * 8.0, (y as f32 + 0.5) * 8.0);
                let mut wind = 0;
                for l in &loops {
                    for j in 0..l.len() {
                        let (a, b) = (l[j], l[(j + 1) % l.len()]);
                        let side = (b[0] - a[0]) * (py - a[1]) - (px - a[0]) * (b[1] - a[1]);
                        if a[1] <= py && b[1] > py && side > 0.0 {
                            wind += 1;
                        } else if a[1] > py && b[1] <= py && side < 0.0 {
                            wind -= 1;
                        }
                    }
                }
                out[(y * w + x) as usize] = wind != 0;
            }
        }
        out
    }

    fn loop_path(pts: &[(i16, i16)], out: &mut Vec<i16>) {
        for (j, &(x, y)) in pts.iter().enumerate() {
            out.extend_from_slice(&[if j == 0 { PATH_MOVE as i16 } else { PATH_LINE as i16 }, x * 8, y * 8]);
        }
        out.push(PATH_CLOSE as i16);
    }

    #[test]
    fn clipping_to_overlapping_rectangles_keeps_what_they_cover() {
        // A square with a square hole (wound the other way), and a concave L.
        let mut path = Vec::new();
        loop_path(&[(2, 2), (30, 2), (30, 30), (2, 30)], &mut path);
        loop_path(&[(10, 10), (10, 20), (20, 20), (20, 10)], &mut path);
        loop_path(&[(34, 2), (46, 2), (46, 8), (40, 8), (40, 30), (34, 30)], &mut path);
        let rects: Vec<[i16; 4]> = [[0, 0, 16, 40], [8, 4, 44, 14], [36, 20, 48, 40], [24, 26, 28, 28]].iter().map(|r| r.map(|v| v * 8)).collect();
        let mut out = Vec::new();
        let b = clip_to(&path, &rects, [0, 0, 48 * 8, 40 * 8], &mut out, &mut ClipScratch::default());
        let (whole, clipped) = (filled(&path, 48, 40), filled(&out, 48, 40));
        for y in 0..40 {
            for x in 0..48 {
                let inside = rects.iter().any(|r| x * 8 >= r[0] as i32 && x * 8 + 8 <= r[2] as i32 && y * 8 >= r[1] as i32 && y * 8 + 8 <= r[3] as i32);
                let i = (y * 48 + x) as usize;
                if inside {
                    assert_eq!(clipped[i], whole[i], "pixel {x},{y} inside a rectangle");
                } else if !rects.iter().any(|r| x * 8 + 8 > r[0] as i32 && x * 8 < r[2] as i32 && y * 8 + 8 > r[1] as i32 && y * 8 < r[3] as i32) {
                    assert!(!clipped[i], "pixel {x},{y} outside every rectangle");
                }
            }
        }
        assert!(b[0] >= 0 && b[2] <= 46 * 8 && b[3] <= 40 * 8, "{b:?}");
    }

    #[test]
    fn a_convex_loop_holding_a_rectangle_gives_the_rectangle() {
        let quad = [[0i16, 0], [400, 0], [400, 300], [0, 300]];
        assert_eq!(holds(&quad, &[10, 10, 100, 100]), Some(true));
        let rev: Vec<[i16; 2]> = quad.iter().rev().copied().collect();
        assert_eq!(holds(&rev, &[10, 10, 100, 100]), Some(false));
        assert_eq!(holds(&quad, &[350, 10, 450, 100]), None);
        let mut out = Vec::new();
        let mut path = Vec::new();
        loop_path(&[(0, 0), (50, 0), (50, 37), (0, 37)], &mut path);
        clip_to(&path, &[[80, 80, 160, 160]], [0, 0, 400, 300], &mut out, &mut ClipScratch::default());
        assert_eq!(out, [PATH_MOVE as i16, 80, 80, PATH_LINE as i16, 160, 80, PATH_LINE as i16, 160, 160, PATH_LINE as i16, 80, 160, PATH_CLOSE as i16]);
    }

    #[test]
    fn cells_mark_what_the_rectangles_meet() {
        let g = cells_of(&[[0, 0, 40, 16], [100, 40, 101, 64]]);
        assert_eq!(g.len(), 4);
        assert_eq!(g[0], 0b111);
        assert_eq!(g[1], 0);
        assert_eq!(g[2], 1 << 6);
        assert!(meets(&g, [0, 0, 5, 5]));
        assert!(!meets(&g, [50, 0, 90, 30]));
        assert!(meets(&g, [96, 33, 99, 40]));
        // Past the grid: not known covered.
        assert!(meets(&g, [50, 70, 60, 80]));
        assert!(meets(&[], [50, 0, 90, 30]));
    }

    #[test]
    fn inconsistent_or_folded_triangles_have_no_outline() {
        // The same edge used the same way round twice (one triangle flipped).
        assert!(loops(&[0, 1, 2, 0, 2, 3, 0, 3, 2]).is_none());
        // A degenerate triangle.
        assert!(loops(&[0, 0, 1]).is_none());
    }
}

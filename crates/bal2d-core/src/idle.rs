//! Baked idle playback. The compiler plays each layer's idle loop and stores it as GPU calls
//! (`bal2d_pack::IdleBake`): per frame, which triangles of which attachment go together and the
//! affine that draws them, the frames ten or so a second. While nothing but the idle loop
//! plays, the lobby draws those calls (matrices interpolated between frames) instead of
//! animating, skinning and grouping: the watch's CPU then only writes paths and calls the
//! driver.

use alloc::vec::Vec;

use bal2d_pack::{IdleBake, IdleCall, IdleCursor, IdleShape};
use bal2d_render::{path_coord, Call, PATH_CLOSE, PATH_LINE, PATH_MOVE};
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
    /// Per shape: the distinct vertices its triangles use (`verts[vert_at[s]..vert_at[s + 1]]`).
    verts: Vec<u16>,
    vert_at: Vec<u32>,
    path: Vec<i16>,
    /// Per vertex of the attachment being drawn: summed positions (path units) and count, then
    /// the averaged position.
    acc: Vec<[f32; 3]>,
    fixed: Vec<[i16; 2]>,
    mats: Vec<[f32; 6]>,
}

impl IdleLayer {
    /// The bake for a layer of `data`, when every shape names a drawable attachment of a slot
    /// and triangles it has.
    pub fn new(bake: IdleBake, data: &SkeletonData) -> Option<IdleLayer> {
        let mut geo = Vec::with_capacity(bake.shapes.len());
        let (mut all, mut vert_at) = (Vec::new(), Vec::with_capacity(bake.shapes.len() + 1));
        let mut used: Vec<u16> = Vec::new();
        vert_at.push(0u32);
        for s in &bake.shapes {
            let slot = data.slots.get(s.slot as usize)?;
            let (page, tris, verts) = geometry(data, s.attachment)?;
            if s.tris.iter().any(|&t| (t as usize + 1) * 3 > tris.len()) || tris.iter().any(|&v| v as usize >= verts) {
                return None;
            }
            geo.push((page, slot.blend));
            used.clear();
            for &t in &s.tris {
                used.extend_from_slice(&tris[t as usize * 3..t as usize * 3 + 3]);
            }
            used.sort_unstable();
            used.dedup();
            all.extend_from_slice(&used);
            vert_at.push(all.len() as u32);
        }
        let n = bake.shapes.len();
        Some(IdleLayer {
            cursor: IdleCursor::new(n),
            frame: None,
            cur: Vec::new(),
            next: Vec::new(),
            next_of: alloc::vec![u16::MAX; n],
            geo,
            verts: all,
            vert_at,
            path: Vec::new(),
            acc: Vec::new(),
            fixed: Vec::new(),
            mats: Vec::new(),
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
    pub fn draw(&mut self, data: &SkeletonData, tau: f32, origin: (f32, f32), size: (f32, f32), page_size: &dyn Fn(u32) -> Option<(f32, f32)>, emit: &mut dyn FnMut(&Call)) -> u32 {
        let b = &self.bake;
        let pos = tau * b.rate;
        let f = (pos as i32).clamp(0, b.frames as i32 - 1) as u16;
        let alpha = (pos - f as f32).clamp(0.0, 1.0);
        if !self.seek(f) {
            return 0;
        }
        let (lx, ly) = (path_coord(size.0), path_coord(size.1));
        let k = bal2d_render::PATH_SCALE;
        let mut triangles = 0;
        let mut i = 0;
        while i < self.cur.len() {
            // A run of calls of one attachment (they are consecutive: one batch).
            let att = self.bake.shapes.get(self.cur[i].shape as usize).map(|s| (s.slot, s.attachment));
            let mut j = i + 1;
            while j < self.cur.len() && self.bake.shapes.get(self.cur[j].shape as usize).map(|s| (s.slot, s.attachment)) == att {
                j += 1;
            }
            let run = i..j;
            i = j;
            let Some((_, att)) = att else { continue };
            let Some((page, tris, verts)) = geometry(data, att) else { continue };
            let Some((pw, ph)) = page_size(page) else { continue };
            let uvs = uvs(data, att);
            // Each call's texel -> target affine, and the vertices' summed positions.
            self.mats.clear();
            if self.acc.len() < verts {
                self.acc.resize(verts, [0.0; 3]);
            }
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
                for &v in vs {
                    let (x, y) = (uvs[v as usize * 2], uvs[v as usize * 2 + 1]);
                    let a = &mut self.acc[v as usize];
                    a[0] += u[0] * x + u[1] * y + u[2];
                    a[1] += u[3] * x + u[4] * y + u[5];
                    a[2] += 1.0;
                }
                self.mats.push(m);
            }
            // Averaged once per vertex (most are used by one call: no division then).
            if self.fixed.len() < verts {
                self.fixed.resize(verts, [0; 2]);
            }
            for (a, f) in self.acc[..verts].iter_mut().zip(&mut self.fixed) {
                if a[2] > 0.0 {
                    let w = if a[2] == 1.0 { 1.0 } else { 1.0 / a[2] };
                    *f = [clamp16(a[0] * w), clamp16(a[1] * w)];
                    *a = [0.0; 3];
                }
            }
            for (c, m) in self.cur[run].iter().zip(&self.mats) {
                let shape = &self.bake.shapes[c.shape as usize];
                let path = &mut self.path;
                path.clear();
                path.reserve(shape.tris.len() * 10 + 2);
                let (mut lo, mut hi) = ([i16::MAX; 2], [i16::MIN; 2]);
                for &v in &self.verts[self.vert_at[c.shape as usize] as usize..self.vert_at[c.shape as usize + 1] as usize] {
                    let q = self.fixed[v as usize];
                    lo = [lo[0].min(q[0]), lo[1].min(q[1])];
                    hi = [hi[0].max(q[0]), hi[1].max(q[1])];
                }
                if hi[0] < 0 || hi[1] < 0 || lo[0] > lx || lo[1] > ly {
                    continue;
                }
                for &t in &shape.tris {
                    let t = t as usize * 3;
                    let (p, q, r) = (self.fixed[tris[t] as usize], self.fixed[tris[t + 1] as usize], self.fixed[tris[t + 2] as usize]);
                    path.extend_from_slice(&[PATH_MOVE as i16, p[0], p[1], PATH_LINE as i16, q[0], q[1], PATH_LINE as i16, r[0], r[1], PATH_CLOSE as i16]);
                }
                if hi[0] < 0 || hi[1] < 0 || lo[0] > lx || lo[1] > ly || path.is_empty() {
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
                emit(&Call { page, blend, color, path, bounds: [lo[0], lo[1], hi[0], hi[1]], matrix: *m, rect: None });
            }
        }
        triangles
    }
}

fn clamp16(v: f32) -> i16 {
    v.clamp(-32768.0, 32767.0) as i16
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

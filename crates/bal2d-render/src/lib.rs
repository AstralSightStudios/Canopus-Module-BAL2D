//! Draw list generation: every visible region / mesh attachment becomes one batch of
//! textured triangles with a tint and a blend mode, in draw order.
#![no_std]

extern crate alloc;
#[cfg(feature = "outline")]
extern crate std;

use alloc::vec::Vec;

use bal2d_spine::skeleton::{compute_region_world, compute_vertex_world};
use bal2d_spine::*;

pub const QUAD_INDICES: [u16; 6] = [0, 1, 2, 2, 3, 0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Batch {
    pub slot: u16,
    pub attachment: u32,
    pub blend: BlendMode,
    pub page: u32,
    /// Straight (non-premultiplied) tint: skeleton * slot * attachment color.
    pub color: [f32; 4],
    /// Two-color tint dark color, when the slot has one.
    pub dark: Option<[f32; 3]>,
    pub first_vertex: u32,
    pub vertex_count: u32,
    pub first_index: u32,
    pub index_count: u32,
}

#[derive(Clone, Debug, Default)]
pub struct DrawList {
    /// World positions, x, y per vertex.
    pub positions: Vec<f32>,
    /// Page UVs, u, v per vertex.
    pub uvs: Vec<f32>,
    /// Indices relative to the batch's first vertex.
    pub indices: Vec<u16>,
    pub batches: Vec<Batch>,
    /// Clipping attachments applied while building.
    pub clips: u32,
    /// Scratch for clipping: the clip polygon's triangles, the batch's unclipped vertices and the
    /// polygon being clipped (x, y, u, v).
    clip_tris: Vec<[[f32; 2]; 3]>,
    clip_src: Vec<f32>,
    clip_src_uv: Vec<f32>,
    clip_poly: Vec<[f32; 4]>,
    clip_next: Vec<[f32; 4]>,
    clip_index: Vec<u16>,
}

impl DrawList {
    pub fn clear(&mut self) {
        self.positions.clear();
        self.uvs.clear();
        self.indices.clear();
        self.batches.clear();
        self.clips = 0;
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Axis-aligned bounds of all batch vertices (min x, min y, max x, max y).
    pub fn bounds(&self) -> Option<[f32; 4]> {
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in self.positions.chunks_exact(2) {
            b[0] = b[0].min(p[0]);
            b[1] = b[1].min(p[1]);
            b[2] = b[2].max(p[0]);
            b[3] = b[3].max(p[1]);
        }
        (b[0] <= b[2]).then_some(b)
    }
}

/// Builds the draw list for the skeleton's current world pose.
pub fn build(data: &SkeletonData, sk: &Skeleton, out: &mut DrawList) {
    build_masked(data, sk, out, None);
}

/// Like [`build`], leaving out the slots `skip` marks (baked into the background image).
pub fn build_masked(data: &SkeletonData, sk: &Skeleton, out: &mut DrawList, skip: Option<&[bool]>) {
    out.clear();
    // The active clipping attachment's end slot.
    let mut clip_end: Option<u16> = None;
    for &si in &sk.draw_order {
        build_slot(data, sk, out, skip, si, &mut clip_end);
        if clip_end == Some(si) {
            clip_end = None;
        }
    }
}

/// One slot of [`build_masked`]: its batch, clipped while a clipping attachment is active.
fn build_slot(data: &SkeletonData, sk: &Skeleton, out: &mut DrawList, skip: Option<&[bool]>, si: u16, clip_end: &mut Option<u16>) {
    if skip.is_some_and(|m| m.get(si as usize).copied().unwrap_or(false)) {
        return;
    }
    let slot = &sk.slots[si as usize];
    if !sk.bones[slot.bone as usize].active {
        return;
    }
    let Some(id) = slot.attachment else { return };
    let att = &data.attachments[id as usize];
    let (att_color, page, uvs, tri): (Color, u32, &[f32], &[u16]) = match &att.kind {
        AttachmentKind::Region(r) => {
            let Some(region) = r.region else { return };
            (r.color, region.page, &r.uvs[..], &QUAD_INDICES[..])
        }
        AttachmentKind::Mesh(m) => {
            let Some(region) = m.region else { return };
            (m.color, region.page, &m.uvs[..], &m.triangles[..])
        }
        AttachmentKind::Clipping(c) => {
            #[cfg(feature = "outline")]
            if std::env::var("BAL2D_NO_CLIP").is_ok() {
                return;
            }
            start_clip(sk, slot, c, out);
            *clip_end = (!out.clip_tris.is_empty()).then_some(c.end_slot);
            out.clips += 1;
            return;
        }
        _ => return,
    };
    let c = Color {
        r: sk.color.r * slot.color.r * att_color.r,
        g: sk.color.g * slot.color.g * att_color.g,
        b: sk.color.b * slot.color.b * att_color.b,
        a: sk.color.a * slot.color.a * att_color.a,
    };
    if c.a <= 0.0 {
        return;
    }
    let first_vertex = (out.positions.len() / 2) as u32;
    let first_index = out.indices.len() as u32;
    let mut vertex_count;
    match &att.kind {
        AttachmentKind::Region(r) => {
            let base = out.positions.len();
            out.positions.resize(base + 8, 0.0);
            compute_region_world(&sk.bones[slot.bone as usize], &r.offset, &mut out.positions[base..], 0, 2);
            vertex_count = 4;
        }
        AttachmentKind::Mesh(m) => {
            let n = m.vertices.world_len as usize;
            let base = out.positions.len();
            out.positions.resize(base + n, 0.0);
            compute_vertex_world(&sk.bones, slot.bone as usize, &slot.deform, &m.vertices, 0, n, &mut out.positions[base..], 0, 2);
            vertex_count = (n / 2) as u32;
        }
        _ => unreachable!(),
    }
    out.uvs.extend_from_slice(&uvs[..vertex_count as usize * 2]);
    let mut index_count = tri.len() as u32;
    if clip_end.is_some() {
        index_count = clip_batch(out, first_vertex as usize, vertex_count as usize, tri);
        vertex_count = (out.positions.len() / 2) as u32 - first_vertex;
        if index_count == 0 {
            return;
        }
    } else {
        out.indices.extend_from_slice(tri);
    }
    out.batches.push(Batch {
        slot: si,
        attachment: id,
        blend: data.slots[si as usize].blend,
        page,
        color: [c.r, c.g, c.b, c.a],
        dark: slot.dark.map(|d| [d.r, d.g, d.b]),
        first_vertex,
        vertex_count,
        first_index,
        index_count,
    });
}

/// The clipping polygon in world space, cut into triangles (ear clipping; each is convex).
fn start_clip(sk: &Skeleton, slot: &bal2d_spine::Slot, c: &ClippingAttachment, out: &mut DrawList) {
    out.clip_tris.clear();
    let n = c.vertices.world_len as usize;
    if n < 6 {
        return;
    }
    let mut w = core::mem::take(&mut out.clip_src);
    w.clear();
    w.resize(n, 0.0);
    compute_vertex_world(&sk.bones, slot.bone as usize, &slot.deform, &c.vertices, 0, n, &mut w, 0, 2);
    let mut poly: Vec<[f32; 2]> = w.chunks_exact(2).map(|p| [p[0], p[1]]).collect();
    out.clip_src = w;
    // Counter-clockwise.
    let area: f32 = (0..poly.len()).map(|i| {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        a[0] * b[1] - b[0] * a[1]
    }).sum();
    if area < 0.0 {
        poly.reverse();
    }
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut guard = poly.len() * poly.len();
    while poly.len() > 3 && guard > 0 {
        guard -= 1;
        let k = poly.len();
        let ear = (0..k).find(|&i| {
            let (a, b, c) = (poly[(i + k - 1) % k], poly[i], poly[(i + 1) % k]);
            if cross(a, b, c) <= 0.0 {
                return false;
            }
            // No other vertex inside the ear.
            (0..k).all(|j| {
                if j == i || j == (i + k - 1) % k || j == (i + 1) % k {
                    return true;
                }
                let p = poly[j];
                !(cross(a, b, p) > 0.0 && cross(b, c, p) > 0.0 && cross(c, a, p) > 0.0)
            })
        });
        let Some(i) = ear else { break };
        out.clip_tris.push([poly[(i + k - 1) % k], poly[i], poly[(i + 1) % k]]);
        poly.remove(i);
    }
    if poly.len() == 3 && cross(poly[0], poly[1], poly[2]) > 0.0 {
        out.clip_tris.push([poly[0], poly[1], poly[2]]);
    }
}

/// Replaces the batch's vertices (from `first`) by its triangles clipped to the clip triangles
/// (Sutherland-Hodgman per clip triangle, fans out). Returns the new index count.
fn clip_batch(out: &mut DrawList, first: usize, count: usize, tri: &[u16]) -> u32 {
    let mut src = core::mem::take(&mut out.clip_src);
    let mut src_uv = core::mem::take(&mut out.clip_src_uv);
    src.clear();
    src.extend_from_slice(&out.positions[first * 2..(first + count) * 2]);
    src_uv.clear();
    src_uv.extend_from_slice(&out.uvs[first * 2..(first + count) * 2]);
    out.positions.truncate(first * 2);
    out.uvs.truncate(first * 2);
    let mut poly = core::mem::take(&mut out.clip_poly);
    let mut next = core::mem::take(&mut out.clip_next);
    let mut index = core::mem::take(&mut out.clip_index);
    index.clear();
    let mut produced: usize = 0;
    for t in tri.chunks_exact(3) {
        for ct in &out.clip_tris {
            poly.clear();
            for &v in t {
                let v = v as usize;
                poly.push([src[v * 2], src[v * 2 + 1], src_uv[v * 2], src_uv[v * 2 + 1]]);
            }
            for e in 0..3 {
                let (a, b) = (ct[e], ct[(e + 1) % 3]);
                let side = |p: &[f32; 4]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
                next.clear();
                for i in 0..poly.len() {
                    let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
                    let (dp, dq) = (side(&p), side(&q));
                    if dp >= 0.0 {
                        next.push(p);
                    }
                    if (dp >= 0.0) != (dq >= 0.0) {
                        let k = dp / (dp - dq);
                        next.push([p[0] + (q[0] - p[0]) * k, p[1] + (q[1] - p[1]) * k, p[2] + (q[2] - p[2]) * k, p[3] + (q[3] - p[3]) * k]);
                    }
                }
                core::mem::swap(&mut poly, &mut next);
                if poly.len() < 3 {
                    break;
                }
            }
            if poly.len() < 3 || produced + poly.len() > u16::MAX as usize {
                continue;
            }
            let base = produced as u16;
            for p in &poly {
                out.positions.extend_from_slice(&[p[0], p[1]]);
                out.uvs.extend_from_slice(&[p[2], p[3]]);
            }
            for k in 1..poly.len() as u16 - 1 {
                index.extend_from_slice(&[base, base + k, base + k + 1]);
            }
            produced += poly.len();
        }
    }
    out.indices.extend_from_slice(&index);
    let n = index.len() as u32;
    out.clip_src = src;
    out.clip_src_uv = src_uv;
    out.clip_poly = poly;
    out.clip_next = next;
    out.clip_index = index;
    n
}

/// A run of a batch's triangles drawn with one texel -> screen affine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cluster {
    /// Triangle indices into the batch (first triangle, count), in `order`.
    pub start: u32,
    pub count: u32,
    /// Texel -> screen: x' = m[0] tx + m[1] ty + m[2], y' = m[3] tx + m[4] ty + m[5].
    pub m: [f32; 6],
}

/// Screen-space triangle with its texel corners and the ids of its vertices (shared corners of
/// neighbouring triangles have the same id, which [`outline`] relies on).
#[derive(Clone, Copy, Debug, Default)]
pub struct Tri {
    pub s: [[f32; 2]; 3],
    pub t: [[f32; 2]; 3],
    pub v: [u32; 3],
}

impl Tri {
    /// The affine mapping this triangle's texels onto its screen corners.
    pub fn affine(&self) -> Option<[f32; 6]> {
        let (t, s) = (&self.t, &self.s);
        let (a1, b1) = (t[1][0] - t[0][0], t[1][1] - t[0][1]);
        let (a2, b2) = (t[2][0] - t[0][0], t[2][1] - t[0][1]);
        let det = a1 * b2 - a2 * b1;
        if det.abs() < 1e-6 {
            return None;
        }
        let inv = 1.0 / det;
        let (i00, i01, i10, i11) = (b2 * inv, -a2 * inv, -b1 * inv, a1 * inv);
        let (d1x, d1y, d2x, d2y) = (s[1][0] - s[0][0], s[1][1] - s[0][1], s[2][0] - s[0][0], s[2][1] - s[0][1]);
        let m00 = d1x * i00 + d2x * i10;
        let m01 = d1x * i01 + d2x * i11;
        let m10 = d1y * i00 + d2y * i10;
        let m11 = d1y * i01 + d2y * i11;
        Some([m00, m01, s[0][0] - m00 * t[0][0] - m01 * t[0][1], m10, m11, s[0][1] - m10 * t[0][0] - m11 * t[0][1]])
    }

    /// Whether `m` draws this triangle within `tol` pixels: [`Tri::error`] `<= tol`, giving up at
    /// the first corner that is off (most candidates are rejected, usually at the first one).
    #[inline]
    pub fn fits(&self, m: &[f32; 6], tol: f32) -> bool {
        for k in 0..3 {
            let (tx, ty) = (self.t[k][0], self.t[k][1]);
            let dx = m[0] * tx + m[1] * ty + m[2] - self.s[k][0];
            let dy = m[3] * tx + m[4] * ty + m[5] - self.s[k][1];
            if !(dx.abs() <= tol && dy.abs() <= tol) {
                return false;
            }
        }
        true
    }

    /// Largest screen-space error of drawing this triangle with `m`.
    pub fn error(&self, m: &[f32; 6]) -> f32 {
        let mut e: f32 = 0.0;
        for k in 0..3 {
            let (tx, ty) = (self.t[k][0], self.t[k][1]);
            let dx = m[0] * tx + m[1] * ty + m[2] - self.s[k][0];
            let dy = m[3] * tx + m[4] * ty + m[5] - self.s[k][1];
            e = e.max(dx.abs()).max(dy.abs());
        }
        e
    }
}

/// The texel rectangle (x, y, width, height, in whole texels) that two triangles cover exactly:
/// the halves of one axis-aligned rectangle split along a diagonal, as a region attachment or a
/// one-quad mesh draws. Such a group can be drawn as an image blit instead of a path.
pub fn texel_rect(tris: &[Tri]) -> Option<[i32; 4]> {
    let [a, b] = tris else { return None };
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in a.t.iter().chain(&b.t) {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    let whole = |v: f32| {
        let r = if v >= 0.0 { (v + 0.5) as i32 } else { (v - 0.5) as i32 };
        ((v - r as f32).abs() < 0.01).then_some(r)
    };
    let (x0, y0, x1, y1) = (whole(lo[0])?, whole(lo[1])?, whole(hi[0])?, whole(hi[1])?);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    // Every corner is a rectangle corner (index: bit 0 right, bit 1 bottom); each triangle uses
    // three distinct ones and the corners they leave out are opposite (they share the diagonal).
    let corner = |p: &[f32; 2]| -> Option<u8> {
        let (x, y) = (whole(p[0])?, whole(p[1])?);
        let cx = if x == x0 { 0 } else if x == x1 { 1 } else { return None };
        let cy = if y == y0 { 0 } else if y == y1 { 2 } else { return None };
        Some(cx | cy)
    };
    let missing = |t: &Tri| -> Option<u8> {
        let mut seen = 0u8;
        for p in &t.t {
            seen |= 1 << corner(p)?;
        }
        (seen.count_ones() == 3).then(|| (!seen & 0xF).trailing_zeros() as u8)
    };
    let (ma, mb) = (missing(a)?, missing(b)?);
    (ma ^ mb == 3).then_some([x0, y0, x1 - x0, y1 - y0])
}

/// The corners of two triangles over four vertices in order round the quad they make: the two
/// vertices both use (its diagonal) at 0 and 2, the others at 1 and 3. `None` unless the
/// triangles share exactly one edge.
pub fn quad_order(idx: &[u16]) -> Option<[usize; 4]> {
    let (a, b) = (idx.get(..3)?, idx.get(3..6)?);
    // The distinct vertices both use: exactly two.
    let (mut shared, mut n) = ([0u16; 3], 0);
    for (i, v) in a.iter().enumerate() {
        if b.contains(v) && !a[..i].contains(v) {
            shared[n] = *v;
            n += 1;
        }
    }
    let only_a = *a.iter().find(|v| !b.contains(v))?;
    let only_b = *b.iter().find(|v| !a.contains(v))?;
    let order = [shared[0], only_a, shared[1], only_b].map(|v| v as usize);
    (n == 2 && order.iter().all(|&v| v < 4) && only_a != only_b).then_some(order)
}

/// The blit for a quad drawn as triangles 0-1-2 and 2-3-0 ([`QUAD_INDICES`]): its texel
/// rectangle and texel -> target affine, when the corners go round one axis-aligned rectangle of
/// whole texels (0 and 2 opposite) and one affine places all four within [`BLIT_ERROR`].
pub fn quad_blit(screen: &[[f32; 2]], texel: &[[f32; 2]]) -> Option<([i32; 4], [f32; 6])> {
    let (s, t) = (screen.get(..4)?, texel.get(..4)?);
    let whole = |v: f32| {
        let r = if v >= 0.0 { (v + 0.5) as i32 } else { (v - 0.5) as i32 };
        ((v - r as f32).abs() < 0.01).then_some(r)
    };
    let (u0, v0, u2, v2) = (whole(t[0][0])?, whole(t[0][1])?, whole(t[2][0])?, whole(t[2][1])?);
    let (u1, v1, u3, v3) = (whole(t[1][0])?, whole(t[1][1])?, whole(t[3][0])?, whole(t[3][1])?);
    // 0 and 2 opposite, 1 and 3 opposite, all four on the same two columns and rows.
    let cols = (u0 != u2) && ((u1 == u0 && u3 == u2) || (u1 == u2 && u3 == u0));
    let rows = (v0 != v2) && ((v1 == v0 && v3 == v2) || (v1 == v2 && v3 == v0));
    if !(cols && rows) || (u1 == u0) == (v1 == v0) {
        return None;
    }
    let tri = Tri { s: [s[0], s[1], s[2]], t: [t[0], t[1], t[2]], v: [0, 1, 2] };
    let m = tri.affine()?;
    let last = Tri { s: [s[3], s[3], s[3]], t: [t[3], t[3], t[3]], v: [3, 3, 3] };
    if !(tri.error(&m) <= BLIT_ERROR && last.error(&m) <= BLIT_ERROR) {
        return None;
    }
    let (x0, y0) = (u0.min(u2), v0.min(v2));
    Some(([x0, y0, (u0 - u2).abs(), (v0 - v2).abs()], m))
}

/// Least-squares refits per group the watch uses (`Paths::new`) and the previews follow.
pub const REFITS: u8 = 0;

/// [`cluster`] as [`Paths`] groups (the same code), with `refits` least-squares refits per
/// group: what the watch draws, for previews.
pub fn cluster_refit(tris: &[Tri], tol: f32, refits: u8, order: &mut Vec<u32>, out: &mut Vec<Cluster>) {
    let mut p = Paths { tolerance: tol, refits, ..Default::default() };
    p.tris.extend_from_slice(tris);
    p.group();
    order.clear();
    order.extend_from_slice(&p.order);
    out.clear();
    out.extend(p.clusters.iter().map(|c| Cluster { start: c.start, count: c.count, m: c.m }));
}

/// Groups smaller than this are not refitted (little to gain for a whole extra scan).
pub const REFIT_MIN: usize = 3;

/// The least-squares texel -> screen affine over the corners of `members` (texels centred on
/// their mean, in f32: the watch has no double-precision FPU). `None` when the corners are
/// collinear in texel space.
pub fn fit_affine(tris: &[Tri], members: &[u32]) -> Option<[f32; 6]> {
    let n = members.len() as f32 * 3.0;
    let (mut mu, mut mv) = (0.0f32, 0.0f32);
    for &i in members {
        let t = &tris[i as usize].t;
        mu += t[0][0] + t[1][0] + t[2][0];
        mv += t[0][1] + t[1][1] + t[2][1];
    }
    mu /= n;
    mv /= n;
    let (mut suu, mut suv, mut svv) = (0.0f32, 0.0f32, 0.0f32);
    let (mut sux, mut svx, mut suy, mut svy, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for &i in members {
        let tri = &tris[i as usize];
        for k in 0..3 {
            let (u, v) = (tri.t[k][0] - mu, tri.t[k][1] - mv);
            let (x, y) = (tri.s[k][0], tri.s[k][1]);
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
    if !(det > 1e-6 * suu * svv) {
        return None;
    }
    let inv = 1.0 / det;
    let a = (sux * svv - svx * suv) * inv;
    let b = (svx * suu - sux * suv) * inv;
    let d = (suy * svv - svy * suv) * inv;
    let e = (svy * suu - suy * suv) * inv;
    Some([a, b, sx / n - a * mu - b * mv, d, e, sy / n - d * mu - e * mv])
}

/// Greedily groups triangles that one affine draws within `tol` screen pixels. A cluster's
/// affine is its first triangle's; a triangle joins the current cluster if that affine places
/// its corners within tolerance, else starts the next one. `tris` is the batch's visible,
/// non-degenerate triangles; `order` receives their indices grouped by cluster.
pub fn cluster(tris: &[Tri], tol: f32, order: &mut Vec<u32>, out: &mut Vec<Cluster>) {
    cluster_with(tris, tol, order, out, &mut Vec::new(), &mut Vec::new());
}

/// [`cluster`] with caller-owned scratch lists (no allocation once they are big enough).
pub fn cluster_with(tris: &[Tri], tol: f32, order: &mut Vec<u32>, out: &mut Vec<Cluster>, pending: &mut Vec<u32>, rest: &mut Vec<u32>) {
    order.clear();
    out.clear();
    pending.clear();
    pending.extend(0..tris.len() as u32);
    let mut head = 0;
    while head < pending.len() {
        let first = pending[head];
        let Some(m) = tris[first as usize].affine() else {
            head += 1;
            continue;
        };
        let start = order.len() as u32;
        rest.clear();
        // The first one always joins (its own affine places it exactly, up to rounding).
        order.push(first);
        for &i in &pending[head + 1..] {
            if tris[i as usize].fits(&m, tol) {
                order.push(i);
            } else {
                rest.push(i);
            }
        }
        out.push(Cluster { start, count: order.len() as u32 - start, m });
        core::mem::swap(pending, rest);
        head = 0;
    }
}

/// Largest corner error (target pixels) of a group drawn as a blit: its edges come from the
/// affine, not from the mesh (a path's edges are the mesh's own).
pub const BLIT_ERROR: f32 = 0.25;

/// Path coordinates are in 1/`PATH_SCALE` target pixels: VGLite S16 paths drawn through a
/// 1/`PATH_SCALE` path matrix. Half the size of FP32 paths, 1/8 pixel precision, +-4095 pixels
/// of range (farther vertices are clamped).
pub const PATH_SCALE: f32 = 8.0;

/// A target coordinate in path units (truncated, saturating).
#[inline]
pub fn path_coord(v: f32) -> i16 {
    (v * PATH_SCALE) as i16
}

/// One GPU call: a path filled with an image through an affine.
pub struct Call<'a> {
    pub page: u32,
    pub blend: BlendMode,
    /// Straight tint, as in [`Batch::color`].
    pub color: [f32; 4],
    /// VGLite S16 path: 16-bit opcodes, each followed by its coordinates in path units
    /// ([`PATH_SCALE`]); END-terminated, a whole number of 32-bit words.
    pub path: &'a [i16],
    /// Path bounds (min x, min y, max x, max y) in path units.
    pub bounds: [i16; 4],
    /// Texel -> target pixel affine (`Cluster::m`).
    pub matrix: [f32; 6],
    /// With [`Paths::blits`]: the group is this texel rectangle (x, y, width, height) mapped by
    /// `matrix`, to be drawn as an image blit (`path` is then empty).
    pub rect: Option<[i32; 4]>,
}

/// Turns draw lists into GPU calls: the visible triangles of each batch, grouped by affine
/// ([`cluster_with`]) into one path per call. (Re-using last frame's groups was tried: cheaper to
/// build, but groups split as meshes deform and cost 15-50% more calls.) Everything is kept
/// across frames: after [`Paths::reserve`] drawing does not allocate.
#[derive(Default)]
pub struct Paths {
    /// The batch's vertices in target pixels, path units and texels.
    screen: Vec<[f32; 2]>,
    fixed: Vec<[i16; 2]>,
    texel: Vec<[f32; 2]>,
    tris: Vec<Tri>,
    order: Vec<u32>,
    clusters: Vec<Cluster>,
    pending: Vec<u32>,
    rest: Vec<u32>,
    alt: Vec<u32>,
    alt_rest: Vec<u32>,
    #[cfg(feature = "outline")]
    outline: Outline,
    data: Vec<i16>,
    /// Screen-space tolerance of the affine clustering (pixels).
    pub tolerance: f32,
    /// Paths as cluster outlines instead of one subpath per triangle (feature `outline`).
    pub outline_paths: bool,
    /// Groups that are one texel rectangle drawn rigidly come as [`Call::rect`] (image blits:
    /// no path to build, upload or rasterize as edges).
    pub blits: bool,
    /// Calls given as rectangles since [`Paths::reset_stats`].
    pub rects: u32,
    /// Least-squares refits tried per group (0: the seed triangle's affine only).
    pub refits: u8,
    /// Visible triangles and calls since [`Paths::reset_stats`].
    pub triangles: u32,
    pub calls: u32,
}

impl Paths {
    pub fn new() -> Paths {
        Paths { tolerance: 0.75, refits: REFITS, ..Default::default() }
    }

    /// Room for batches of up to `max_triangles` triangles and `max_vertices` vertices.
    pub fn reserve(&mut self, max_triangles: usize) -> bool {
        let n = max_triangles;
        fn grow<T>(v: &mut Vec<T>, n: usize) -> bool {
            v.try_reserve_exact(n.saturating_sub(v.capacity())).is_ok()
        }
        grow(&mut self.tris, n)

            && grow(&mut self.screen, n * 3)
            && grow(&mut self.fixed, n * 3)
            && grow(&mut self.texel, n * 3)
            && grow(&mut self.order, n)
            && grow(&mut self.clusters, n)
            && grow(&mut self.pending, n)
            && grow(&mut self.rest, n)
            && grow(&mut self.alt, n)
            && grow(&mut self.alt_rest, n)
            && grow(&mut self.data, n * 10 + 2)
            && self.reserve_outline(n)
    }

    #[cfg(feature = "outline")]
    fn reserve_outline(&mut self, n: usize) -> bool {
        self.outline.reserve(n)
    }

    #[cfg(not(feature = "outline"))]
    fn reserve_outline(&mut self, _n: usize) -> bool {
        true
    }

    pub fn reset_stats(&mut self) {
        self.triangles = 0;
        self.calls = 0;
        self.rects = 0;
    }

    /// Clusters the batch's visible triangles into `order` / `clusters` (as [`cluster_with`]).
    fn group(&mut self) {
        self.pending.clear();
        self.pending.extend(0..self.tris.len() as u32);
        let tol = self.tolerance;
        self.order.clear();
        self.clusters.clear();
        self.order.reserve(self.pending.len());
        self.rest.reserve(self.pending.len());
        let mut head = 0;
        while head < self.pending.len() {
            let first = self.pending[head];
            let Some(m) = self.tris[first as usize].affine() else {
                head += 1;
                continue;
            };
            let start = self.order.len();
            self.order.push(first);
            // Branch-free partition of the rest into members and the next round's candidates:
            // every candidate is written to both lists, only one of them advances.
            let candidates = &self.pending[head + 1..];
            let (mut taken, mut left) = (self.order.len(), 0usize);
            let (order, rest) = (self.order.as_mut_ptr(), self.rest.as_mut_ptr());
            // A slice (pointer and length in registers), not the Vec: its header would be reloaded
            // after every store through the raw pointers.
            let tris: &[Tri] = &self.tris;
            for &i in candidates {
                // SAFETY: `pending` only holds indices of `tris`.
                let fit = unsafe { tris.get_unchecked(i as usize) }.fits(&m, tol);
                // SAFETY: both lists have room for every candidate (reserved above; `order` holds
                // at most all pending triangles, `rest` at most the candidates).
                unsafe {
                    *order.add(taken) = i;
                    *rest.add(left) = i;
                }
                taken += fit as usize;
                left += !fit as usize;
            }
            // SAFETY: the first `taken` / `left` entries were written just above.
            unsafe {
                self.order.set_len(taken);
                self.rest.set_len(left);
            }
            let mut m = m;
            // Refits: one affine fitted to all the members' corners usually places more of this
            // round's triangles within tolerance than the seed's own does (10-25% fewer calls).
            for _ in 0..self.refits {
                if taken - start < REFIT_MIN || self.rest.is_empty() {
                    break;
                }
                let Some(m2) = fit_affine(&self.tris, &self.order[start..taken]) else { break };
                self.alt.clear();
                self.alt_rest.clear();
                for &i in self.order[start..taken].iter().chain(self.rest.iter()) {
                    // SAFETY: both lists only hold indices of `tris`.
                    if unsafe { self.tris.get_unchecked(i as usize) }.fits(&m2, tol) {
                        self.alt.push(i);
                    } else {
                        self.alt_rest.push(i);
                    }
                }
                if self.alt.len() <= taken - start {
                    break;
                }
                self.order.truncate(start);
                self.order.extend_from_slice(&self.alt);
                core::mem::swap(&mut self.rest, &mut self.alt_rest);
                taken = self.order.len();
                m = m2;
            }
            self.clusters.push(Cluster { start: start as u32, count: (taken - start) as u32, m });
            core::mem::swap(&mut self.pending, &mut self.rest);
            head = 0;
        }
    }

    /// Emits the calls for layer `layer`'s `list`. `cam` maps skeleton to target pixels:
    /// x' = (x - cx) * s + w/2, y' = h/2 - (y - cy) * s for a target of `size`; `page_size` gives
    /// each page's size in texels (`None`: skip the batch).
    pub fn draw(
        &mut self,
        layer: usize,
        list: &DrawList,
        cam: (f32, f32, f32),
        size: (f32, f32),
        page_size: impl Fn(u32) -> Option<(f32, f32)>,
        mut emit: impl FnMut(&Call),
    ) {
        self.draw_dyn(layer, list, cam, size, &page_size, &mut emit);
    }

    /// `draw`'s body, not generic: it is compiled (and optimized) in this crate whatever crate
    /// the callbacks come from (the watch builds its own crate for size).
    #[inline(never)]
    fn draw_dyn(
        &mut self,
        layer: usize,
        list: &DrawList,
        cam: (f32, f32, f32),
        size: (f32, f32),
        page_size: &dyn Fn(u32) -> Option<(f32, f32)>,
        emit: &mut dyn FnMut(&Call),
    ) {
        let (w2, h2) = (size.0 * 0.5, size.1 * 0.5);
        let (cx, cy, s) = cam;
        let _ = layer;
        let (limit_x, limit_y) = (path_coord(size.0), path_coord(size.1));
        for b in &list.batches {
            let Some((pw, ph)) = page_size(b.page) else { continue };
            // Vertices once, then triangles from them (whole slices: no per-vertex checks).
            let v0 = b.first_vertex as usize;
            let span = v0 * 2..(v0 + b.vertex_count as usize) * 2;
            let (Some(pos), Some(uv)) = (list.positions.get(span.clone()), list.uvs.get(span)) else { continue };
            let n = b.vertex_count as usize;
            let Some(idx) = list.indices.get(b.first_index as usize..(b.first_index + b.index_count) as usize) else { continue };
            // Every index is checked once here: the loops below index without checks.
            if idx.iter().any(|&i| i as usize >= n) {
                continue;
            }
            // One pass over the vertices: target pixels, path units, texels, and the batch's
            // bounds (written straight into reserved room).
            self.screen.clear();
            self.fixed.clear();
            self.texel.clear();
            self.screen.reserve(n);
            self.fixed.reserve(n);
            self.texel.reserve(n);
            let (mut lo, mut hi) = ([i16::MAX; 2], [i16::MIN; 2]);
            {
                let (sc, fx, tx) = (self.screen.spare_capacity_mut(), self.fixed.spare_capacity_mut(), self.texel.spare_capacity_mut());
                for (k, (q, t)) in pos.chunks_exact(2).zip(uv.chunks_exact(2)).enumerate() {
                    let p = [(q[0] - cx) * s + w2, h2 - (q[1] - cy) * s];
                    let f = [path_coord(p[0]), path_coord(p[1])];
                    lo = [lo[0].min(f[0]), lo[1].min(f[1])];
                    hi = [hi[0].max(f[0]), hi[1].max(f[1])];
                    // SAFETY: `k < n` (the spans hold `n` vertices) and each list has room for `n`.
                    unsafe {
                        sc.get_unchecked_mut(k).write(p);
                        fx.get_unchecked_mut(k).write(f);
                        tx.get_unchecked_mut(k).write([t[0] * pw, t[1] * ph]);
                    }
                }
            }
            // SAFETY: the first `n` entries of each were written just above.
            unsafe {
                self.screen.set_len(n);
                self.fixed.set_len(n);
                self.texel.set_len(n);
            }
            // Only the triangles that reach the target are kept. The batch's bounds decide for
            // all of them at once when it lies wholly on or wholly off the target.
            if hi[0] < 0 || hi[1] < 0 || lo[0] > limit_x || lo[1] > limit_y {
                continue;
            }
            let all_in = lo[0] >= 0 && lo[1] >= 0 && hi[0] <= limit_x && hi[1] <= limit_y;
            // A region attachment (or a one-quad mesh): one atlas rectangle drawn rigidly is a
            // blit, found from its four corners without building triangles or clustering.
            if self.blits && b.vertex_count == 4 && idx.len() == 6 {
                let quad = quad_order(idx).and_then(|o| quad_blit(&o.map(|k| self.screen[k]), &o.map(|k| self.texel[k])));
                if let Some((rect, m)) = quad {
                    self.triangles += 2;
                    self.calls += 1;
                    self.rects += 1;
                    emit(&Call { page: b.page, blend: b.blend, color: b.color, path: &[], bounds: [0; 4], matrix: m, rect: Some(rect) });
                    continue;
                }
            }
            self.tris.clear();
            self.tris.reserve(idx.len() / 3);
            let mut kept = 0;
            {
                let (screen, fixed, texel) = (&self.screen[..], &self.fixed[..], &self.texel[..]);
                let out = self.tris.spare_capacity_mut();
                for t in idx.chunks_exact(3) {
                    let (a, bb, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
                    // SAFETY: every index is below `n` (checked above), the length of the three.
                    let (qa, qb, qc) = unsafe { (*fixed.get_unchecked(a), *fixed.get_unchecked(bb), *fixed.get_unchecked(c)) };
                    let vis = all_in
                        || !(qa[0].max(qb[0]).max(qc[0]) < 0
                            || qa[1].max(qb[1]).max(qc[1]) < 0
                            || qa[0].min(qb[0]).min(qc[0]) > limit_x
                            || qa[1].min(qb[1]).min(qc[1]) > limit_y);
                    if !vis {
                        continue;
                    }
                    // SAFETY: as above; `kept` < idx.len() / 3, the room reserved.
                    unsafe {
                        out.get_unchecked_mut(kept).write(Tri {
                            s: [*screen.get_unchecked(a), *screen.get_unchecked(bb), *screen.get_unchecked(c)],
                            t: [*texel.get_unchecked(a), *texel.get_unchecked(bb), *texel.get_unchecked(c)],
                            v: [a as u32, bb as u32, c as u32],
                        });
                    }
                    kept += 1;
                }
            }
            // SAFETY: the first `kept` entries were written just above.
            unsafe { self.tris.set_len(kept) };
            if self.tris.is_empty() {
                continue;
            }
            self.group();
            self.triangles += self.order.len() as u32;
            for c in &self.clusters {
                let d = &mut self.data;
                d.clear();
                let mut lo = [i16::MAX; 2];
                let mut hi = [i16::MIN; 2];
                let members = &self.order[c.start as usize..(c.start + c.count) as usize];
                if self.blits && c.count == 2 {
                    let pair = [self.tris[members[0] as usize], self.tris[members[1] as usize]];
                    // Drawn by the affine alone, so its corners must land where the mesh has them.
                    if let Some(rect) = texel_rect(&pair).filter(|_| pair.iter().all(|t| t.error(&c.m) <= BLIT_ERROR)) {
                        self.calls += 1;
                        self.rects += 1;
                        emit(&Call { page: b.page, blend: b.blend, color: b.color, path: &[], bounds: [0; 4], matrix: c.m, rect: Some(rect) });
                        continue;
                    }
                }
                #[cfg(feature = "outline")]
                let outlined = self.outline_paths;
                #[cfg(not(feature = "outline"))]
                let outlined = false;
                if outlined {
                    #[cfg(feature = "outline")]
                    outline(&self.tris, members, &mut self.outline, |op, x, y| {
                        d.push(op as i16);
                        if op != PATH_CLOSE {
                            let q = [path_coord(x), path_coord(y)];
                            d.extend_from_slice(&q);
                            lo = [lo[0].min(q[0]), lo[1].min(q[1])];
                            hi = [hi[0].max(q[0]), hi[1].max(q[1])];
                        }
                    });
                } else {
                    const MOVE: i16 = PATH_MOVE as i16;
                    const LINE: i16 = PATH_LINE as i16;
                    const CLOSE: i16 = PATH_CLOSE as i16;
                    // Ten elements per triangle, written straight into reserved room.
                    let len = members.len() * 10;
                    d.reserve(len + 2);
                    let (tris, fixed) = (&self.tris, &self.fixed);
                    for (out, &ti) in d.spare_capacity_mut()[..len].chunks_exact_mut(10).zip(members) {
                        // SAFETY: cluster members index `tris` (they come from `group`), and every
                        // triangle's vertices indexed `fixed` (checked) when it was made.
                        let v = unsafe { tris.get_unchecked(ti as usize) }.v;
                        let (p, q, r) = unsafe { (*fixed.get_unchecked(v[0] as usize), *fixed.get_unchecked(v[1] as usize), *fixed.get_unchecked(v[2] as usize)) };
                        for (o, e) in out.iter_mut().zip([MOVE, p[0], p[1], LINE, q[0], q[1], LINE, r[0], r[1], CLOSE]) {
                            o.write(e);
                        }
                        lo = [lo[0].min(p[0]).min(q[0]).min(r[0]), lo[1].min(p[1]).min(q[1]).min(r[1])];
                        hi = [hi[0].max(p[0]).max(q[0]).max(r[0]), hi[1].max(p[1]).max(q[1]).max(r[1])];
                    }
                    // SAFETY: the first `len` elements were all written above.
                    unsafe { d.set_len(len) };
                }
                // END, padded to whole 32-bit words.
                d.push(0);
                if d.len() % 2 == 1 {
                    d.push(0);
                }
                self.calls += 1;
                emit(&Call { page: b.page, blend: b.blend, color: b.color, path: &self.data, bounds: [lo[0], lo[1], hi[0], hi[1]], matrix: c.m, rect: None });
            }
        }
    }
}

/// Path commands of [`outline`] (VGLite's opcodes).
pub const PATH_MOVE: u32 = 2;
pub const PATH_LINE: u32 = 4;
pub const PATH_CLOSE: u32 = 1;

/// Scratch space of [`outline`], kept across calls so drawing does not allocate.
#[cfg(feature = "outline")]
#[derive(Default)]
pub struct Outline {
    edges: Vec<(u64, u32, u32, [f32; 2])>,
    live: Vec<(u32, u32, [f32; 2])>,
    used: Vec<bool>,
}

#[cfg(feature = "outline")]
impl Outline {
    /// Room for clusters of up to `triangles` triangles.
    pub fn reserve(&mut self, triangles: usize) -> bool {
        let n = triangles * 3;
        self.edges.try_reserve_exact(n.saturating_sub(self.edges.capacity())).is_ok()
            && self.live.try_reserve_exact(n.saturating_sub(self.live.capacity())).is_ok()
            && self.used.try_reserve_exact(n.saturating_sub(self.used.capacity())).is_ok()
    }
}

#[cfg(feature = "outline")]
/// The boundary of a group of triangles as closed polygons, for one path filled with the
/// non-zero rule: every triangle is turned counter-clockwise (so overlapping ones add up instead
/// of cancelling into holes), edges two triangles share in opposite directions are dropped (that
/// leaves every winding number as it was), and the remaining edges are chained into loops. The
/// result covers exactly the pixels the triangles cover, with far fewer edges for a mesh: only
/// its border is left. `emit(op, x, y)` receives MOVE, LINE and CLOSE in order.
pub fn outline(tris: &[Tri], members: &[u32], scratch: &mut Outline, mut emit: impl FnMut(u32, f32, f32)) {
    let e = &mut scratch.edges;
    e.clear();
    for &ti in members {
        let t = &tris[ti as usize];
        let area = (t.s[1][0] - t.s[0][0]) * (t.s[2][1] - t.s[0][1]) - (t.s[2][0] - t.s[0][0]) * (t.s[1][1] - t.s[0][1]);
        let o: [usize; 3] = if area < 0.0 { [0, 2, 1] } else { [0, 1, 2] };
        for k in 0..3 {
            let (i, j) = (o[k], o[(k + 1) % 3]);
            let (a, b) = (t.v[i], t.v[j]);
            let key = ((a.min(b) as u64) << 32) | a.max(b) as u64;
            e.push((key, a, b, t.s[i]));
        }
    }
    e.sort_unstable_by_key(|x| x.0);
    let live = &mut scratch.live;
    live.clear();
    let mut i = 0;
    while i < e.len() {
        let mut j = i;
        let mut net: i32 = 0;
        while j < e.len() && e[j].0 == e[i].0 {
            net += if e[j].1 < e[j].2 { 1 } else { -1 };
            j += 1;
        }
        // Keep |net| edges of the direction that wins (a == b: degenerate, dropped).
        let mut left = net.unsigned_abs();
        for x in &e[i..j] {
            if left == 0 {
                break;
            }
            if (net > 0) == (x.1 < x.2) && x.1 != x.2 {
                live.push((x.1, x.2, x.3));
                left -= 1;
            }
        }
        i = j;
    }
    live.sort_unstable_by_key(|x| x.0);
    let used = &mut scratch.used;
    used.clear();
    used.resize(live.len(), false);
    for first in 0..live.len() {
        if used[first] {
            continue;
        }
        used[first] = true;
        let start = live[first].0;
        emit(PATH_MOVE, live[first].2[0], live[first].2[1]);
        let mut cur = live[first].1;
        let mut guard = live.len();
        while cur != start && guard > 0 {
            guard -= 1;
            let from = live.partition_point(|x| x.0 < cur);
            let mut next = None;
            for k in from..live.len() {
                if live[k].0 != cur {
                    break;
                }
                if !used[k] {
                    next = Some(k);
                    break;
                }
            }
            let Some(k) = next else { break };
            used[k] = true;
            emit(PATH_LINE, live[k].2[0], live[k].2[1]);
            cur = live[k].1;
        }
        emit(PATH_CLOSE, 0.0, 0.0);
    }
}

#[cfg(all(test, feature = "outline"))]
mod outline_tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// A jittered grid mesh; `flip` reverses some triangles' winding, `fold` overlaps a few.
    fn mesh(n: usize, seed: u32) -> Vec<Tri> {
        let mut r = seed;
        let mut rnd = || {
            r = r.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((r >> 8) & 0xffff) as f32 / 65535.0
        };
        let mut pos = vec![[0f32; 2]; (n + 1) * (n + 1)];
        for y in 0..=n {
            for x in 0..=n {
                pos[y * (n + 1) + x] = [x as f32 * 10.0 + rnd() * 3.0, y as f32 * 10.0 + rnd() * 3.0];
            }
        }
        let mut tris = Vec::new();
        for y in 0..n {
            for x in 0..n {
                let a = (y * (n + 1) + x) as u32;
                let (b, c, d) = (a + 1, a + (n + 1) as u32, a + (n + 2) as u32);
                for v in [[a, b, d], [a, d, c]] {
                    let v = if rnd() < 0.3 { [v[0], v[2], v[1]] } else { v };
                    tris.push(Tri { s: v.map(|i| pos[i as usize]), t: [[0.0; 2]; 3], v });
                }
            }
        }
        // A fold: a few vertices pulled across their neighbours.
        for _ in 0..3 {
            let i = (rnd() * pos.len() as f32) as usize % pos.len();
            pos[i][0] += 25.0;
        }
        for t in &mut tris {
            t.s = t.v.map(|i| pos[i as usize]);
        }
        tris
    }

    fn inside(t: &Tri, p: [f32; 2]) -> bool {
        let d = |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        let (d0, d1, d2) = (d(t.s[0], t.s[1]), d(t.s[1], t.s[2]), d(t.s[2], t.s[0]));
        (d0 > 0.0 && d1 > 0.0 && d2 > 0.0) || (d0 < 0.0 && d1 < 0.0 && d2 < 0.0)
    }

    fn winding(polys: &[Vec<[f32; 2]>], p: [f32; 2]) -> i32 {
        let mut w = 0;
        for poly in polys {
            for k in 0..poly.len() {
                let (a, b) = (poly[k], poly[(k + 1) % poly.len()]);
                let cross = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
                if a[1] <= p[1] {
                    if b[1] > p[1] && cross > 0.0 {
                        w += 1;
                    }
                } else if b[1] <= p[1] && cross < 0.0 {
                    w -= 1;
                }
            }
        }
        w
    }

    #[test]
    fn the_outline_covers_exactly_the_triangles() {
        for seed in 1..20 {
            let tris = mesh(12, seed);
            let members: Vec<u32> = (0..tris.len() as u32).collect();
            let mut polys: Vec<Vec<[f32; 2]>> = Vec::new();
            let mut edges = 0;
            outline(&tris, &members, &mut Outline::default(), |op, x, y| match op {
                PATH_MOVE => polys.push(vec![[x, y]]),
                PATH_LINE => {
                    polys.last_mut().unwrap().push([x, y]);
                    edges += 1;
                }
                _ => edges += 1,
            });
            assert!(edges < tris.len() * 3 / 2, "seed {seed}: {edges} edges for {} triangles", tris.len());
            let mut r = seed;
            for _ in 0..4000 {
                r = r.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let px = ((r >> 8) & 0xffff) as f32 / 65535.0 * 160.0 - 5.0;
                r = r.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let py = ((r >> 8) & 0xffff) as f32 / 65535.0 * 130.0 - 5.0;
                let p = [px + 0.0137, py + 0.0291];
                let covered = tris.iter().any(|t| inside(t, p));
                assert_eq!(covered, winding(&polys, p) != 0, "seed {seed} point {p:?}");
            }
        }
    }

    #[test]
    fn a_grid_keeps_little_more_than_its_border() {
        // 10 x 10 cells (with the generator's flips and folds).
        let tris = mesh(10, 7);
        let members: Vec<u32> = (0..tris.len() as u32).collect();
        let mut lines = 0;
        outline(&tris, &members, &mut Outline::default(), |op, _, _| {
            if op != PATH_CLOSE {
                lines += 1;
            }
        });
        // 200 triangles: 600 triangle edges; the border has 40 (folds may add a few).
        assert!(lines <= 60, "{lines}");
    }
}

#[cfg(test)]
mod clip_tests {
    use super::*;

    #[test]
    fn a_square_clipped_by_a_triangle_keeps_their_intersection() {
        let mut out = DrawList::default();
        // Unit square (two triangles), UV = position.
        out.positions.extend_from_slice(&[0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0]);
        out.uvs.extend_from_slice(&[0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0]);
        // Clip triangle covering the lower-left half (x + y <= 1) and more, counter-clockwise.
        out.clip_tris.push([[-1.0, -1.0], [2.0, -1.0], [-1.0, 2.0]]);
        let n = clip_batch(&mut out, 0, 4, &[0, 1, 2, 2, 3, 0]);
        assert!(n > 0 && n % 3 == 0);
        let mut area = 0.0f32;
        for t in out.indices.chunks_exact(3) {
            let p = |i: u16| (out.positions[i as usize * 2], out.positions[i as usize * 2 + 1]);
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            area += ((b.0 - a.0) * (c.1 - a.1) - (c.0 - a.0) * (b.1 - a.1)) * 0.5;
            for i in t {
                let (x, y) = p(*i);
                assert!(x + y <= 1.0 + 1e-5, "outside the clip: {x} {y}");
                // UVs follow positions.
                assert!((out.uvs[*i as usize * 2] - x).abs() < 1e-5 && (out.uvs[*i as usize * 2 + 1] - y).abs() < 1e-5);
            }
        }
        assert!((area.abs() - 0.5).abs() < 1e-4, "area {area}");
    }
}

#[cfg(test)]
mod rect_tests {
    use super::*;

    fn tri(t: [[f32; 2]; 3]) -> Tri {
        Tri { s: t, t, v: [0, 1, 2] }
    }

    #[test]
    fn a_quad_split_along_a_diagonal_is_its_rectangle() {
        let (a, b, c, d) = ([2.0, 3.0], [10.0, 3.0], [10.0, 7.0], [2.0, 7.0]);
        assert_eq!(texel_rect(&[tri([a, b, c]), tri([a, c, d])]), Some([2, 3, 8, 4]));
        assert_eq!(texel_rect(&[tri([b, c, d]), tri([d, a, b])]), Some([2, 3, 8, 4]));
    }

    #[test]
    fn a_rigid_quad_is_a_blit() {
        // Texels (4, 6)..(12, 10) turned a quarter and moved: x' = 100 - v, y' = 20 + u.
        let t = [[4.0, 6.0], [12.0, 6.0], [12.0, 10.0], [4.0, 10.0]];
        let s: Vec<[f32; 2]> = t.iter().map(|p| [100.0 - p[1], 20.0 + p[0]]).collect();
        let (rect, m) = quad_blit(&s, &t).unwrap();
        assert_eq!(rect, [4, 6, 8, 4]);
        for (p, q) in t.iter().zip(&s) {
            assert!((m[0] * p[0] + m[1] * p[1] + m[2] - q[0]).abs() < 1e-4);
            assert!((m[3] * p[0] + m[4] * p[1] + m[5] - q[1]).abs() < 1e-4);
        }
    }

    #[test]
    fn quads_are_put_in_order_round_their_diagonal() {
        assert_eq!(quad_order(&QUAD_INDICES), Some([0, 1, 2, 3]));
        // Diagonal 1-3: 1, 2, 3, 0.
        assert_eq!(quad_order(&[1, 2, 3, 1, 3, 0]), Some([1, 2, 3, 0]));
        assert_eq!(quad_order(&[0, 1, 2, 0, 1, 2]), None);
        assert_eq!(quad_order(&[0, 1, 2, 3, 4, 5]), None);
    }

    #[test]
    fn bent_or_scrambled_quads_are_not() {
        let t = [[4.0, 6.0], [12.0, 6.0], [12.0, 10.0], [4.0, 10.0]];
        let mut s: Vec<[f32; 2]> = t.to_vec();
        s[3][0] += 1.0; // one corner off the affine
        assert!(quad_blit(&s, &t).is_none());
        // 0 and 2 adjacent, not opposite: the triangles would overlap.
        let t2 = [[4.0, 6.0], [12.0, 10.0], [12.0, 6.0], [4.0, 10.0]];
        assert!(quad_blit(&t2, &t2).is_none());
    }

    #[test]
    fn overlapping_or_partial_pairs_are_not() {
        let (a, b, c, d) = ([2.0, 3.0], [10.0, 3.0], [10.0, 7.0], [2.0, 7.0]);
        // Both leave out corners next to each other: they overlap and miss a quarter.
        assert_eq!(texel_rect(&[tri([a, b, c]), tri([a, b, d])]), None);
        // A corner inside the rectangle.
        assert_eq!(texel_rect(&[tri([a, b, [6.0, 5.0]]), tri([a, c, d])]), None);
        // Not on whole texels.
        let e = [2.5, 3.0];
        assert_eq!(texel_rect(&[tri([e, b, c]), tri([e, c, [2.5, 7.0]])]), None);
        assert_eq!(texel_rect(&[tri([a, b, c])]), None);
    }
}

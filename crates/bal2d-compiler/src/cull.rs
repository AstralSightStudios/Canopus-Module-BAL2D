//! Fill the GPU need not do in a baked idle.
//!
//! A pixel a call draws changes nothing when the texels it samples are all zero (premultiplied:
//! no blend mode changes the target there), or when opaque pixels drawn later cover it. The
//! reference lobbies spend a fifth to a half of their fill on such pixels (Hoshino's aquarium
//! lies mostly behind her: 5.6 of 8.5 screens a frame).
//!
//! - **Empty texels** do not depend on the frame: attachments with big triangles (a background
//!   is often two) get an [`IdleMesh`] that splits each triangle evenly, and the shapes leave out
//!   the parts whose texels are all zero (with a margin of texels around).
//! - **Covered pixels** are found on the screen, where they stay put while the background
//!   behind moves: every baked frame is played, and the pixels opaque pixels drawn later cover
//!   in *every* frame (a margin in, at the start and the end of each frame's interval) make the
//!   layer's always-covered area, for the shapes drawn before those slots (one split in the
//!   draw order per layer, chosen for the most fill saved), and the layers above make another
//!   (used while they play their bakes too: a talk animates and moves the character). The watch
//!   clips the shapes worth it to the rectangles left visible ([`IdleBake::regions`]).

use bal2d_core::Lobby;
use bal2d_pack::{IdleBake, IdleCall, IdleCursor, IdleMesh, IDLE_BEFORE, IDLE_CLIP_ABOVE, IDLE_CLIP_LATER};
use bal2d_spine::BlendMode;

use crate::raster::Texture;

/// Longest on-screen edge a split triangle may keep (pixels), most splits per edge, most
/// triangles a split attachment may have.
const SPLIT_PX: f32 = 48.0;
const MAX_SPLIT: usize = 12;
const MAX_SPLIT_TRIS: usize = 2048;
/// A split attachment keeps its own geometry unless the split leaves out at least this share of
/// its texture area.
const MIN_SPLIT_GAIN: f32 = 0.15;
/// Texels looked at around a texel position for emptiness and opacity.
const EMPTY_PAD: f32 = 2.5;
const OPAQUE_PAD: f32 = 1.5;
/// Opaque cover narrowed by this many pixels (antialiased edges, interpolation), after each
/// call's triangles grow by `CRACK_PX`.
const SHRINK_PX: usize = 2;
const CRACK_PX: f32 = 0.75;
/// Visible rectangles: the most, and how much wider a band's span may make the one above it.
const MAX_RECTS: usize = 40;
const MERGE_PX: i16 = 8;
/// A shape is clipped when that saves at least this many pixels per call on average, and
/// `PX_PER_CLIP` pixels for every outline vertex clipped to every rectangle its calls meet (the
/// watch's CPU clips a vertex to a rectangle in about the time its GPU fills that many pixels;
/// measured in the calibrated timing model, clipping every shape that saved 1500 pixels cost
/// more CPU than the GPU time it saved).
const MIN_SAVED_PX: f32 = 4000.0;
const PX_PER_CLIP: f32 = 40.0;

/// What the pass did, for the report.
#[derive(Clone, Debug, Default)]
pub struct CullReport {
    /// Attachments given finer triangles, and the fill per frame in screens: before, after
    /// leaving out empty texels, after leaving out covered pixels too, and that with every
    /// layer drawn from its bake.
    pub split_attachments: usize,
    pub fill_before: f32,
    pub fill_after_empty: f32,
    pub fill_after: f32,
    pub fill_after_above: f32,
}

/// Summed-area table of a per-texel flag: counts in any texel box.
struct Sat {
    w: usize,
    h: usize,
    s: Vec<u32>,
}

impl Sat {
    fn new(w: usize, h: usize, f: impl Fn(usize, usize) -> bool) -> Sat {
        let mut s = vec![0u32; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0u32;
            for x in 0..w {
                row += f(x, y) as u32;
                s[(y + 1) * (w + 1) + x + 1] = s[y * (w + 1) + x + 1] + row;
            }
        }
        Sat { w, h, s }
    }

    /// Flags in the texels whose centres lie within `pad` texels of (x, y) (texel units, the
    /// page's top-left at 0), clamped to the page; how many texels that box holds; and whether
    /// it reached past the page (the GPU then samples the edge or nothing).
    fn count(&self, x: f32, y: f32, pad: f32) -> (u32, u32, bool) {
        let range = |c: f32, n: usize| -> (usize, usize, bool) {
            let (i0, i1) = ((c - pad - 0.5).ceil(), (c + pad - 0.5).floor());
            let out = i0 < 0.0 || i1 > (n - 1) as f32;
            let i0 = (i0.max(0.0) as usize).min(n - 1);
            let i1 = (i1.max(0.0) as usize).min(n - 1).max(i0);
            (i0, i1 + 1, out)
        };
        let (x0, x1, ox) = range(x, self.w);
        let (y0, y1, oy) = range(y, self.h);
        let at = |x: usize, y: usize| self.s[y * (self.w + 1) + x];
        let n = at(x1, y1) + at(x0, y0) - at(x0, y1) - at(x1, y0);
        (n, ((x1 - x0) * (y1 - y0)) as u32, ox || oy)
    }
}

fn inverse(m: &[f32; 6]) -> Option<[f32; 6]> {
    let det = m[0] * m[4] - m[1] * m[3];
    if !(det.abs() > 1e-9) {
        return None;
    }
    let (a, b, c, d) = (m[4] / det, -m[1] / det, -m[3] / det, m[0] / det);
    Some([a, b, -(a * m[2] + b * m[5]), c, d, -(c * m[2] + d * m[5])])
}

fn apply(m: &[f32; 6], p: [f32; 2]) -> [f32; 2] {
    [m[0] * p[0] + m[1] * p[1] + m[2], m[3] * p[0] + m[4] * p[1] + m[5]]
}

/// Pixels whose centres lie inside triangle `t`, the edges moved out by `grow` pixels, clipped
/// to `w` x `h`; `f` returns false to stop.
fn raster_tri(t: [[f32; 2]; 3], grow: f32, w: usize, h: usize, f: impl FnMut(usize, usize) -> bool) {
    raster_tri_in(t, grow, [0, 0, w, h], f)
}

/// `raster_tri` within the box `clip` (x0, y0, x1, y1; x1 and y1 exclusive).
fn raster_tri_in(t: [[f32; 2]; 3], grow: f32, clip: [usize; 4], mut f: impl FnMut(usize, usize) -> bool) {
    let area = (t[1][0] - t[0][0]) * (t[2][1] - t[0][1]) - (t[1][1] - t[0][1]) * (t[2][0] - t[0][0]);
    if area.abs() < 1e-6 {
        return;
    }
    let s = area.signum();
    let mut e = [[0f32; 3]; 3];
    for k in 0..3 {
        let (p, q) = (t[k], t[(k + 1) % 3]);
        let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        e[k] = [-s * dy / len, s * dx / len, s * (dy * p[0] - dx * p[1]) / len + grow];
    }
    let g = grow.max(0.0);
    let x0 = ((t.iter().map(|p| p[0]).fold(f32::MAX, f32::min) - g - 1.0).floor().max(0.0) as usize).max(clip[0]);
    let x1 = ((t.iter().map(|p| p[0]).fold(f32::MIN, f32::max) + g + 1.0).ceil().max(0.0) as usize).min(clip[2]);
    let y0 = ((t.iter().map(|p| p[1]).fold(f32::MAX, f32::min) - g - 1.0).floor().max(0.0) as usize).max(clip[1]);
    let y1 = ((t.iter().map(|p| p[1]).fold(f32::MIN, f32::max) + g + 1.0).ceil().max(0.0) as usize).min(clip[3]);
    for y in y0..y1 {
        let cy = y as f32 + 0.5;
        for x in x0..x1 {
            let cx = x as f32 + 0.5;
            if e.iter().all(|e| e[0] * cx + e[1] * cy + e[2] >= 0.0) && !f(x, y) {
                return;
            }
        }
    }
}

/// An attachment split evenly: every triangle into `n * n`, its vertices first, then `n - 1`
/// per edge (shared by the triangles on it), then each triangle's inner ones. Sub-triangle
/// `t * n * n + k` lies in triangle `t`, wound like it.
fn split(uvs: &[f32], tris: &[u16], verts: usize, n: usize) -> Option<IdleMesh> {
    let uv = |i: u16| [uvs[i as usize * 2], uvs[i as usize * 2 + 1]];
    let mut out_uv: Vec<f32> = uvs[..verts * 2].to_vec();
    let mut edges: std::collections::HashMap<(u16, u16), u32> = std::collections::HashMap::new();
    let mut out_tris: Vec<u32> = Vec::with_capacity(tris.len() / 3 * n * n * 3);
    for t in tris.chunks_exact(3) {
        let (a, b, c) = (t[0], t[1], t[2]);
        // Point k of n along p -> q.
        let mut along = |out_uv: &mut Vec<f32>, p: u16, q: u16, k: usize| -> u32 {
            if k == 0 {
                return p as u32;
            }
            if k == n {
                return q as u32;
            }
            let (lo, hi, kk) = if p < q { (p, q, k) } else { (q, p, n - k) };
            let base = *edges.entry((lo, hi)).or_insert_with(|| {
                let base = (out_uv.len() / 2) as u32;
                let (s, e) = (uv(lo), uv(hi));
                for j in 1..n {
                    let f = j as f32 / n as f32;
                    out_uv.extend_from_slice(&[s[0] + (e[0] - s[0]) * f, s[1] + (e[1] - s[1]) * f]);
                }
                base
            });
            base + kk as u32 - 1
        };
        let (pa, pb, pc) = (uv(a), uv(b), uv(c));
        // Lattice point (i, j) = a + (b - a) i / n + (c - a) j / n.
        let mut lattice = vec![0u32; (n + 1) * (n + 1)];
        for i in 0..=n {
            for j in 0..=n - i {
                lattice[i * (n + 1) + j] = if j == 0 {
                    along(&mut out_uv, a, b, i)
                } else if i == 0 {
                    along(&mut out_uv, a, c, j)
                } else if i + j == n {
                    along(&mut out_uv, b, c, j)
                } else {
                    let (u, v) = (i as f32 / n as f32, j as f32 / n as f32);
                    out_uv.extend_from_slice(&[pa[0] + (pb[0] - pa[0]) * u + (pc[0] - pa[0]) * v, pa[1] + (pb[1] - pa[1]) * u + (pc[1] - pa[1]) * v]);
                    (out_uv.len() / 2 - 1) as u32
                };
            }
        }
        let at = |i: usize, j: usize| lattice[i * (n + 1) + j];
        for i in 0..n {
            for j in 0..n - i {
                out_tris.extend_from_slice(&[at(i, j), at(i + 1, j), at(i, j + 1)]);
                if i + j + 2 <= n {
                    out_tris.extend_from_slice(&[at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]);
                }
            }
        }
    }
    if out_uv.len() / 2 > u16::MAX as usize {
        return None;
    }
    Some(IdleMesh { attachment: 0, uvs: out_uv, tris: out_tris.into_iter().map(|v| v as u16).collect(), solo: Vec::new() })
}

/// A call's placements over its frame's interval: the matrices at the start and the end (the
/// watch interpolates towards the next frame's call of the same shape when there is one).
fn placements(bake: &IdleBake, c: &IdleCall, next: &std::collections::HashMap<u16, [i16; 6]>) -> [[f32; 6]; 2] {
    let s = &bake.shapes[c.shape as usize];
    let m1 = c.m1.or_else(|| next.get(&c.shape).copied()).unwrap_or(c.m0);
    [IdleBake::matrix(s, &c.m0.map(|v| v as f32)), IdleBake::matrix(s, &m1.map(|v| v as f32))]
}

/// Rectangles (x0, y0, x1, y1, x1 and y1 exclusive) covering every pixel not in `hidden`, at
/// most `MAX_RECTS` (they may overlap: the watch fills non-zero). Bands of rows; in each, the
/// columns with a visible pixel, gaps up to a width closed; a band's spans carry on the
/// rectangles above while that keeps each within `MERGE_PX` of its first band's span. Of the
/// band heights and gap widths tried, the set leaving out the most pixels.
fn visible_rects(hidden: &[bool], w: usize, h: usize) -> Vec<[i16; 4]> {
    if !hidden.iter().any(|&x| x) {
        return Vec::new();
    }
    let mut best: Option<(usize, Vec<[i16; 4]>)> = None;
    for band in [8usize, 12, 16, 24, 32, 48] {
        let cols: Vec<(usize, usize, Vec<bool>)> = (0..h)
            .step_by(band)
            .map(|y0| {
                let y1 = (y0 + band).min(h);
                (y0, y1, (0..w).map(|x| (y0..y1).any(|y| !hidden[y * w + x])).collect())
            })
            .collect();
        for gap in [4usize, 8, 16, 32, 64] {
            // Open rectangles with their first band's span.
            let mut out: Vec<[i16; 4]> = Vec::new();
            let mut open: Vec<([i16; 4], (i16, i16))> = Vec::new();
            for (y0, y1, vis) in &cols {
                let mut spans: Vec<(usize, usize)> = Vec::new();
                let mut x = 0;
                while x < w {
                    if !vis[x] {
                        x += 1;
                        continue;
                    }
                    let (s0, mut e) = (x, x + 1);
                    let (mut k, mut run) = (x + 1, 0);
                    while k < w {
                        if vis[k] {
                            run = 0;
                            e = k + 1;
                        } else {
                            run += 1;
                            if run > gap {
                                break;
                            }
                        }
                        k += 1;
                    }
                    spans.push((s0, e));
                    x = e;
                }
                let mut next: Vec<([i16; 4], (i16, i16))> = Vec::new();
                for (s0, e) in spans {
                    let (s0, e) = (s0 as i16, e as i16);
                    let fit = open.iter().position(|(_, first)| (s0 - first.0).abs() <= MERGE_PX && (e - first.1).abs() <= MERGE_PX);
                    match fit {
                        Some(i) => {
                            let (r, first) = open.swap_remove(i);
                            next.push(([r[0].min(s0), r[1], r[2].max(e), *y1 as i16], first));
                        }
                        None => next.push(([s0, *y0 as i16, e, *y1 as i16], (s0, e))),
                    }
                }
                out.extend(open.drain(..).map(|(r, _)| r));
                open = next;
            }
            out.extend(open.drain(..).map(|(r, _)| r));
            if out.len() > MAX_RECTS {
                continue;
            }
            let left_out = outside(&out, w, h).iter().filter(|&&x| x).count();

            if best.as_ref().is_none_or(|b| left_out > b.0) {
                best = Some((left_out, out));
            }
        }
    }
    best.map_or_else(|| vec![[0, 0, w as i16, h as i16]], |b| b.1)
}

/// A square max (`max`) or min filter of radius `r` over `v` (w x h), rows then columns; outside
/// the screen counts as -1 (nothing covers it).
fn filter(v: &mut [i32], tmp: &mut [i32], w: usize, h: usize, r: usize, max: bool) {
    let pick = |a: i32, b: i32| if max { a.max(b) } else { a.min(b) };
    for y in 0..h {
        for x in 0..w {
            let mut m = v[y * w + x];
            for xx in x.saturating_sub(r)..=(x + r) {
                m = pick(m, if xx < w { v[y * w + xx] } else { -1 });
            }
            if x < r {
                m = pick(m, -1);
            }
            tmp[y * w + x] = m;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut m = tmp[y * w + x];
            for yy in y.saturating_sub(r)..=(y + r) {
                m = pick(m, if yy < h { tmp[yy * w + x] } else { -1 });
            }
            if y < r {
                m = pick(m, -1);
            }
            v[y * w + x] = m;
        }
    }
}

/// Pixels left uncovered by `rects` (what the watch clips away).
fn outside(rects: &[[i16; 4]], w: usize, h: usize) -> Vec<bool> {
    let mut hid = vec![true; w * h];
    for r in rects {
        for y in r[1].max(0) as usize..(r[3] as usize).min(h) {
            for x in r[0].max(0) as usize..(r[2] as usize).min(w) {
                hid[y * w + x] = false;
            }
        }
    }
    hid
}

/// Leaves out of `bakes` (layer, bake) what no frame shows; `pages[layer][page]` are the
/// decoded pages.
pub fn cull(lobby: &Lobby, pages: &[Vec<Texture>], bakes: &mut [(u16, IdleBake)]) -> CullReport {
    let mut report = CullReport::default();
    let t0 = std::time::Instant::now();
    let lap = |what: &str| {
        if std::env::var("BAL2D_CULL_DEBUG").is_ok() {
            eprintln!("  cull {what}: {:.1} s", t0.elapsed().as_secs_f32());
        }
    };
    let (w, h) = (lobby.width.round() as usize, lobby.height.round() as usize);
    let screen = (w * h) as f32;
    let layer_of: Vec<usize> = bakes.iter().map(|(li, _)| *li as usize).collect();
    let mut frames: Vec<Vec<Vec<IdleCall>>> = Vec::new();
    for (_, b) in bakes.iter() {
        let mut c = IdleCursor::new(b.shapes.len());
        let mut fs = Vec::with_capacity(b.frames as usize);
        let mut out = Vec::new();
        for _ in 0..b.frames {
            if c.next(b, &mut out).is_none() {
                return report;
            }
            fs.push(out.clone());
        }
        frames.push(fs);
    }
    let nexts = |fs: &[Vec<IdleCall>], fi: usize| -> std::collections::HashMap<u16, [i16; 6]> {
        if fi + 1 < fs.len() { fs[fi + 1].iter().map(|c| (c.shape, c.m0)).collect() } else { Default::default() }
    };

    // The fill as baked (every third frame), for the report.
    let step = 3;
    let coarse = 6;
    for (bi, (_, bake)) in bakes.iter().enumerate() {
        let li = layer_of[bi];
        let data = &lobby.layers[li].data;
        let (mut px, mut n) = (0f64, 0usize);
        for f in frames[bi].iter().step_by(coarse) {
            n += 1;
            for c in f.iter().filter(|c| c.shape != bal2d_pack::IDLE_LIVE) {
                let s = &bake.shapes[c.shape as usize];
                let Some((page, tris, _)) = bal2d_core::idle::geometry(data, s.attachment) else { continue };
                let Some(tex) = pages[li].get(page as usize) else { continue };
                let uv = bal2d_core::idle::uvs(data, s.attachment);
                let m = IdleBake::matrix(s, &c.m0.map(|v| v as f32));
                for &t in &s.tris {
                    let v: [[f32; 2]; 3] = core::array::from_fn(|k| {
                        let i = tris[t as usize * 3 + k] as usize;
                        apply(&m, [uv[i * 2] * tex.width as f32, uv[i * 2 + 1] * tex.height as f32])
                    });
                    raster_tri(v, 0.0, w, h, |_, _| {
                        px += 1.0;
                        true
                    });
                }
            }
        }
        report.fill_before += (px / n.max(1) as f64 / screen as f64) as f32;
    }

    lap("fill before");
    // Development: BAL2D_CULL=empty or clip does only that part.
    let only = std::env::var("BAL2D_CULL").unwrap_or_default();
    // Empty texels: split attachments with big triangles, drop the empty parts.
    for (bi, (_, bake)) in bakes.iter_mut().enumerate().filter(|_| only != "clip") {
        let li = layer_of[bi];
        let data = &lobby.layers[li].data;
        let mut longest: std::collections::HashMap<u32, f32> = Default::default();
        for f in &frames[bi] {
            for c in f {
                let Some(s) = bake.shapes.get(c.shape as usize) else { continue };
                let Some((page, tris, _)) = bal2d_core::idle::geometry(data, s.attachment) else { continue };
                let Some(t) = pages[li].get(page as usize) else { continue };
                let uv = bal2d_core::idle::uvs(data, s.attachment);
                let m = IdleBake::matrix(s, &c.m0.map(|v| v as f32));
                let e = longest.entry(s.attachment).or_insert(0.0);
                for &tri in &s.tris {
                    let v: [[f32; 2]; 3] = core::array::from_fn(|k| {
                        let i = tris[tri as usize * 3 + k] as usize;
                        apply(&m, [uv[i * 2] * t.width as f32, uv[i * 2 + 1] * t.height as f32])
                    });
                    for k in 0..3 {
                        let (p, q) = (v[k], v[(k + 1) % 3]);
                        *e = e.max(((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt());
                    }
                }
            }
        }
        let mut atts: Vec<(u32, f32)> = longest.into_iter().collect();
        atts.sort_by_key(|a| a.0);
        let mut meshes = Vec::new();
        for (att, len) in atts {
            let Some((page, tris, verts)) = bal2d_core::idle::geometry(data, att) else { continue };
            let Some(tex) = pages[li].get(page as usize) else { continue };
            let own_uv = bal2d_core::idle::uvs(data, att);
            let split_px = std::env::var("BAL2D_SPLIT_PX").ok().and_then(|v| v.parse().ok()).unwrap_or(SPLIT_PX);
            let n = ((len / split_px).ceil() as usize).clamp(1, MAX_SPLIT);
            let n = (1..=n).rev().find(|&k| tris.len() / 3 * k * k <= MAX_SPLIT_TRIS).unwrap_or(1);
            let mesh = if n >= 2 { split(own_uv, tris, verts, n) } else { None };
            let (mtris, muv): (&[u16], &[f32]) = match &mesh {
                Some(m) => (&m.tris, &m.uvs),
                None => (tris, own_uv),
            };
            let (pw, ph) = (tex.width as f32, tex.height as f32);
            let empty: Vec<bool> = mtris
                .chunks_exact(3)
                .map(|t| {
                    let v: [[f32; 2]; 3] = core::array::from_fn(|k| [muv[t[k] as usize * 2] * pw, muv[t[k] as usize * 2 + 1] * ph]);
                    // A triangle reaching past the page samples its edge (or nothing): kept.
                    if !v.iter().all(|p| p[0] >= EMPTY_PAD && p[1] >= EMPTY_PAD && p[0] <= pw - EMPTY_PAD && p[1] <= ph - EMPTY_PAD) {
                        return false;
                    }
                    let mut any = false;
                    raster_tri(v, EMPTY_PAD, tex.width, tex.height, |x, y| {
                        any = !tex.texel_kind(x, y).0;
                        !any
                    });
                    !any
                })
                .collect();
            let gone = empty.iter().filter(|&&e| e).count();
            let nn = n * n;
            // By texture area: the split's longer outlines cost the watch's CPU per frame.
            let area = |t: &[u16]| -> f32 {
                let p = |k: usize| [muv[t[k] as usize * 2] * pw, muv[t[k] as usize * 2 + 1] * ph];
                let (a, b, c) = (p(0), p(1), p(2));
                ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() * 0.5
            };
            let (gone_area, all_area) = mtris.chunks_exact(3).zip(&empty).fold((0f32, 0f32), |(g, a), (t, &e)| {
                let s = area(t);
                (g + if e { s } else { 0.0 }, a + s)
            });
            if mesh.is_some() && gone_area >= MIN_SPLIT_GAIN * all_area {
                let mut m = mesh.unwrap();
                let nn = nn as u16;
                for s in bake.shapes.iter_mut().filter(|s| s.attachment == att) {
                    s.tris = s.tris.iter().flat_map(|&t| (0..nn).map(move |k| t * nn + k)).filter(|&t| !empty[t as usize]).collect();
                }
                m.attachment = att;
                meshes.push(m);
            } else if gone > 0 {
                // Whole own triangles only.
                let own_empty: Vec<bool> = empty.chunks(if mesh.is_some() { nn } else { 1 }).map(|c| c.iter().all(|&e| e)).collect();
                for s in bake.shapes.iter_mut().filter(|s| s.attachment == att) {
                    s.tris.retain(|&t| !own_empty[t as usize]);
                }
            }
        }
        report.split_attachments += meshes.len();
        bake.meshes = meshes;
    }

    lap("empty texels");
    // Covered pixels. Per layer: for each pixel, the latest slot (index; the calls come in slot
    // order) whose opaque pixels cover it in every frame (-1: none).
    let mut latest: Vec<Vec<i32>> = vec![Vec::new(); bakes.len()];
    let mut ordered = vec![true; bakes.len()];
    for (bi, (_, bake)) in bakes.iter().enumerate() {
        let li = layer_of[bi];
        let data = &lobby.layers[li].data;
        let masks: Vec<Sat> = pages[li].iter().map(|t| Sat::new(t.width, t.height, |x, y| t.texel_kind(x, y).1)).collect();
        let mut m_min = vec![i32::MAX; w * h];
        let mut frame_max = vec![-1i32; w * h];
        let mut local: Vec<u8> = Vec::new();
        let mut scratch: Vec<i32> = vec![0; w * h];
        // The box of the pixels still possibly always covered.
        let mut live_box = [0usize, 0, w, h];
        for (fi, f) in frames[bi].iter().enumerate() {
            frame_max.iter_mut().for_each(|v| *v = -1);
            let next = nexts(&frames[bi], fi);
            let mut last_rank = -1i32;
            for c in f {
                if c.shape == bal2d_pack::IDLE_LIVE {
                    continue;
                }
                let s = &bake.shapes[c.shape as usize];
                let rank = s.slot as i32;
                if rank < last_rank {
                    ordered[bi] = false;
                }
                last_rank = rank;
                let col = bake.colors.get(c.color as usize).copied().unwrap_or(0);
                if data.slots[s.slot as usize].blend != BlendMode::Normal || col >> 24 != 255 || s.tris.is_empty() {
                    continue;
                }
                let Some((page, tris, _)) = bal2d_core::idle::geometry_in(bake, data, s.attachment) else { continue };
                let (Some(tex), Some(sat)) = (pages[li].get(page as usize), masks.get(page as usize)) else { continue };
                let uv = bal2d_core::idle::uvs_in(bake, data, s.attachment);
                let (pw, ph) = (tex.width as f32, tex.height as f32);
                let mats = placements(bake, c, &next);
                let Some(invs) = mats.iter().map(inverse).collect::<Option<Vec<_>>>() else { continue };
                let tri_at = |t: u16, m: &[f32; 6]| -> [[f32; 2]; 3] {
                    core::array::from_fn(|k| {
                        let i = tris[t as usize * 3 + k] as usize;
                        apply(m, [uv[i * 2] * pw, uv[i * 2 + 1] * ph])
                    })
                };
                let (mut lo, mut hi) = ([usize::MAX; 2], [0usize; 2]);
                for m in &mats {
                    for &t in &s.tris {
                        for p in tri_at(t, m) {
                            lo = [lo[0].min((p[0].max(0.0) as usize).min(w)), lo[1].min((p[1].max(0.0) as usize).min(h))];
                            hi = [hi[0].max((p[0].max(0.0) as usize + 2).min(w)), hi[1].max((p[1].max(0.0) as usize + 2).min(h))];
                        }
                    }
                }
                if lo[0] >= hi[0] || lo[1] >= hi[1] {
                    continue;
                }
                let (bw, bh) = (hi[0] - lo[0], hi[1] - lo[1]);
                // Too small to hide anything worth clipping.
                if bw * bh < 400 {
                    continue;
                }
                local.clear();
                local.resize(bw * bh, 0);
                for (k, (m, inv)) in mats.iter().zip(&invs).enumerate() {
                    for &t in &s.tris {
                        // Grown a little: neighbouring calls of a mesh meet without the cracks
                        // their own affines leave (the watch averages the shared vertices).
                        raster_tri_in(tri_at(t, m), CRACK_PX, live_box, |x, y| {
                            // Uncovered in an earlier frame: never always covered.
                            if x >= lo[0] && x < hi[0] && y >= lo[1] && y < hi[1] && m_min[y * w + x] != -1 {
                                let p = apply(inv, [x as f32 + 0.5, y as f32 + 0.5]);
                                let (n, all, out) = sat.count(p[0], p[1], OPAQUE_PAD);
                                if n == all && !out {
                                    local[(y - lo[1]) * bw + x - lo[0]] |= 1 << k;
                                }
                            }
                            true
                        });
                    }
                }
                // Opaque at both ends.
                for y in 0..bh {
                    for x in 0..bw {
                        if local[y * bw + x] == 3 {
                            let i = (y + lo[1]) * w + x + lo[0];
                            frame_max[i] = frame_max[i].max(rank);
                        }
                    }
                }
            }
            let mut b = [usize::MAX, usize::MAX, 0, 0];
            for (i, (m, &f)) in m_min.iter_mut().zip(&frame_max).enumerate() {
                *m = (*m).min(f);
                if *m != -1 {
                    let (x, y) = (i % w, i / w);
                    b = [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)];
                }
            }
            if b[0] == usize::MAX {
                break;
            }
            live_box = b;
        }
        // SHRINK_PX in from what is not covered, beyond the growth below (a min filter commutes
        // with the minimum over frames: once here).
        let mut m_min: Vec<i32> = m_min.into_iter().map(|v| if v == i32::MAX { -1 } else { v }).collect();
        filter(&mut m_min, &mut scratch, w, h, SHRINK_PX + 1, false);
        latest[bi] = m_min;
        if let Ok(dir) = std::env::var("BAL2D_CULL_DUMP") {
            let data: Vec<u8> = latest[bi].iter().flat_map(|&v| if v >= 0 { [255, (v * 7 % 255) as u8, 0, 255] } else { [0, 0, 0, 255] }).collect();
            let png = crate::image::encode_png(&crate::image::Rgba { width: w as u32, height: h as u32, data });
            let _ = std::fs::write(format!("{dir}/latest-{li}.png"), png);
        }
    }

    lap("cover");
    // Per layer: what the layers above always cover, the best split, the rectangles and which
    // shapes are worth clipping.
    let mut fills = [0f64; 3];
    for bi in 0..bakes.len() {
        let li = layer_of[bi];
        let above: Vec<bool> = (0..w * h).map(|i| (bi + 1..bakes.len()).any(|u| latest[u][i] >= 0)).collect();
        let data = &lobby.layers[li].data;
        let footprint = |bake: &IdleBake, c: &IdleCall, f: &mut dyn FnMut(usize)| {
            let s = &bake.shapes[c.shape as usize];
            let Some((page, tris, _)) = bal2d_core::idle::geometry_in(bake, data, s.attachment) else { return };
            let Some(tex) = pages[li].get(page as usize) else { return };
            let uv = bal2d_core::idle::uvs_in(bake, data, s.attachment);
            let m = IdleBake::matrix(s, &c.m0.map(|v| v as f32));
            for &t in &s.tris {
                let v: [[f32; 2]; 3] = core::array::from_fn(|k| {
                    let i = tris[t as usize * 3 + k] as usize;
                    apply(&m, [uv[i * 2] * tex.width as f32, uv[i * 2 + 1] * tex.height as f32])
                });
                raster_tri(v, 0.0, w, h, |x, y| {
                    f(y * w + x);
                    true
                });
            }
        };
        let bake = &bakes[bi].1;
        // Fill each split would save: a call of slot r at a pixel whose latest cover is M is
        // hidden for every split in (r, M].
        let nslots = data.slots.len() + 1;
        let mut gain = vec![0i64; nslots + 1];
        if ordered[bi] {
            for f in frames[bi].iter().step_by(coarse) {
                for c in f.iter().filter(|c| c.shape != bal2d_pack::IDLE_LIVE) {
                    let r = bake.shapes[c.shape as usize].slot as i32;
                    footprint(bake, c, &mut |i| {
                        let m = latest[bi][i];
                        if m > r {
                            gain[(r + 1) as usize] += 1;
                            gain[(m + 1) as usize] -= 1;
                        }
                    });
                }
            }
        }
        let mut best = (0i64, 0usize);
        let mut acc = 0i64;
        for (k, g) in gain.iter().enumerate().take(nslots) {
            acc += g;
            if acc > best.0 {
                best = (acc, k);
            }
        }
        let split = best.1 as i32;
        let later: Vec<bool> = (0..w * h).map(|i| split > 0 && latest[bi][i] >= split).collect();
        let both: Vec<bool> = later.iter().zip(&above).map(|(a, b)| *a || *b).collect();
        let regions = [visible_rects(&later, w, h), visible_rects(&above, w, h), visible_rects(&both, w, h)];
        let hidden: Vec<Vec<bool>> = regions.iter().zip([&later, &above, &both]).map(|(r, hm)| if hm.iter().any(|&x| x) { outside(r, w, h) } else { vec![false; w * h] }).collect();
        // Per shape: calls, the pixels clipping saves with the later cover and with both, and
        // the rectangles its calls meet in each case.
        let mut per: Vec<(u32, [u64; 2], [u64; 2])> = vec![(0, [0; 2], [0; 2]); bake.shapes.len()];
        let mut lf = 0f64;
        let mut sampled = 0usize;
        for f in frames[bi].iter().step_by(step) {
            sampled += 1;
            for c in f.iter().filter(|c| c.shape != bal2d_pack::IDLE_LIVE) {
                let before = (bake.shapes[c.shape as usize].slot as i32) < split;
                let e = &mut per[c.shape as usize];
                e.0 += 1;
                let (mut lo, mut hi) = ([usize::MAX; 2], [0usize; 2]);
                footprint(bake, c, &mut |i| {
                    lf += 1.0;
                    let (x, y) = (i % w, i / w);
                    lo = [lo[0].min(x), lo[1].min(y)];
                    hi = [hi[0].max(x + 1), hi[1].max(y + 1)];
                    if before && hidden[0][i] {
                        e.1[0] += 1;
                    }
                    if if before { hidden[2][i] } else { hidden[1][i] } {
                        e.1[1] += 1;
                    }
                });
                let meets = |rs: &[[i16; 4]]| rs.iter().filter(|r| (r[0] as usize) < hi[0] && (r[2] as usize) > lo[0] && (r[1] as usize) < hi[1] && (r[3] as usize) > lo[1]).count() as u64;
                e.2[0] += meets(&regions[0]);
                e.2[1] += meets(if before { &regions[2] } else { &regions[1] });
            }
        }
        // Shapes clipped: only an outline of one convex loop of a few points (a background
        // quad: the watch clips it to a rectangle in a few comparisons). Longer outlines cost the
        // watch's CPU more than the fill clipping them saves (timing model: light beams through
        // two dozen rectangles doubled the bake's CPU time).
        let convex = |s: &bal2d_pack::IdleShape| -> bool {
            let bake = &bakes[bi].1;
            let Some((_, tris, _)) = bal2d_core::idle::geometry_in(bake, data, s.attachment) else { return false };
            let uv = bal2d_core::idle::uvs_in(bake, data, s.attachment);
            let mut edges: std::collections::HashMap<(u16, u16), (u32, (u16, u16))> = Default::default();
            for &t in &s.tris {
                let v = &tris[t as usize * 3..t as usize * 3 + 3];
                for k in 0..3 {
                    let (a, b) = (v[k], v[(k + 1) % 3]);
                    let e = edges.entry((a.min(b), a.max(b))).or_insert((0, (a, b)));
                    e.0 += 1;
                }
            }
            let border: Vec<(u16, u16)> = edges.values().filter(|e| e.0 == 1).map(|e| e.1).collect();
            if border.is_empty() || border.len() > 8 {
                return false;
            }
            // One loop, every turn the same way.
            let next: std::collections::HashMap<u16, u16> = border.iter().copied().collect();
            let mut at = border[0].0;
            let mut lp = Vec::new();
            for _ in 0..border.len() {
                lp.push(at);
                let Some(&n) = next.get(&at) else { return false };
                at = n;
            }
            if at != border[0].0 {
                return false;
            }
            let p = |v: u16| (uv[v as usize * 2], uv[v as usize * 2 + 1]);
            let n = lp.len();
            let turns: Vec<f32> = (0..n)
                .map(|j| {
                    let (a, b, c) = (p(lp[j]), p(lp[(j + 1) % n]), p(lp[(j + 2) % n]));
                    (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0)
                })
                .collect();
            turns.iter().all(|&t| t >= 0.0) || turns.iter().all(|&t| t <= 0.0)
        };
        let verts: Vec<usize> = bakes[bi].1.shapes.iter().map(|s| if convex(s) { 4 } else { usize::MAX / 1024 }).collect();
        let bake = &mut bakes[bi].1;
        let (mut saved, mut saved_above) = (0f64, 0f64);
        for ((s, (n, px, meets)), &nv) in bake.shapes.iter_mut().zip(&per).zip(&verts) {
            let before = (s.slot as i32) < split;
            s.clip = if before && !regions[0].is_empty() { IDLE_BEFORE } else { 0 };
            if *n == 0 {
                continue;
            }
            let avg = |v: u64| v as f32 / *n as f32;
            let worth = |k: usize| avg(px[k]) >= MIN_SAVED_PX.max(PX_PER_CLIP * nv as f32 * avg(meets[k]));
            if before && !regions[0].is_empty() && worth(0) {
                s.clip |= IDLE_CLIP_LATER;
                saved += px[0] as f64;
            }
            if !regions[1].is_empty() && worth(1) {
                s.clip |= IDLE_CLIP_ABOVE;
                saved_above += px[1] as f64;
            } else if s.clip & IDLE_CLIP_LATER != 0 {
                saved_above += px[0] as f64;
            }
        }
        if only == "empty" {
            bake.shapes.iter_mut().for_each(|s| s.clip = 0);
        }
        let any_above = !regions[1].is_empty() && only != "empty";
        bake.regions = if bake.shapes.iter().any(|s| s.clip != 0) || any_above { regions } else { Default::default() };
        let k = 1.0 / sampled.max(1) as f64;
        fills[0] += lf * k;
        fills[1] += (lf - saved) * k;
        fills[2] += (lf - saved_above) * k;
        if std::env::var("BAL2D_CULL_DEBUG").is_ok() {
            eprintln!(
                "  layer {li}: split at slot {split} ({}), rects {}/{}/{}, clipped shapes {} later / {} above; fill {:.2} -> {:.2} / {:.2}",
                data.slots.get(split as usize).map_or("-", |s| s.name.as_str()),
                bake.regions[0].len(),
                bake.regions[1].len(),
                bake.regions[2].len(),
                bake.shapes.iter().filter(|s| s.clip & IDLE_CLIP_LATER != 0).count(),
                bake.shapes.iter().filter(|s| s.clip & IDLE_CLIP_ABOVE != 0).count(),
                lf * k / screen as f64,
                (lf - saved) * k / screen as f64,
                (lf - saved_above) * k / screen as f64
            );
        }
    }
    lap("regions");
    // Split points no two shapes of one frame have on their outlines.
    for (bi, (_, bake)) in bakes.iter_mut().enumerate() {
        if bake.meshes.is_empty() {
            continue;
        }
        let edge_verts = |tris: &[u16], sel: &[u16]| -> Vec<u16> {
            let mut edges: std::collections::HashMap<(u16, u16), u32> = Default::default();
            for &t in sel {
                let v = &tris[t as usize * 3..t as usize * 3 + 3];
                for k in 0..3 {
                    let (a, b) = (v[k], v[(k + 1) % 3]);
                    *edges.entry((a.min(b), a.max(b))).or_default() += 1;
                }
            }
            let mut vs: Vec<u16> = edges.iter().filter(|e| *e.1 == 1).flat_map(|e| [e.0 .0, e.0 .1]).collect();
            vs.sort_unstable();
            vs.dedup();
            vs
        };
        let border: Vec<Vec<u16>> = bake
            .shapes
            .iter()
            .map(|s| match bake.mesh(s.attachment) {
                Some(m) => edge_verts(&m.tris, &s.tris),
                None => Vec::new(),
            })
            .collect();
        let mut shared: Vec<Vec<bool>> = bake.meshes.iter().map(|m| vec![false; m.uvs.len() / 2]).collect();
        for f in &frames[bi] {
            let mut seen: std::collections::HashMap<(usize, u16), u16> = Default::default();
            for c in f.iter().filter(|c| c.shape != bal2d_pack::IDLE_LIVE) {
                let s = &bake.shapes[c.shape as usize];
                let Ok(mi) = bake.meshes.binary_search_by_key(&s.attachment, |m| m.attachment) else { continue };
                for &v in &border[c.shape as usize] {
                    match seen.get(&(mi, v)) {
                        Some(&other) if other != c.shape => shared[mi][v as usize] = true,
                        _ => {
                            seen.insert((mi, v), c.shape);
                        }
                    }
                }
            }
        }
        for (m, sh) in bake.meshes.iter_mut().zip(&shared) {
            m.solo = vec![0u8; sh.len() / 8 + 1];
            for (v, &x) in sh.iter().enumerate() {
                if !x {
                    m.solo[v / 8] |= 1 << (v % 8);
                }
            }
        }
    }
    report.fill_after_empty = (fills[0] / screen as f64) as f32;
    report.fill_after = (fills[1] / screen as f64) as f32;
    report.fill_after_above = (fills[2] / screen as f64) as f32;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_split_quad_shares_its_inner_edges_both_ways() {
        let uvs = [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0];
        let tris = [0u16, 1, 2, 0, 2, 3];
        let m = split(&uvs, &tris, 4, 3).unwrap();
        assert_eq!(m.tris.len() / 3, 18);
        assert_eq!(m.uvs.len() / 2, 16);
        assert_eq!(&m.uvs[..8], &uvs);
        let mut edges: std::collections::HashMap<(u16, u16), i32> = Default::default();
        let mut area = 0.0f32;
        for t in m.tris.chunks_exact(3) {
            let p = |k: usize| (m.uvs[t[k] as usize * 2], m.uvs[t[k] as usize * 2 + 1]);
            let (a, b, c) = (p(0), p(1), p(2));
            let s = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
            assert!(s > 0.0, "every sub-triangle wound like the quad's");
            area += s * 0.5;
            for k in 0..3 {
                let (x, y) = (t[k], t[(k + 1) % 3]);
                *edges.entry((x.min(y), x.max(y))).or_default() += if x < y { 1 } else { -1 };
            }
        }
        assert!((area - 1.0).abs() < 1e-5);
        // Inner edges cancel; the border (12 sub-edges) does not.
        assert_eq!(edges.values().filter(|&&v| v != 0).count(), 12);
    }

    #[test]
    fn visible_rectangles_cover_every_pixel_not_hidden() {
        let (w, h) = (100, 90);
        let hidden: Vec<bool> = (0..w * h).map(|i| {
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            (x - 60).pow(2) + (y - 50).pow(2) < 30 * 30 && !(x > 55 && x < 60 && y > 45 && y < 52)
        }).collect();
        let rects = visible_rects(&hidden, w, h);
        assert!(!rects.is_empty() && rects.len() <= MAX_RECTS);
        let out = outside(&rects, w, h);
        for i in 0..w * h {
            assert!(!out[i] || hidden[i], "pixel {} {} visible but outside every rectangle", i % w, i / w);
        }
        // And most of the hidden disc is left out.
        let left_out = out.iter().filter(|&&x| x).count();
        let disc = hidden.iter().filter(|&&x| x).count();
        assert!(left_out * 2 > disc, "{left_out} of {disc}");
    }
}

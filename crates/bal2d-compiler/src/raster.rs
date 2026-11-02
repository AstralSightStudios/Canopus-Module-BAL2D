//! Reference software rasterizer: affine-textured triangles, bilinear sampling, Spine's
//! premultiplied-alpha blend modes. It is the pixel reference the GPU path is compared with.

use bal2d_render::{Batch, DrawList};
use bal2d_spine::BlendMode;

/// Premultiplied RGBA texture, f32 per channel in 0..1.
pub struct Texture {
    pub width: usize,
    pub height: usize,
    pub texels: Vec<[f32; 4]>,
}

impl Texture {
    /// From straight-alpha RGBA8 (the BA atlases are not premultiplied).
    pub fn from_straight_rgba8(width: usize, height: usize, rgba: &[u8]) -> Texture {
        let texels = rgba
            .chunks_exact(4)
            .map(|p| {
                let a = p[3] as f32 / 255.0;
                [p[0] as f32 / 255.0 * a, p[1] as f32 / 255.0 * a, p[2] as f32 / 255.0 * a, a]
            })
            .collect();
        Texture { width, height, texels }
    }

    pub fn from_premultiplied_rgba8(width: usize, height: usize, rgba: &[u8]) -> Texture {
        let texels = rgba.chunks_exact(4).map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0]).collect();
        Texture { width, height, texels }
    }

    /// Bilinear sample with texel centers at (i + 0.5) / size, clamped to the edge.
    pub fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let x = u * self.width as f32 - 0.5;
        let y = v * self.height as f32 - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let fx = x - x0;
        let fy = y - y0;
        let cl = |v: f32, n: usize| (v as isize).clamp(0, n as isize - 1) as usize;
        let (xa, xb) = (cl(x0, self.width), cl(x0 + 1.0, self.width));
        let (ya, yb) = (cl(y0, self.height), cl(y0 + 1.0, self.height));
        let t = |x: usize, y: usize| self.texels[y * self.width + x];
        let (p00, p10, p01, p11) = (t(xa, ya), t(xb, ya), t(xa, yb), t(xb, yb));
        let mut out = [0f32; 4];
        for k in 0..4 {
            let top = p00[k] + (p10[k] - p00[k]) * fx;
            let bot = p01[k] + (p11[k] - p01[k]) * fx;
            out[k] = top + (bot - top) * fy;
        }
        out
    }
}

/// Skeleton space to screen: screen = ((x - cx) * scale + w/2, h/2 - (y - cy) * scale).
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub cx: f32,
    pub cy: f32,
    pub scale: f32,
}

pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    /// Premultiplied RGBA.
    pub pixels: Vec<[f32; 4]>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize, clear: [f32; 4]) -> Framebuffer {
        Framebuffer { width, height, pixels: vec![clear; width * height] }
    }

    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for p in &self.pixels {
            // Opaque output: composite over black.
            for k in 0..3 {
                out.push((p[k].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
            out.push(255);
        }
        out
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    pub triangles: usize,
    pub pixels: usize,
}

/// Draws the whole list.
pub fn draw(fb: &mut Framebuffer, list: &DrawList, pages: &[Texture], cam: Camera) -> Stats {
    draw_as_gpu(fb, list, pages, cam, None)
}

/// [`draw`], optionally the way the watch's GPU draws it (`Some(tolerance)`): each batch's
/// triangles are grouped like `bal2d_render::Paths` does and every group samples its texture
/// through the group's single affine, not through its own triangles' texture coordinates.
pub fn draw_as_gpu(fb: &mut Framebuffer, list: &DrawList, pages: &[Texture], cam: Camera, tolerance: Option<f32>) -> Stats {
    let Some(tol) = tolerance else { return draw_exact(fb, list, pages, cam) };
    let mut stats = Stats::default();
    let (w2, h2) = (fb.width as f32 * 0.5, fb.height as f32 * 0.5);
    let (mut tris, mut order, mut clusters) = (Vec::new(), Vec::new(), Vec::new());
    for b in &list.batches {
        let tex = &pages[b.page as usize];
        // The watch's pattern fill modulates by one color: no two-color tint.
        let tint = Tint { dark: None, ..tint(b) };
        let (pw, ph) = (tex.width as f32, tex.height as f32);
        tris.clear();
        let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
        for t in idx.chunks_exact(3) {
            let mut tri = bal2d_render::Tri::default();
            for k in 0..3 {
                let vi = b.first_vertex as usize + t[k] as usize;
                tri.s[k] = [(list.positions[vi * 2] - cam.cx) * cam.scale + w2, h2 - (list.positions[vi * 2 + 1] - cam.cy) * cam.scale];
                tri.t[k] = [list.uvs[vi * 2] * pw, list.uvs[vi * 2 + 1] * ph];
                tri.v[k] = vi as u32;
            }
            let (minx, maxx) = (tri.s.iter().map(|p| p[0]).fold(f32::MAX, f32::min), tri.s.iter().map(|p| p[0]).fold(f32::MIN, f32::max));
            let (miny, maxy) = (tri.s.iter().map(|p| p[1]).fold(f32::MAX, f32::min), tri.s.iter().map(|p| p[1]).fold(f32::MIN, f32::max));
            if maxx < 0.0 || maxy < 0.0 || minx > fb.width as f32 || miny > fb.height as f32 {
                continue;
            }
            tris.push(tri);
        }
        bal2d_render::cluster(&tris, tol, &mut order, &mut clusters);
        for c in &clusters {
            let m = c.m;
            let det = m[0] * m[4] - m[1] * m[3];
            if det.abs() < 1e-12 {
                continue;
            }
            // Screen -> uv: the inverse of the texel -> screen affine, divided by the page size.
            let inv = [m[4] / det / pw, -m[1] / det / pw, -m[3] / det / ph, m[0] / det / ph];
            let to_uv = |x: f32, y: f32| {
                let (dx, dy) = (x - m[2], y - m[5]);
                [inv[0] * dx + inv[1] * dy, inv[2] * dx + inv[3] * dy]
            };
            for &ti in &order[c.start as usize..(c.start + c.count) as usize] {
                let t = &tris[ti as usize];
                stats.triangles += 1;
                stats.pixels += raster_triangle(fb, &t.s, &[[0.0; 2]; 3], Some(&to_uv), tex, &tint, b);
            }
        }
    }
    stats
}

fn draw_exact(fb: &mut Framebuffer, list: &DrawList, pages: &[Texture], cam: Camera) -> Stats {
    let mut stats = Stats::default();
    let (w2, h2) = (fb.width as f32 * 0.5, fb.height as f32 * 0.5);
    for b in &list.batches {
        let tex = &pages[b.page as usize];
        let tint = tint(b);
        let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
        for t in idx.chunks_exact(3) {
            let mut sp = [[0f32; 2]; 3];
            let mut uv = [[0f32; 2]; 3];
            for k in 0..3 {
                let vi = b.first_vertex as usize + t[k] as usize;
                let x = list.positions[vi * 2];
                let y = list.positions[vi * 2 + 1];
                sp[k] = [(x - cam.cx) * cam.scale + w2, h2 - (y - cam.cy) * cam.scale];
                uv[k] = [list.uvs[vi * 2], list.uvs[vi * 2 + 1]];
            }
            stats.triangles += 1;
            stats.pixels += raster_triangle(fb, &sp, &uv, None, tex, &tint, b);
        }
    }
    stats
}

struct Tint {
    /// Premultiplied light color; alpha is 0 for additive (Spine's PMA additive trick).
    light: [f32; 4],
    dark: Option<[f32; 3]>,
}

fn tint(b: &Batch) -> Tint {
    let [r, g, bl, a] = b.color;
    let alpha = if b.blend == BlendMode::Additive { 0.0 } else { a };
    Tint { light: [r * a, g * a, bl * a, alpha], dark: b.dark }
}

fn edge(a: [f32; 2], b: [f32; 2], p: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

/// Top-left fill rule: pixels exactly on a shared edge belong to exactly one triangle.
fn is_top_left(a: [f32; 2], b: [f32; 2], ccw: bool) -> bool {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    if ccw { (dy == 0.0 && dx < 0.0) || dy > 0.0 } else { (dy == 0.0 && dx > 0.0) || dy < 0.0 }
}

/// `to_uv`: texture coordinates from the pixel center instead of interpolating `uv`.
fn raster_triangle(fb: &mut Framebuffer, p: &[[f32; 2]; 3], uv: &[[f32; 2]; 3], to_uv: Option<&dyn Fn(f32, f32) -> [f32; 2]>, tex: &Texture, tint: &Tint, b: &Batch) -> usize {
    let area = edge(p[0], p[1], p[2]);
    if area.abs() < 1e-9 {
        return 0;
    }
    let ccw = area > 0.0;
    let min_x = p.iter().map(|q| q[0]).fold(f32::MAX, f32::min).floor().max(0.0) as isize;
    let max_x = p.iter().map(|q| q[0]).fold(f32::MIN, f32::max).ceil().min(fb.width as f32) as isize;
    let min_y = p.iter().map(|q| q[1]).fold(f32::MAX, f32::min).floor().max(0.0) as isize;
    let max_y = p.iter().map(|q| q[1]).fold(f32::MIN, f32::max).ceil().min(fb.height as f32) as isize;
    if min_x >= max_x || min_y >= max_y {
        return 0;
    }
    let tl = [is_top_left(p[1], p[2], ccw), is_top_left(p[2], p[0], ccw), is_top_left(p[0], p[1], ccw)];
    let inv = 1.0 / area;
    let mut n = 0;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let c = [x as f32 + 0.5, y as f32 + 0.5];
            let w0 = edge(p[1], p[2], c);
            let w1 = edge(p[2], p[0], c);
            let w2 = edge(p[0], p[1], c);
            let inside = |w: f32, top_left: bool| if ccw { w > 0.0 || (w == 0.0 && top_left) } else { w < 0.0 || (w == 0.0 && top_left) };
            if !(inside(w0, tl[0]) && inside(w1, tl[1]) && inside(w2, tl[2])) {
                continue;
            }
            let (u, v) = match to_uv {
                Some(f) => {
                    let [u, v] = f(c[0], c[1]);
                    (u, v)
                }
                None => {
                    let (l0, l1, l2) = (w0 * inv, w1 * inv, w2 * inv);
                    (l0 * uv[0][0] + l1 * uv[1][0] + l2 * uv[2][0], l0 * uv[0][1] + l1 * uv[1][1] + l2 * uv[2][1])
                }
            };
            let t = tex.sample(u, v);
            let src = match tint.dark {
                None => [t[0] * tint.light[0], t[1] * tint.light[1], t[2] * tint.light[2], t[3] * tint.light[3]],
                Some(dark) => {
                    // Two-color tint (PMA): ((ta - 1) + 1 - trgb) * dark + trgb * light.
                    let mut o = [0f32; 4];
                    for k in 0..3 {
                        o[k] = ((t[3] - 1.0) + 1.0 - t[k]) * dark[k] * tint.light[3].max(1e-6).min(1.0) + t[k] * tint.light[k];
                    }
                    o[3] = t[3] * tint.light[3];
                    o
                }
            };
            let d = &mut fb.pixels[y as usize * fb.width + x as usize];
            match b.blend {
                BlendMode::Normal | BlendMode::Additive => {
                    for k in 0..4 {
                        d[k] = src[k] + d[k] * (1.0 - src[3]);
                    }
                }
                BlendMode::Multiply => {
                    for k in 0..3 {
                        d[k] = src[k] * d[k] + d[k] * (1.0 - src[3]);
                    }
                    d[3] = src[3] + d[3] * (1.0 - src[3]);
                }
                BlendMode::Screen => {
                    for k in 0..3 {
                        d[k] = src[k] + d[k] * (1.0 - src[k]);
                    }
                    d[3] = src[3] + d[3] * (1.0 - src[3]);
                }
            }
            n += 1;
        }
    }
    n
}

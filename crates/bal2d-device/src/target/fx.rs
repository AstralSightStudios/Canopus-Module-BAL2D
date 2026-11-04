//! Touch effects in the game's style: a press sends out a light-blue ring with a soft flash at
//! its centre and a few small triangles flying off; a drag leaves a trail of fading sparks.
//!
//! The sprites (ring, soft dot, triangle) are drawn once into small white BGRA8888 pages at
//! start; each particle is one textured quad (a path through the same `draw_pattern` as the
//! lobby), tinted by the GPU. Coordinates are the layer's pixels, like the lobby's calls.

use alloc::vec::Vec;

use bal2d_render::{Call, path_coord, PATH_CLOSE, PATH_LINE, PATH_MOVE};
use bal2d_spine::BlendMode;
use bal2d_spine::math::{cos, sin, sqrt};

use super::gpu;

const RING: u8 = 0;
const DOT: u8 = 1;
const TRI: u8 = 2;
/// Sprite sizes (square pages; widths are multiples of 16 as the GPU pages need).
const RING_PX: u32 = 64;
const SMALL_PX: u32 = 32;
/// The ring sprite's radius (its centre line) in sprite pixels.
const RING_R: f32 = 24.0;
const MAX_PARTS: usize = 32;
/// Trail sparks: one per this many pixels of finger travel.
const TRAIL_STEP: f32 = 10.0;

/// Blue Archive's touch colour, and white.
const CYAN: [f32; 3] = [0.38, 0.80, 1.0];
const WHITE: [f32; 3] = [0.92, 0.98, 1.0];
/// The soft glow: light blue (additive light vanishes on the characters' white and pastel).
const GLOW: [f32; 3] = [0.62, 0.88, 1.0];

#[derive(Clone, Copy, Default)]
struct Part {
    kind: u8,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    age: f32,
    life: f32,
    /// Radius (half the quad's side) at the start and at the end.
    r0: f32,
    r1: f32,
    rot: f32,
    spin: f32,
    color: [f32; 3],
    alpha: f32,
}

pub struct Fx {
    /// Ring, dot, triangle; None until made (or when the heap had no room).
    pages: Option<Vec<gpu::Page>>,
    tried: bool,
    parts: Vec<Part>,
    /// Where the last trail spark went, while a finger is down.
    trail: Option<(f32, f32)>,
    trail_n: u32,
    seed: u32,
    path: Vec<i16>,
}

impl Fx {
    pub const fn new() -> Fx {
        Fx { pages: None, tried: false, parts: Vec::new(), trail: None, trail_n: 0, seed: 0x9E37_79B9, path: Vec::new() }
    }

    fn rand(&mut self) -> f32 {
        // xorshift32
        let mut x = self.seed;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.seed = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }

    fn push(&mut self, p: Part) {
        if self.parts.len() >= MAX_PARTS {
            self.parts.remove(0);
        }
        self.parts.push(p);
    }

    /// A touch at layer pixels (kind: 0 press, 1 move, 2 release).
    pub fn touch(&mut self, kind: u8, x: f32, y: f32) {
        if self.parts.capacity() == 0 {
            self.parts.reserve_exact(MAX_PARTS);
        }
        match kind {
            0 => {
                self.seed ^= (x as u32).wrapping_mul(0x85EB_CA6B) ^ (y as u32).wrapping_mul(0xC2B2_AE35);
                self.burst(x, y);
                self.trail = Some((x, y));
                self.trail_n = 0;
            }
            1 => {
                let Some((lx, ly)) = self.trail else { return };
                let (dx, dy) = (x - lx, y - ly);
                let d = sqrt(dx * dx + dy * dy);
                if d < TRAIL_STEP {
                    return;
                }
                // Sparks along the way, at most a few per move.
                let n = ((d / TRAIL_STEP) as u32).min(4);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    self.spark(lx + dx * t, ly + dy * t, dx / d, dy / d);
                }
                self.trail = Some((x, y));
            }
            _ => self.trail = None,
        }
    }

    fn burst(&mut self, x: f32, y: f32) {
        self.push(Part { kind: RING, x, y, life: 0.42, r0: 10.0, r1: 46.0, color: CYAN, alpha: 0.95, ..Default::default() });
        self.push(Part { kind: RING, x, y, life: 0.30, r0: 6.0, r1: 30.0, color: WHITE, alpha: 0.55, ..Default::default() });
        self.push(Part { kind: DOT, x, y, life: 0.24, r0: 14.0, r1: 22.0, color: GLOW, alpha: 0.6, ..Default::default() });
        let n = 5;
        let start = self.rand() * core::f32::consts::TAU;
        for i in 0..n {
            let a = start + i as f32 * core::f32::consts::TAU / n as f32 + (self.rand() - 0.5) * 0.7;
            let speed = 150.0 + self.rand() * 90.0;
            let size = 5.0 + self.rand() * 4.0;
            let spin = (self.rand() - 0.5) * 14.0;
            let life = 0.45 + self.rand() * 0.2;
            let rot = self.rand() * core::f32::consts::TAU;
            self.push(Part {
                kind: TRI,
                x: x + cos(a) * 8.0,
                y: y + sin(a) * 8.0,
                vx: cos(a) * speed,
                vy: sin(a) * speed,
                life,
                r0: size,
                r1: size * 0.6,
                rot,
                spin,
                color: if i % 2 == 0 { CYAN } else { WHITE },
                alpha: 0.95,
                age: 0.0,
            });
        }
    }

    fn spark(&mut self, x: f32, y: f32, dx: f32, dy: f32) {
        self.trail_n += 1;
        let j = (self.rand() - 0.5) * 6.0;
        self.push(Part { kind: DOT, x: x - dy * j, y: y + dx * j, life: 0.4, r0: 12.0, r1: 4.0, color: CYAN, alpha: 0.85, ..Default::default() });
        if self.trail_n % 3 == 0 {
            let a = self.rand() * core::f32::consts::TAU;
            self.push(Part {
                kind: TRI,
                x,
                y,
                vx: cos(a) * 40.0,
                vy: sin(a) * 40.0,
                life: 0.4,
                r0: 4.5,
                r1: 2.5,
                rot: a,
                spin: 6.0,
                color: WHITE,
                alpha: 0.85,
                ..Default::default()
            });
        }
    }

    pub fn update(&mut self, dt: f32) {
        for p in &mut self.parts {
            p.age += dt;
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            // Drag: the triangles slow down as they fly off.
            let k = 1.0 - (5.0 * dt).min(1.0);
            p.vx *= k;
            p.vy *= k;
            p.rot += p.spin * dt;
        }
        self.parts.retain(|p| p.age < p.life);
    }

    /// The sprite pages, made on first use (a few tens of KB of GPU memory).
    fn pages(&mut self) -> Option<&mut Vec<gpu::Page>> {
        if self.pages.is_none() && !self.tried {
            self.tried = true;
            let make = |px: u32, f: &dyn Fn(f32, f32) -> f32| -> Option<gpu::Page> {
                let mut pixels = alloc::vec![0u8; (px * px * 4) as usize];
                let c = px as f32 * 0.5;
                for y in 0..px {
                    for x in 0..px {
                        let a = f(x as f32 + 0.5 - c, y as f32 + 0.5 - c).clamp(0.0, 1.0);
                        let v = (a * 255.0 + 0.5) as u8;
                        let o = ((y * px + x) * 4) as usize;
                        // Premultiplied white.
                        pixels[o..o + 4].copy_from_slice(&[v, v, v, v]);
                    }
                }
                gpu::Page::from_bgra8888(px, px, &pixels)
            };
            let ring = make(RING_PX, &|x, y| {
                let e = (sqrt(x * x + y * y) - RING_R).abs();
                let core = 2.6 - e;
                let glow = (1.0 - e / 7.0).max(0.0);
                core.max(glow * glow * 0.45)
            });
            let dot = make(SMALL_PX, &|x, y| {
                let d = sqrt(x * x + y * y) / 15.0;
                let t = (1.0 - d).max(0.0);
                t * t * (3.0 - 2.0 * t)
            });
            // An equilateral triangle pointing up, its corners 13 px from the centre.
            let tri = make(SMALL_PX, &|x, y| {
                let r = 13.0 * 0.5; // inradius
                let mut out = f32::MIN;
                for k in 0..3 {
                    let a = core::f32::consts::FRAC_PI_2 + k as f32 * core::f32::consts::TAU / 3.0 + core::f32::consts::PI;
                    let (nx, ny) = (cos(a), sin(a));
                    out = out.max(x * nx + y * ny - r);
                }
                0.5 - out
            });
            if let (Some(a), Some(b), Some(c)) = (ring, dot, tri) {
                self.pages = Some(alloc::vec![a, b, c]);
            }
        }
        self.pages.as_mut()
    }

    /// Draws the particles (after the lobby, into the same target).
    pub fn draw(&mut self, renderer: &mut gpu::Renderer, target: &mut gpu::VgBuffer) {
        if self.parts.is_empty() {
            return;
        }
        let parts = core::mem::take(&mut self.parts);
        let mut path = core::mem::take(&mut self.path);
        if let Some(pages) = self.pages() {
            renderer.draw_calls(target, pages, |_, emit| {
                for p in &parts {
                    let t = (p.age / p.life).clamp(0.0, 1.0);
                    // Rings and dots grow fast then settle; everything fades out.
                    let ease = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
                    let r = p.r0 + (p.r1 - p.r0) * ease;
                    let a = p.alpha * (1.0 - t) * (1.0 - t * 0.5);
                    if a <= 0.01 || r <= 0.5 {
                        continue;
                    }
                    let (page, px, half) = match p.kind {
                        // The ring's centre line sits at RING_R of the sprite's 32-pixel half.
                        RING => (0u32, RING_PX as f32, r * (RING_PX as f32 * 0.5) / RING_R),
                        DOT => (1, SMALL_PX as f32, r),
                        _ => (2, SMALL_PX as f32, r * (SMALL_PX as f32 * 0.5) / 13.0),
                    };
                    let (c, s) = (cos(p.rot), sin(p.rot));
                    let k = 2.0 * half / px;
                    let (m0, m1, m3, m4) = (k * c, -k * s, k * s, k * c);
                    let h = px * 0.5;
                    let matrix = [m0, m1, p.x - (m0 * h + m1 * h), m3, m4, p.y - (m3 * h + m4 * h)];
                    let corner = |u: f32, v: f32| (matrix[0] * u + matrix[1] * v + matrix[2], matrix[3] * u + matrix[4] * v + matrix[5]);
                    let q = [corner(0.0, 0.0), corner(px, 0.0), corner(px, px), corner(0.0, px)];
                    path.clear();
                    let (mut lo, mut hi) = ([i16::MAX; 2], [i16::MIN; 2]);
                    for (i, &(x, y)) in q.iter().enumerate() {
                        let (x, y) = (path_coord(x), path_coord(y));
                        path.extend_from_slice(&[if i == 0 { PATH_MOVE as i16 } else { PATH_LINE as i16 }, x, y]);
                        lo = [lo[0].min(x), lo[1].min(y)];
                        hi = [hi[0].max(x), hi[1].max(y)];
                    }
                    // CLOSE, END: 14 elements, a whole number of 32-bit words.
                    path.extend_from_slice(&[PATH_CLOSE as i16, 0]);
                    emit(&Call {
                        page,
                        blend: BlendMode::Normal,
                        color: [p.color[0], p.color[1], p.color[2], a],
                        path: &path,
                        bounds: [lo[0], lo[1], hi[0], hi[1]],
                        matrix,
                        rect: None,
                        tag: 0x7FFF_0000 | p.kind as u32,
                    });
                }
                0
            });
        }
        self.parts = parts;
        self.path = path;
    }
}

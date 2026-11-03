//! GPU self-test, run once before the first pack loads.
//!
//! Mirage's GPU is a model of the GC355, and the watch's caches are not modelled at all, so a
//! feature can work on the emulator and not on the watch. Each probe below draws one 4 x 4
//! square of a 128 x 8 BGR565 target with a feature the renderer relies on, and the result is
//! read back through the non-cached PSRAM alias and compared with what the feature must give.
//! Failures switch to a fallback where one exists (FP32 paths, writing GPU buffers through the
//! non-cached alias, BGRA8888 pages, transparent pattern edges) and are reported to the phone.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use bal2d_pack::PageFormat;

use super::fw;
use super::gpu::{self, GpuMem, VgBuffer};

/// Development file in the module's data directory: probes to treat as failed (hex mask).
const FORCE_FILE: &str = "selftest-force";
const W: u32 = 160;
/// Probe cells across the target (8 pixels each).
const CELLS: u32 = W / 8;
const H: u32 = 8;
const BG: [u8; 3] = [0x40, 0x40, 0x40];
/// The probe colour (premultiplied, opaque): 565-exact in every format it is stored in.
const A: [u8; 3] = [0xF8, 0x80, 0x40];

/// Probe bits (`Result::passed`).
pub mod probe {
    pub const FP32: u32 = 1 << 0;
    pub const S16: u32 = 1 << 1;
    pub const LARGE: u32 = 1 << 2;
    pub const ARGB4444: u32 = 1 << 3;
    pub const ARGB8565: u32 = 1 << 4;
    pub const INDEX8: u32 = 1 << 5;
    pub const PAD: u32 = 1 << 6;
    pub const TINT: u32 = 1 << 7;
    pub const ADDITIVE: u32 = 1 << 8;
    pub const MULTIPLY: u32 = 1 << 9;
    pub const SCREEN: u32 = 1 << 10;
    pub const ALPHA: u32 = 1 << 11;
    pub const BLIT: u32 = 1 << 12;
    pub const MIRROR_SMALL: u32 = 1 << 13;
    pub const MIRROR_LARGE: u32 = 1 << 14;
    /// No probe drew outside its square.
    pub const OUTSIDE: u32 = 1 << 15;
    /// An atlas rectangle blitted rotated, scaled, tinted and blended (`Call::rect` draws).
    pub const BLIT_REGION: u32 = 1 << 16;
    /// Two draws batched into one command stream (`gpu::Stream`), each with its own texture
    /// placement and tint: the first, and the second (wrong when the stream's states do not
    /// take).
    pub const STREAM: u32 = 1 << 17;
    pub const STREAM2: u32 = 1 << 18;
    /// The S16 probe again in the target's last cell.
    pub const S16_END: u32 = 1 << 19;
    pub const ALL: u32 = (1 << 20) - 1;
}

/// Summary bits for `Stats::flags` (bits 16..23).
pub mod flag {
    pub const RAN: u32 = 1 << 16;
    /// The target was not in cached PSRAM: nothing could be read back (not verified).
    pub const UNVERIFIED: u32 = 1 << 17;
    pub const FP32_PATHS: u32 = 1 << 18;
    pub const MIRROR: u32 = 1 << 19;
    pub const FORMATS: u32 = 1 << 20;
    pub const NO_PAD: u32 = 1 << 21;
    pub const BLENDS: u32 = 1 << 22;
    pub const OTHER: u32 = 1 << 23;
    /// Batched draws came out wrong: every draw goes through the driver.
    pub const NO_STREAM: u32 = 1 << 24;
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Outcome {
    pub passed: u32,
    pub flags: u32,
    /// The probes right when read back at once after their own GPU wait, a while after all of
    /// them, and through the cache after dropping its lines.
    pub now: u32,
    pub late: u32,
    pub cached: u32,
    /// The first probe a call failed for: (probe index + 1) << 24 | the return code, 0: none.
    pub first_error: u32,
}

struct Tex {
    mem: GpuMem,
    buffer: VgBuffer,
    clut: Option<Box<[u32; 256]>>,
}

fn rgba(c: [u8; 3], a: u8) -> u32 {
    (a as u32) << 24 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32
}

/// A solid `size` x `size` texture of premultiplied `c` with alpha `a`, stored in `format`.
fn texture(format: PageFormat, size: u32, c: [u8; 3], a: u8, mirror: bool) -> Option<Tex> {
    let n = (size * size) as usize;
    let bpp = format.bytes_per_pixel() as usize;
    let mut mem = GpuMem::new(n * bpp)?;
    let px = mem.slice();
    let mut clut = None;
    match format {
        PageFormat::Bgra8888 => {
            for p in px.chunks_exact_mut(4) {
                p.copy_from_slice(&rgba(c, a).to_le_bytes());
            }
        }
        PageFormat::Argb4444 => {
            let v = (a as u16 >> 4) << 12 | (c[0] as u16 >> 4) << 8 | (c[1] as u16 >> 4) << 4 | c[2] as u16 >> 4;
            for p in px.chunks_exact_mut(2) {
                p.copy_from_slice(&v.to_le_bytes());
            }
        }
        PageFormat::Argb8565 => {
            let v = (c[0] as u16 >> 3) << 11 | (c[1] as u16 >> 2) << 5 | c[2] as u16 >> 3;
            for p in px.chunks_exact_mut(3) {
                p[..2].copy_from_slice(&v.to_le_bytes());
                p[2] = a;
            }
        }
        PageFormat::Index8 => {
            px.fill(1);
            let mut t = Box::new([0u32; 256]);
            t[1] = rgba(c, a);
            clut = Some(t);
        }
    }
    mem.clean();
    if mirror {
        mem.mirror();
    }
    let fmt = match format {
        PageFormat::Bgra8888 => gpu::VG_FMT_BGRA8888,
        PageFormat::Argb4444 => gpu::VG_FMT_ARGB4444,
        PageFormat::Argb8565 => gpu::VG_FMT_BGRA5658,
        PageFormat::Index8 => gpu::VG_FMT_INDEX8,
    };
    let mut buffer = VgBuffer::new(&mut mem, size, size, size * bpp as u32, fmt);
    buffer.set_image_mode(0x1F01);
    Some(Tex { mem, buffer, clut })
}

/// One probe square: cell `cell` (8 px wide), pixels [2, 6) x [2, 6).
struct Square {
    path: Box<[u8; 72]>,
    s16: Vec<i16>,
    fp32: Vec<u32>,
    path_matrix: Box<[f32; 12]>,
    image_matrix: Box<[f32; 12]>,
}

impl Square {
    fn new() -> Square {
        let unit = 1.0 / bal2d_render::PATH_SCALE;
        let mut path_matrix = Box::new([0f32; 12]);
        path_matrix[0] = unit;
        path_matrix[4] = unit;
        path_matrix[8] = 1.0;
        path_matrix[9] = unit;
        path_matrix[10] = unit;
        Square { path: Box::new([0u8; 72]), s16: Vec::new(), fp32: Vec::new(), path_matrix, image_matrix: Box::new([0f32; 12]) }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(&mut self, target: &mut VgBuffer, cell: u32, tex: &mut Tex, fp32: bool, shift: f32, blend: u32, pattern: u32, color: u32) -> i32 {
        let s = bal2d_render::PATH_SCALE as i16;
        let (x0, x1) = ((cell * 8 + 2) as i16 * s, (cell * 8 + 6) as i16 * s);
        let (y0, y1) = (2 * s, 6 * s);
        let (mv, ln, cl) = (bal2d_render::PATH_MOVE as i16, bal2d_render::PATH_LINE as i16, bal2d_render::PATH_CLOSE as i16);
        self.s16.clear();
        self.s16.extend_from_slice(&[mv, x0, y0, ln, x1, y0, ln, x1, y1, ln, x0, y1, cl, 0]);
        let p = &mut *self.path;
        p.fill(0);
        for (k, v) in [x0, y0, x1, y1].iter().enumerate() {
            p[k * 4..k * 4 + 4].copy_from_slice(&(*v as f32).to_le_bytes());
        }
        p[16] = 3; // VG_LITE_LOW
        p[50] = 2; // fill path
        if fp32 {
            gpu::path_to_fp32(&self.s16, &mut self.fp32);
            p[17] = gpu::PATH_FP32;
            p[40..44].copy_from_slice(&((self.fp32.len() * 4) as u32).to_le_bytes());
            p[44..48].copy_from_slice(&(self.fp32.as_ptr() as usize as u32).to_le_bytes());
        } else {
            p[17] = gpu::PATH_S16;
            p[40..44].copy_from_slice(&((self.s16.len() * 2) as u32).to_le_bytes());
            p[44..48].copy_from_slice(&(self.s16.as_ptr() as usize as u32).to_le_bytes());
        }
        p[48] = 1;
        // Image space -> target: the texture's texel (u, v) lands on (u + cell * 8 - shift, v).
        *self.image_matrix = [1.0, 0.0, (cell * 8) as f32 - shift, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
        if let Some(c) = &tex.clut {
            fw::vg_set_clut(256, c.as_ptr());
        }
        fw::vg_draw_pattern(
            target.ptr(),
            p.as_mut_ptr(),
            0x1901,
            self.path_matrix.as_ptr(),
            tex.buffer.ptr(),
            self.image_matrix.as_ptr(),
            blend,
            pattern,
            0,
            color,
            0x2000,
        )
    }
}

/// Target pixel (x, y) at `base` (the non-cached alias, or the cached one) as stored.
fn raw(base: usize, x: u32, y: u32) -> u16 {
    unsafe { core::ptr::read_volatile((base + (y * W * 2 + x * 2) as usize) as *const u16) }
}

/// Target pixel (x, y) as 8-bit channels (565 expanded).
fn pixel(base: usize, x: u32, y: u32) -> [i32; 3] {
    let v = raw(base, x, y) as i32;
    [(v >> 11) << 3, ((v >> 5) & 63) << 2, (v & 31) << 3]
}

fn near(got: [i32; 3], want: [i32; 3], tol: i32) -> bool {
    // tol in 8-bit units; 565 steps are 8 / 4 / 8.
    (0..3).all(|k| (got[k] - want[k]).abs() <= tol + if k == 1 { 4 } else { 8 })
}

fn exp(c: [u8; 3]) -> [i32; 3] {
    // What 565 keeps of an 8-bit colour.
    [(c[0] as i32 >> 3) << 3, (c[1] as i32 >> 2) << 2, (c[2] as i32 >> 3) << 3]
}

/// Runs the probes; applies the fallbacks to `renderer` and the upload paths.
pub fn run(renderer: &mut gpu::Renderer) -> Outcome {
    let mut out = Outcome { flags: flag::RAN, ..Default::default() };
    let Some(mut tmem) = GpuMem::new((W * H * 2) as usize) else {
        out.flags |= flag::UNVERIFIED;
        return out;
    };
    tmem.clean();
    let Some(nc) = gpu::uncached(tmem.ptr() as usize) else {
        out.flags |= flag::UNVERIFIED;
        return out;
    };
    let mut target = VgBuffer::new(&mut tmem, W, H, W * 2, gpu::VG_FMT_BGR565);
    let mut sq = Square::new();
    let bg = 0xFF00_0000 | (BG[2] as u32) << 16 | (BG[1] as u32) << 8 | BG[0] as u32;
    let white = 0xFFFF_FFFFu32;
    fw::vg_set_scissor(0, 0, W as i32, H as i32);
    fw::vg_enable_scissor();
    // Every probe is submitted and waited for on its own and read back at once: one that the
    // GPU or driver chokes on cannot take the later ones with it, and each return code is kept.
    let clear_rc = fw::vg_clear(target.ptr(), core::ptr::null(), bg);
    let clear_finish = fw::vg_finish();
    let clear_ok = (0..CELLS).all(|c| near(pixel(nc, c * 8 + 4, 4), exp(BG), 0));
    let mut wants: Vec<(u32, u32, [i32; 3], i32)> = Vec::new(); // (probe, cell, colour, tolerance)
    let mut log: Vec<(u32, i32, i32, u16, u16)> = Vec::new(); // (probe, draw rc, finish rc, pixels)
    let mut now = 0u32;
    let mut step = |wants: &mut Vec<(u32, u32, [i32; 3], i32)>, bit: u32, cell: u32, want: [i32; 3], tol: i32, draw_rc: i32| {
        let t0 = super::lv::now_ms();
        let mut finish_rc = fw::vg_finish();
        // The driver's timeout: it reset the GPU and says nothing (VG_LITE_TIMEOUT stands in).
        if finish_rc == 0 && super::lv::now_ms().wrapping_sub(t0) >= gpu::TIMEOUT_MS {
            finish_rc = 4;
        }
        if near(pixel(nc, cell * 8 + 4, 4), want, tol) && near(pixel(nc, cell * 8 + 3, 3), want, tol) {
            now |= bit;
        }
        log.push((bit, draw_rc, finish_rc, raw(nc, cell * 8 + 4, 4), raw(nc, cell * 8 + 3, 3)));
        wants.push((bit, cell, want, tol));
    };
    // The large textures first: cleaning one (over 0x1FE0 bytes) cleans the whole cache, which
    // would also publish small textures made before it and hide a failing per-line clean.
    let (large, mlarge) = (texture(PageFormat::Bgra8888, 64, A, 0xFF, false), texture(PageFormat::Bgra8888, 64, A, 0xFF, true));
    let textures = (
        texture(PageFormat::Bgra8888, 16, A, 0xFF, false),
        large,
        texture(PageFormat::Argb4444, 16, A, 0xFF, false),
        texture(PageFormat::Argb8565, 16, A, 0xFF, false),
        texture(PageFormat::Index8, 16, A, 0xFF, false),
        texture(PageFormat::Bgra8888, 16, [0x80, 0x80, 0x80], 0xFF, false),
        texture(PageFormat::Bgra8888, 16, [0x7C, 0x40, 0x20], 0x80, false),
        texture(PageFormat::Bgra8888, 16, A, 0xFF, true),
        mlarge,
    );
    let (Some(mut small), Some(mut large), Some(mut t4444), Some(mut t8565), Some(mut ti8), Some(mut gray), Some(mut half), Some(mut msmall), Some(mut mlarge)) = textures else {
        out.flags |= flag::UNVERIFIED;
        return out;
    };
    let src_over = 1;
    let pad = 0x1D01;
    let a4 = [0xFF, 0x88, 0x44];
    let rc;
    let r = sq.draw(&mut target, 0, &mut small, true, 0.0, src_over, pad, white);
    step(&mut wants, probe::FP32, 0, exp(A), 0, r);
    let r = sq.draw(&mut target, 1, &mut small, false, 0.0, src_over, pad, white);
    step(&mut wants, probe::S16, 1, exp(A), 0, r);
    let r = sq.draw(&mut target, 2, &mut large, false, 0.0, src_over, pad, white);
    step(&mut wants, probe::LARGE, 2, exp(A), 0, r);
    let r = sq.draw(&mut target, 3, &mut t4444, false, 0.0, src_over, pad, white);
    // 4-bit channels widened by replication (0x8 -> 0x88) or by shifting (0x80): either is right.
    step(&mut wants, probe::ARGB4444, 3, exp(a4), 4, r);
    let r = sq.draw(&mut target, 4, &mut t8565, false, 0.0, src_over, pad, white);
    step(&mut wants, probe::ARGB8565, 4, exp(A), 0, r);
    let r = sq.draw(&mut target, 5, &mut ti8, false, 0.0, src_over, pad, white);
    step(&mut wants, probe::INDEX8, 5, exp(A), 0, r);
    // The square samples texels 42..46 of a 16-texel image: PAD gives the edge texel.
    let r = sq.draw(&mut target, 6, &mut small, false, 40.0, src_over, pad, white);
    step(&mut wants, probe::PAD, 6, exp(A), 0, r);
    // Tint (1, 0.5, 0.25) in multiply image mode: ABGR 0xFF 40 80 FF.
    let r = sq.draw(&mut target, 7, &mut small, false, 0.0, src_over, pad, 0xFF40_80FF);
    step(&mut wants, probe::TINT, 7, exp([0xF8, 0x40, 0x10]), 6, r);
    let r = sq.draw(&mut target, 8, &mut gray, false, 0.0, 9, pad, white);
    step(&mut wants, probe::ADDITIVE, 8, exp([0xC0, 0xC0, 0xC0]), 6, r);
    let r = sq.draw(&mut target, 9, &mut gray, false, 0.0, 5, pad, white);
    step(&mut wants, probe::MULTIPLY, 9, exp([0x20, 0x20, 0x20]), 6, r);
    let r = sq.draw(&mut target, 10, &mut gray, false, 0.0, 6, pad, white);
    step(&mut wants, probe::SCREEN, 10, exp([0xA0, 0xA0, 0xA0]), 6, r);
    let r = sq.draw(&mut target, 11, &mut half, false, 0.0, src_over, pad, white);
    step(&mut wants, probe::ALPHA, 11, exp([0x9C, 0x60, 0x40]), 6, r);
    // The plain S16 draw again at the far end of the target: wrong only there means the
    // results depend on where in the target they are.
    let r = sq.draw(&mut target, CELLS - 1, &mut small, false, 0.0, src_over, pad, white);
    step(&mut wants, probe::S16_END, CELLS - 1, exp(A), 0, r);
    // The bake path: a 4 x 4 BGR565 blit, no blending.
    let blit_c = (10u16 << 11) | (20 << 5) | 30;
    if let Some(mut bm) = GpuMem::new(16 * 16 * 2) {
        for p in bm.slice().chunks_exact_mut(2) {
            p.copy_from_slice(&blit_c.to_le_bytes());
        }
        bm.clean();
        let mut bb = VgBuffer::new(&mut bm, 16, 16, 32, gpu::VG_FMT_BGR565);
        let rect = [0i32, 0, 4, 4];
        let m = [1.0f32, 0.0, (12 * 8 + 2) as f32, 0.0, 1.0, 2.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
        let r = fw::vg_blit_rect(target.ptr(), bb.ptr(), rect.as_ptr(), m.as_ptr(), 0, 0, 0);
        step(&mut wants, probe::BLIT, 12, [10 << 3, 20 << 2, 30 << 3], 0, r);
        // An atlas region as `Call::rect` draws it: texels [4, 12) x [4, 12) of a 16 x 16 image
        // (the probe colour, blue around it) turned a quarter, halved and tinted onto cell 15's
        // square. The blit's matrix maps rectangle-relative texels (the rectangle is the image
        // for the GPU): (u', v') -> (126 - v' / 2, 2 + u' / 2). Blue anywhere, or the square
        // misplaced, means the driver or GPU does not keep to the rectangle that way.
        let region = GpuMem::new(16 * 16 * 4).map(|mut mem| {
            for (i, p) in mem.slice().chunks_exact_mut(4).enumerate() {
                let (u, v) = (i % 16, i / 16);
                let inside = (4..12).contains(&u) && (4..12).contains(&v);
                p.copy_from_slice(&if inside { rgba(A, 0xFF) } else { 0xFF00_00FF }.to_le_bytes());
            }
            mem.clean();
            let mut buffer = VgBuffer::new(&mut mem, 16, 16, 64, gpu::VG_FMT_BGRA8888);
            buffer.set_image_mode(0x1F01);
            (mem, buffer)
        });
        let mut region = region;
        if let Some((_, rb)) = region.as_mut() {
            let rect = [4i32, 4, 8, 8];
            let m = [0.0f32, -0.5, 126.0, 0.5, 0.0, 2.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
            let r = fw::vg_blit_rect(target.ptr(), rb.ptr(), rect.as_ptr(), m.as_ptr(), src_over, 0xFF40_80FF, 0x2000);
            step(&mut wants, probe::BLIT_REGION, 15, exp([0xF8, 0x40, 0x10]), 6, r);
        }
        // Two draws in one command stream, carried by one driver call: cells 16 and 17 sample
        // the region texture's inside (texels 6..10) through their own matrices; the second
        // with the first's would sample texels 14..18 (blue, the edge).
        let mut stream = gpu::Stream::new(1024);
        if let (Some(st), Some((_, rb))) = (stream.as_mut(), region.as_mut()) {
            let s = bal2d_render::PATH_SCALE as i16;
            let (mv, ln, cl) = (bal2d_render::PATH_MOVE as i16, bal2d_render::PATH_LINE as i16, bal2d_render::PATH_CLOSE as i16);
            for (cell, color) in [(16u32, 0xFF40_80FFu32), (17, white)] {
                let (x0, x1, y0, y1) = ((cell * 8 + 2) as i16, (cell * 8 + 6) as i16, 2i16, 6i16);
                let path = [mv, x0 * s, y0 * s, ln, x1 * s, y0 * s, ln, x1 * s, y1 * s, ln, x0 * s, y1 * s, cl, 0];
                let m = [1.0f32, 0.0, (cell * 8 - 4) as f32, 0.0, 1.0, -4.0];
                if let Some(g) = gpu::generator(&m, 16.0, 16.0) {
                    st.draw(0, bal2d_spine::BlendMode::Normal, &m, &g, color, [x0 as i32 - 2, 0, 8, 8], [x0 * s, y0 * s, x1 * s, y1 * s], &path);
                }
            }
            if let Some((path, _, _, m, color)) = st.close() {
                *sq.image_matrix = [m[0], m[1], m[2], m[3], m[4], m[5], 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
                let r = fw::vg_draw_pattern(target.ptr(), path, 0x1901, sq.path_matrix.as_ptr(), rb.ptr(), sq.image_matrix.as_ptr(), src_over, pad, 0, color, 0x2000);
                step(&mut wants, probe::STREAM, 16, exp([0xF8, 0x40, 0x10]), 6, r);
                step(&mut wants, probe::STREAM2, 17, exp(A), 0, 0);
            }
        }
        // Kept alive until the GPU is done.
        let r = sq.draw(&mut target, 13, &mut msmall, false, 0.0, src_over, pad, white);
        step(&mut wants, probe::MIRROR_SMALL, 13, exp(A), 0, r);
        let r = sq.draw(&mut target, 14, &mut mlarge, false, 0.0, src_over, pad, white);
        step(&mut wants, probe::MIRROR_LARGE, 14, exp(A), 0, r);
        rc = fw::vg_finish();
        drop(stream);
        drop(region);
        drop(bb);
        drop(bm);
    } else {
        rc = fw::vg_finish();
    }
    // The same pixels again a while later, and through the cache after dropping its lines: a
    // probe right only then says the read came too early or the GPU's writes sit somewhere the
    // non-cached alias does not see.
    let t0 = super::lv::now_ms();
    while super::lv::now_ms().wrapping_sub(t0) < 30 {}
    let judge = |px: &dyn Fn(u32, u32) -> [i32; 3]| {
        let mut m = 0u32;
        for (bit, cell, want, tol) in &wants {
            if near(px(cell * 8 + 4, 4), *want, *tol) && near(px(cell * 8 + 3, 3), *want, *tol) {
                m |= bit;
            }
        }
        let bgx = exp(BG);
        if (0..CELLS).all(|c| near(px(c * 8, 0), bgx, 0) && near(px(c * 8 + 7, 7), bgx, 0) && near(px(c * 8 + 6, 4), bgx, 0)) {
            m |= probe::OUTSIDE;
        }
        m
    };
    let late = judge(&|x, y| pixel(nc, x, y));
    fw::cache_flush_invalidate(tmem.ptr(), (W * H * 2) as usize);
    let cached_base = tmem.ptr() as usize;
    let cached = judge(&|x, y| pixel(cached_base, x, y));
    // A probe counts when any read found its colour (nothing else puts it there).
    let mut passed = late | cached | (now & !probe::OUTSIDE);
    out.now = now;
    out.late = late;
    out.cached = cached;
    out.first_error = log.iter().position(|e| e.1 != 0 || e.2 != 0).map_or(0, |i| {
        let e = log[i];
        (e.0.trailing_zeros() + 1) << 24 | ((if e.1 != 0 { e.1 } else { e.2 }) as u32 & 0xFFFF)
    });
    super::trace(&alloc::format!(
        "gpu self-test: target {:#x} (nc {nc:#x}); clear: rc {clear_rc}/{clear_finish} ok {clear_ok}; passed now {now:#07x} late {late:#07x} cached {cached:#07x}",
        tmem.ptr() as usize
    ));
    for chunk in log.chunks(4) {
        let mut line = alloc::string::String::from("gpu probes:");
        for (bit, draw_rc, finish_rc, a, b) in chunk {
            line += &alloc::format!(" [{} rc {draw_rc}/{finish_rc} px {a:04x} {b:04x}]", bit.trailing_zeros());
        }
        super::trace(&line);
    }
    // Outside pixels of the first cells that are not background (what is there instead).
    let mut bad = alloc::string::String::new();
    for c in 0..CELLS {
        for (x, y) in [(c * 8, 0), (c * 8 + 7, 7), (c * 8 + 6, 4)] {
            let v = raw(nc, x, y);
            if !near(pixel(nc, x, y), exp(BG), 0) && bad.len() < 200 {
                bad += &alloc::format!(" ({x},{y})={v:04x}");
            }
        }
    }
    if !bad.is_empty() {
        super::trace(&alloc::format!("gpu self-test: not background:{bad}"));
    }
    passed &= probe::ALL;
    out.passed = passed;
    super::trace(&alloc::format!("gpu self-test: passed {passed:#07x} of {:#07x}, rc {rc}", probe::ALL));
    // Development: `selftest-force` (text, a hex probe mask) marks those probes failed, so the
    // fallbacks can be tried where everything passes (Mirage).
    if let Some(text) = super::host::Fs::read(FORCE_FILE, 16) {
        let forced = core::str::from_utf8(&text).ok().and_then(|t| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok()).unwrap_or(0);
        passed &= !forced;
        out.passed = passed;
        super::trace(&alloc::format!("gpu self-test: forced failures {forced:#06x}"));
    }
    // Fallbacks.
    let coherent_small = passed & (probe::S16 | probe::FP32) != 0;
    if (!coherent_small && passed & probe::MIRROR_SMALL != 0) || (passed & probe::LARGE == 0 && passed & probe::MIRROR_LARGE != 0) {
        gpu::MIRROR_NC.store(true, Ordering::Relaxed);
        out.flags |= flag::MIRROR;
    }
    // Region blits stay off: in the timing model they saved nothing (a blit costs the driver
    // what a path does), and the first real watch reported its draw results off. The probe still
    // runs and reports.
    let _ = &renderer.paths.blits;
    // Batched draws need both stream probes right (and their textures were cleaned, not
    // mirrored, like the S16 probe's).
    if passed & (probe::STREAM | probe::STREAM2) != probe::STREAM | probe::STREAM2 || gpu::MIRROR_NC.load(Ordering::Relaxed) {
        gpu::STREAM_OFF.store(true, Ordering::Relaxed);
        out.flags |= flag::NO_STREAM;
    }
    let s16_ok = passed & probe::S16 != 0 || (passed & probe::MIRROR_SMALL != 0);
    if !s16_ok && passed & probe::FP32 != 0 {
        renderer.use_fp32_paths();
        out.flags |= flag::FP32_PATHS;
    }
    // Formats, edges and blends are only judged when the plain BGRA8888 probe drew right: their
    // probes use S16 paths and cleaned (not mirrored) textures, like that one.
    if passed & probe::S16 != 0 {
        let mut bad = 0u32;
        for (bit, fmt) in [(probe::ARGB4444, PageFormat::Argb4444), (probe::ARGB8565, PageFormat::Argb8565), (probe::INDEX8, PageFormat::Index8)] {
            if passed & bit == 0 {
                bad |= 1 << fmt as u32;
            }
        }
        if bad != 0 {
            gpu::BAD_FORMATS.store(bad, Ordering::Relaxed);
            out.flags |= flag::FORMATS;
        }
        if passed & probe::PAD == 0 {
            renderer.use_pattern_color();
            out.flags |= flag::NO_PAD;
        }
        if passed & (probe::ADDITIVE | probe::MULTIPLY | probe::SCREEN | probe::ALPHA) != probe::ADDITIVE | probe::MULTIPLY | probe::SCREEN | probe::ALPHA {
            out.flags |= flag::BLENDS;
        }
        if passed & (probe::TINT | probe::BLIT | probe::OUTSIDE) != probe::TINT | probe::BLIT | probe::OUTSIDE {
            out.flags |= flag::OTHER;
        }
    } else {
        out.flags |= flag::OTHER;
    }
    drop(target);
    drop(tmem);
    out
}

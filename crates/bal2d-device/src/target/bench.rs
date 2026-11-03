//! How fast the watch's GPU fills, measured once at start (before a pack loads).
//!
//! The driver gives each submit 100 ms and then resets the GPU, dropping the rest of it; the
//! fifth real-watch log showed timeouts even with every feature off whenever the animation got
//! heavy, so the work per submit has to follow the GPU's real speed, which Mirage does not
//! model. Each kind of draw a frame is made of covers one screen (336 x 480) of a PSRAM target,
//! three times, each waited for on its own: milliseconds per screen. `split_px` turns the
//! slowest textured kind into the pixels a submit may carry.

use alloc::boxed::Box;

use super::fw;
use super::gpu::{self, GpuMem, VgBuffer};

const W: u32 = 336;
const H: u32 = 480;
const TEX: u32 = 256;
const RUNS: u32 = 3;
/// GPU time one submit may take (a quarter of the driver's limit).
const SUBMIT_MS: u32 = 25;

/// Milliseconds per screen, in tenths (0: not measured).
#[derive(Clone, Copy, Default, Debug)]
pub struct Bench {
    /// `vg_lite_clear` (plain fill).
    pub clear: u32,
    /// A rotated, scaled BGRA8888 texture blended over the screen: through the cached alias,
    /// and through the non-cached one.
    pub tex32: u32,
    pub tex32_nc: u32,
    /// The same with ARGB8565.
    pub tex24: u32,
    pub tex24_nc: u32,
    /// The background blit (BGR565 copied, no blending).
    pub blit: u32,
    /// A wait that ran into the driver's timeout.
    pub timeouts: u32,
}

impl Bench {
    /// Pixels a submit may carry: `SUBMIT_MS` of the slowest textured draw, between an eighth
    /// of a screen and a whole one.
    pub fn split_px(&self) -> u32 {
        let slowest = [self.tex32, self.tex32_nc, self.tex24, self.tex24_nc].into_iter().max().unwrap_or(0);
        let screen = W * H;
        if slowest == 0 {
            return screen / 2;
        }
        (screen as u64 * SUBMIT_MS as u64 * 10 / slowest as u64).clamp(screen as u64 / 8, screen as u64) as u32
    }
}

/// A screen-sized rectangle path (S16, `bal2d_render::PATH_SCALE` units) and its vg_lite_path_t.
struct Rect {
    data: Box<[i16; 14]>,
    path: Box<[u8; 72]>,
}

impl Rect {
    fn new() -> Rect {
        let s = bal2d_render::PATH_SCALE as i16;
        let (mv, ln, cl) = (bal2d_render::PATH_MOVE as i16, bal2d_render::PATH_LINE as i16, bal2d_render::PATH_CLOSE as i16);
        let (w, h) = (W as i16 * s, H as i16 * s);
        let data = Box::new([mv, 0, 0, ln, w, 0, ln, w, h, ln, 0, h, cl, 0]);
        let mut path = Box::new([0u8; 72]);
        for (k, v) in [0f32, 0.0, w as f32, h as f32].iter().enumerate() {
            path[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        path[16] = 3; // VG_LITE_LOW
        path[17] = gpu::PATH_S16;
        path[40..44].copy_from_slice(&28u32.to_le_bytes());
        path[44..48].copy_from_slice(&(data.as_ptr() as usize as u32).to_le_bytes());
        path[50] = 2; // fill
        Rect { data, path }
    }
}

/// A texture of `TEX` x `TEX` in `format` (`bpp` bytes per texel) with some structure in it.
fn texture(format: u16, bpp: u32) -> Option<(GpuMem, VgBuffer)> {
    let mut mem = GpuMem::new((TEX * TEX * bpp) as usize)?;
    for (i, b) in mem.slice().iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2_654_435_761).rotate_left(7) as u8 | 0x80;
    }
    mem.clean();
    let mut buffer = VgBuffer::new(&mut mem, TEX, TEX, TEX * bpp, format);
    buffer.set_image_mode(0x1F01);
    Some((mem, buffer))
}

/// Runs the measurements (about a second at most). `None` without memory for them.
pub fn run() -> Option<Bench> {
    let was = gpu::UNCACHED.load(core::sync::atomic::Ordering::Relaxed);
    // The target: PSRAM through the non-cached alias (direct frames go to SRAM, faster still).
    gpu::UNCACHED.store(true, core::sync::atomic::Ordering::Relaxed);
    let mut tmem = GpuMem::new((W * H * 2) as usize)?;
    tmem.clean();
    let mut target = VgBuffer::new(&mut tmem, W, H, W * 2, gpu::VG_FMT_BGR565);
    gpu::UNCACHED.store(false, core::sync::atomic::Ordering::Relaxed);
    let mut t32 = texture(gpu::VG_FMT_BGRA8888, 4)?;
    let mut t24 = texture(gpu::VG_FMT_BGRA5658, 3)?;
    let mut src = GpuMem::new((W * H * 2) as usize)?;
    src.clean();
    let mut blit_src = VgBuffer::new(&mut src, W, H, W * 2, gpu::VG_FMT_BGR565);
    let mut rect = Rect::new();
    let unit = 1.0 / bal2d_render::PATH_SCALE;
    let path_matrix: Box<[f32; 12]> = Box::new([unit, 0.0, 0.0, 0.0, unit, 0.0, 0.0, 0.0, 1.0, unit, unit, 0.0]);
    // Texture -> screen: scaled 2.4 and turned 15 degrees about the screen's centre, so the
    // texture covers all of it (as the water and light layers do), sampled bilinearly.
    let (c, s) = (0.966f32 * 2.4, 0.259f32 * 2.4);
    let (tx, ty) = (168.0 - (c * 128.0 - s * 128.0), 240.0 - (s * 128.0 + c * 128.0));
    let image_matrix: Box<[f32; 12]> = Box::new([c, -s, tx, s, c, ty, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0]);
    fw::vg_set_scissor(0, 0, W as i32, H as i32);
    fw::vg_enable_scissor();
    fw::vg_finish();
    let mut timeouts = 0;
    let mut time = |draw: &mut dyn FnMut()| -> u32 {
        let mut total = 0;
        for _ in 0..RUNS {
            let t0 = super::lv::now_ms();
            draw();
            fw::vg_finish();
            let dt = super::lv::now_ms().wrapping_sub(t0);
            if dt >= gpu::TIMEOUT_MS {
                timeouts += 1;
            }
            total += dt;
        }
        (total * 10 / RUNS).max(1)
    };
    let tp = target.ptr();
    let path = rect.path.as_mut_ptr();
    let pattern = |tex: *mut u8| {
        fw::vg_draw_pattern(tp, path, 0x1901, path_matrix.as_ptr(), tex, image_matrix.as_ptr(), 1, gpu::PATTERN_COLOR, 0, 0xFFFF_FFFF, 0x2000);
    };
    let clear = time(&mut || {
        fw::vg_clear(tp, core::ptr::null(), 0xFF20_4060);
    });
    let (p32, p24) = (t32.1.ptr(), t24.1.ptr());
    let tex32 = time(&mut || pattern(p32));
    let tex24 = time(&mut || pattern(p24));
    gpu::UNCACHED.store(true, core::sync::atomic::Ordering::Relaxed);
    t32.1.readdress();
    t24.1.readdress();
    let tex32_nc = time(&mut || pattern(p32));
    let tex24_nc = time(&mut || pattern(p24));
    gpu::UNCACHED.store(false, core::sync::atomic::Ordering::Relaxed);
    let full = [0i32, 0, W as i32, H as i32];
    let id: [f32; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
    let bp = blit_src.ptr();
    let blit = time(&mut || {
        fw::vg_blit_rect(tp, bp, full.as_ptr(), id.as_ptr(), 0, 0, 0);
    });
    gpu::UNCACHED.store(was, core::sync::atomic::Ordering::Relaxed);
    let _ = (&rect.data, &tmem, &t32.0, &t24.0, &src);
    Some(Bench { clear, tex32, tex32_nc, tex24, tex24_nc, blit, timeouts })
}

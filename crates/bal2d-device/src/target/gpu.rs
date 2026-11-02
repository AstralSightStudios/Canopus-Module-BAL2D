//! VGLite rendering of a draw list.
//!
//! Struct layouts are the firmware's VGLite 4.0.47 (docs/re/firmware-p0.md §2.1). Every
//! triangle is a filled path in screen space, painted with its atlas page through the affine
//! matrix that maps the triangle's texels onto its screen corners. Pages are premultiplied, so
//! SRC_OVER / ADDITIVE / MULTIPLY / SCREEN give Spine's PMA blend modes; the slot tint is the
//! paint color in multiply image mode.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use bal2d_pack::PageFormat;
use bal2d_render::DrawList;
use bal2d_spine::BlendMode;

use super::fw;

pub const VG_FMT_BGR565: u16 = 5;
pub const VG_FMT_BGRA8888: u16 = 1;
/// VGLite names components from the least significant bits: our 16-bit pixels (A high, B low)
/// are its BGRA4444, the format LVGL maps its own ARGB4444 to.
pub const VG_FMT_ARGB4444: u16 = 7;
pub const VG_FMT_INDEX8: u16 = 103;
/// 565 colour then 8-bit alpha (LVGL's ARGB8565).
pub const VG_FMT_BGRA5658: u16 = 121;

const BLEND_NONE: u32 = 0;
const BLEND_SRC_OVER: u32 = 1;
const BLEND_MULTIPLY: u32 = 5;
const BLEND_SCREEN: u32 = 6;
const BLEND_ADDITIVE: u32 = 9;
const FILTER_POINT: u32 = 0;
const FILTER_BILINEAR: u32 = 0x2000;
const FILL_NON_ZERO: u32 = 0x1901;
const PATTERN_PAD: u32 = 0x1D01;
const IMAGE_MODE_MULTIPLY: u16 = 0x1F01;
/// `vg_lite_format_t` of the paths: S16 (coordinates in 1/`bal2d_render::PATH_SCALE` pixels),
/// or FP32 (the same units) when the self-test finds S16 paths drawn wrong.
pub const PATH_S16: u8 = 1;
pub const PATH_FP32: u8 = 3;
/// `VG_LITE_PATTERN_COLOR`: outside the image the pattern colour (0, transparent), the fallback
/// when the self-test finds PAD not honoured.
pub const PATTERN_COLOR: u32 = 0x1D00;

/// Set by the self-test when the cache clean did not reach memory: buffers are also written
/// through the non-cached PSRAM alias.
pub static MIRROR_NC: AtomicBool = AtomicBool::new(false);
/// Page formats the self-test found drawn wrong (bit = `PageFormat as u8`): converted to
/// BGRA8888 on upload.
pub static BAD_FORMATS: AtomicU32 = AtomicU32::new(0);

/// The non-cached alias of a cached PSRAM address (0x3C.. -> 0x38..), when it is one.
pub fn uncached(addr: usize) -> Option<usize> {
    (0x3C00_0000..0x3E00_0000).contains(&addr).then_some(addr - 0x0400_0000)
}
/// Draw calls between submits (`vg_lite_flush`) within a frame.
const FLUSH_EVERY: u32 = 48;
/// `VG_LITE_LOW`: no anti-aliasing, so the triangles of a mesh meet without seams.
const QUALITY_LOW: u8 = 3;

/// A 64-byte aligned GPU-visible allocation.
pub struct GpuMem {
    raw: Vec<u8>,
    offset: usize,
    pub len: usize,
}

impl GpuMem {
    pub fn new(len: usize) -> Option<GpuMem> {
        let mut raw = Vec::new();
        raw.try_reserve_exact(len + 64).ok()?;
        raw.resize(len + 64, 0);
        let offset = (64 - (raw.as_ptr() as usize & 63)) & 63;
        Some(GpuMem { raw, offset, len })
    }

    /// Like `new`, with `prefix` extra bytes in front of the aligned region (a section header
    /// the data is inflated together with).
    pub fn with_prefix(len: usize, prefix: usize) -> Option<GpuMem> {
        let mut raw = Vec::new();
        raw.try_reserve_exact(len + prefix + 64).ok()?;
        raw.resize(len + prefix + 64, 0);
        let base = raw.as_ptr() as usize + prefix;
        let offset = ((64 - (base & 63)) & 63) + prefix;
        Some(GpuMem { raw, offset, len })
    }

    /// The aligned region with `prefix` bytes in front of it.
    pub fn with_front(&mut self, prefix: usize) -> &mut [u8] {
        let o = self.offset;
        &mut self.raw[o - prefix..o + self.len]
    }

    pub fn ptr(&mut self) -> *mut u8 {
        unsafe { self.raw.as_mut_ptr().add(self.offset) }
    }

    pub fn slice(&mut self) -> &mut [u8] {
        let o = self.offset;
        &mut self.raw[o..o + self.len]
    }

    /// Publishes CPU writes to the GPU.
    pub fn clean(&mut self) {
        let (p, n) = (self.ptr(), self.len);
        fw::cache_clean(p, n);
        if MIRROR_NC.load(Ordering::Relaxed) {
            self.mirror();
        }
    }

    /// Copies the region to memory through the non-cached alias. The cached lines hold the same
    /// bytes, so evicting them later changes nothing.
    pub fn mirror(&mut self) {
        let (p, n) = (self.ptr() as usize, self.len);
        let Some(nc) = uncached(p) else { return };
        unsafe {
            let (src, dst) = (p as *const u32, nc as *mut u32);
            for i in 0..n / 4 {
                core::ptr::write_volatile(dst.add(i), core::ptr::read(src.add(i)));
            }
            core::arch::asm!("dsb sy");
        }
    }
}

/// `vg_lite_buffer_t` (176 bytes) over a `GpuMem`.
pub struct VgBuffer {
    pub raw: Box<[u8; 176]>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

impl VgBuffer {
    pub fn new(mem: &mut GpuMem, width: u32, height: u32, stride: u32, format: u16) -> VgBuffer {
        let mut raw = Box::new([0u8; 176]);
        raw[0..4].copy_from_slice(&(width as i32).to_le_bytes());
        raw[4..8].copy_from_slice(&(height as i32).to_le_bytes());
        raw[8..12].copy_from_slice(&(stride as i32).to_le_bytes());
        raw[14..16].copy_from_slice(&format.to_le_bytes());
        let p = mem.ptr() as usize as u32;
        raw[20..24].copy_from_slice(&p.to_le_bytes());
        raw[24..28].copy_from_slice(&p.to_le_bytes());
        // Normal image mode until set; opaque transparency mode 0.
        raw[80..82].copy_from_slice(&0x1F00u16.to_le_bytes());
        VgBuffer { raw, width, height, stride }
    }

    pub fn set_image_mode(&mut self, mode: u16) {
        self.raw[80..82].copy_from_slice(&mode.to_le_bytes());
    }

    pub fn ptr(&mut self) -> *mut u8 {
        self.raw.as_mut_ptr()
    }
}

/// An uploaded atlas page.
pub struct Page {
    pub mem: GpuMem,
    pub buffer: VgBuffer,
    /// INDEX8 palette (ARGB8888, premultiplied).
    pub clut: Option<Box<[u32; 256]>>,
}

impl Page {
    /// Inflates a pack page straight into GPU memory (the compiler pads page widths to 16
    /// pixels, so rows need no restriding).
    pub fn from_pack(src: &mut dyn bal2d_pack::Source, page: &bal2d_core::Page) -> Result<Page, i32> {
        let h = page.header;
        let (width, height) = (h.width as u32, h.height as u32);
        let bpp = h.format.bytes_per_pixel() as u32;
        let stride = width * bpp;
        if width % 16 != 0 {
            return Err(-22);
        }
        let prefix = page.prefix();
        let pixels = page.section_len().checked_sub(prefix).ok_or(-22)?;
        if pixels != (stride * height) as usize {
            return Err(-22);
        }
        let mut mem = GpuMem::with_prefix(pixels, prefix).ok_or(-12)?;
        bal2d_core::read_page(src, page, mem.with_front(prefix)).map_err(|_| -84)?;
        let clut = if h.format == PageFormat::Index8 {
            let front = mem.with_front(prefix);
            let mut c = Box::new([0u32; 256]);
            for (i, e) in front[8..1032].chunks_exact(4).enumerate() {
                c[i] = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
            }
            Some(c)
        } else {
            None
        };
        // A format the self-test found drawn wrong is widened to BGRA8888, when the heap has room
        // for it (up to four times the size); otherwise the page keeps its format: slightly off
        // colours are better than no character.
        if h.format != PageFormat::Bgra8888 && BAD_FORMATS.load(Ordering::Relaxed) & (1 << h.format as u32) != 0 {
            if let Some(mut wide) = GpuMem::new((width * height * 4) as usize) {
                to_bgra8888(h.format, mem.slice(), clut.as_deref(), wide.slice());
                drop(mem);
                wide.clean();
                let mut buffer = VgBuffer::new(&mut wide, width, height, width * 4, VG_FMT_BGRA8888);
                buffer.set_image_mode(IMAGE_MODE_MULTIPLY);
                return Ok(Page { mem: wide, buffer, clut: None });
            }
        }
        mem.clean();
        let fmt = match h.format {
            PageFormat::Bgra8888 => VG_FMT_BGRA8888,
            PageFormat::Argb4444 => VG_FMT_ARGB4444,
            PageFormat::Index8 => VG_FMT_INDEX8,
            PageFormat::Argb8565 => VG_FMT_BGRA5658,
        };
        let mut buffer = VgBuffer::new(&mut mem, width, height, stride, fmt);
        buffer.set_image_mode(IMAGE_MODE_MULTIPLY);
        Ok(Page { mem, buffer, clut })
    }
}

/// Expands page pixels to BGRA8888 (premultiplied, as the page is).
pub fn to_bgra8888(format: PageFormat, src: &[u8], clut: Option<&[u32; 256]>, dst: &mut [u8]) {
    let px = dst.len() / 4;
    for i in 0..px {
        let v: u32 = match format {
            PageFormat::Bgra8888 => u32::from_le_bytes([src[i * 4], src[i * 4 + 1], src[i * 4 + 2], src[i * 4 + 3]]),
            PageFormat::Argb4444 => {
                let c = u16::from_le_bytes([src[i * 2], src[i * 2 + 1]]) as u32;
                let n = |s: u32| ((c >> s) & 15) * 17;
                n(12) << 24 | n(8) << 16 | n(4) << 8 | n(0)
            }
            PageFormat::Argb8565 => {
                let c = u16::from_le_bytes([src[i * 3], src[i * 3 + 1]]) as u32;
                let (r, g, b) = (c >> 11, (c >> 5) & 63, c & 31);
                (src[i * 3 + 2] as u32) << 24 | ((r << 3) | (r >> 2)) << 16 | ((g << 2) | (g >> 4)) << 8 | ((b << 3) | (b >> 2))
            }
            PageFormat::Index8 => clut.map(|c| c[src[i] as usize]).unwrap_or(0),
        };
        dst[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
}

/// The baked static bottom of the scene: an opaque BGR565 image at (x, y) of the pack's screen.
pub struct BakeImage {
    pub mem: GpuMem,
    pub buffer: VgBuffer,
    pub x: i32,
    pub y: i32,
}

impl BakeImage {
    /// Inflates the BAKE section straight into GPU memory (the compiler makes its width a
    /// multiple of 16 pixels).
    pub fn from_pack(src: &mut dyn bal2d_pack::Source, bake: &bal2d_core::Bake) -> Result<BakeImage, i32> {
        let h = bake.header;
        let (width, height) = (h.width as u32, h.height as u32);
        if width % 16 != 0 || width == 0 || height == 0 {
            return Err(-22);
        }
        let prefix = bal2d_core::Bake::PREFIX;
        let pixels = (width * height * 2) as usize;
        if bake.section_len() != prefix + pixels {
            return Err(-22);
        }
        let mut mem = GpuMem::with_prefix(pixels, prefix).ok_or(-12)?;
        bal2d_pack::read_section(src, &bake.entry, mem.with_front(prefix)).map_err(|_| -84)?;
        mem.clean();
        let buffer = VgBuffer::new(&mut mem, width, height, width * 2, VG_FMT_BGR565);
        Ok(BakeImage { mem, buffer, x: h.x as i32, y: h.y as i32 })
    }
}

/// The render target: an opaque BGR565 frame on the heap, 16-pixel aligned rows.
pub struct Target {
    /// Owns the pixels (dropped with the target).
    pub mem: GpuMem,
    pub buffer: VgBuffer,
    pub pixels: *mut u8,
}

impl Target {
    pub fn new(width: u32, height: u32) -> Option<Target> {
        let stride = width.div_ceil(16) * 16 * 2;
        let mut mem = GpuMem::new(stride as usize * height as usize)?;
        // Push the zeroed lines out of the cache so later evictions cannot overwrite GPU output.
        mem.clean();
        let buffer = VgBuffer::new(&mut mem, width, height, stride, VG_FMT_BGR565);
        let pixels = mem.ptr();
        Some(Target { mem, buffer, pixels })
    }

}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub triangles: u32,
    pub calls: u32,
    /// Milliseconds spent inside `vg_lite_draw_pattern` (the driver; on Mirage also the model's
    /// rasterisation), summed over calls.
    pub submit_ms: u32,
    pub errors: u32,
    pub last_error: i32,
}

/// Per-frame drawing state reused across calls.
pub struct Renderer {
    path: Box<[u8; 72]>,
    /// Batches -> GPU calls (portable: `bal2d_render::Paths`).
    pub paths: bal2d_render::Paths,
    /// Path units to target pixels.
    path_matrix: Box<[f32; 12]>,
    image_matrix: Box<[f32; 12]>,
    pub stats: Stats,
    /// FP32 path data (the self-test's fallback): each call's path converted here.
    fp32: Option<Vec<u32>>,
    pattern_mode: u32,
}

fn page_sizes(pages: &[Page]) -> [(f32, f32); 16] {
    core::array::from_fn(|i| pages.get(i).map(|p| (p.buffer.width as f32, p.buffer.height as f32)).unwrap_or((0.0, 0.0)))
}

/// Submits calls to the driver (the renderer's state, borrowed for one draw).
struct Sink<'a> {
    path: &'a mut Box<[u8; 72]>,
    path_matrix: &'a Box<[f32; 12]>,
    image_matrix: &'a mut Box<[f32; 12]>,
    stats: &'a mut Stats,
    fp32: &'a mut Option<Vec<u32>>,
    pattern_mode: u32,
    /// The page whose palette is loaded.
    clut_page: usize,
}

impl Sink<'_> {
    fn submit(&mut self, target: &mut Target, pages: &mut [Page], call: &bal2d_render::Call) {
        let Sink { path, path_matrix, image_matrix, stats, fp32, pattern_mode, clut_page } = self;
        let pattern_mode = *pattern_mode;
        let Some(page) = pages.get_mut(call.page as usize) else { return };
        if let Some(c) = &page.clut {
            if *clut_page != call.page as usize {
                fw::vg_set_clut(256, c.as_ptr());
                *clut_page = call.page as usize;
            }
        }
        let m = &mut ***image_matrix;
        *m = [call.matrix[0], call.matrix[1], call.matrix[2], call.matrix[3], call.matrix[4], call.matrix[5], 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
        if let Some(rect) = &call.rect {
            // An atlas rectangle drawn rigidly: an image blit (no path). The rectangle is
            // the image for the GPU, so the matrix takes rectangle-relative texels.
            let (rx, ry) = (rect[0] as f32, rect[1] as f32);
            m[2] += m[0] * rx + m[1] * ry;
            m[5] += m[3] * rx + m[4] * ry;
            let timed = stats.calls % 8 == 0;
            let t0 = if timed { super::lv::now_ms() } else { 0 };
            let rc = fw::vg_blit_rect(target.buffer.ptr(), page.buffer.ptr(), rect.as_ptr(), m.as_ptr(), blend(call.blend), tint(call.color), FILTER_BILINEAR);
            if timed {
                stats.submit_ms += super::lv::now_ms().wrapping_sub(t0) * 8;
            }
            stats.calls += 1;
            if stats.calls % FLUSH_EVERY == 0 {
                let rc = fw::vg_flush();
                if rc != 0 {
                    stats.errors += 1;
                    stats.last_error = rc;
                }
            }
            if rc != 0 {
                stats.errors += 1;
                stats.last_error = rc;
            }
            return;
        }
        let p = &mut ***path;
        for (k, v) in call.bounds.iter().enumerate() {
            p[k * 4..k * 4 + 4].copy_from_slice(&(*v as f32).to_le_bytes());
        }
        match fp32.as_mut() {
            Some(f) => {
                path_to_fp32(call.path, f);
                p[40..44].copy_from_slice(&((f.len() * 4) as u32).to_le_bytes());
                p[44..48].copy_from_slice(&(f.as_ptr() as usize as u32).to_le_bytes());
            }
            None => {
                p[40..44].copy_from_slice(&((call.path.len() * 2) as u32).to_le_bytes());
                p[44..48].copy_from_slice(&(call.path.as_ptr() as usize as u32).to_le_bytes());
            }
        }
        p[48] = 1; // path changed
        // Reading the clock costs as much as a small draw (it goes through 64-bit
        // timer arithmetic): one call in 8 is timed and stands for all 8.
        let timed = stats.calls % 8 == 0;
        let t0 = if timed { super::lv::now_ms() } else { 0 };
        let rc = fw::vg_draw_pattern(
            target.buffer.ptr(),
            p.as_mut_ptr(),
            FILL_NON_ZERO,
            path_matrix.as_ptr(),
            page.buffer.ptr(),
            m.as_ptr(),
            blend(call.blend),
            pattern_mode,
            0,
            tint(call.color),
            FILTER_BILINEAR,
        );
        if timed {
            stats.submit_ms += super::lv::now_ms().wrapping_sub(t0) * 8;
        }
        stats.calls += 1;
        // Hand the GPU work as it accumulates, so it rasterizes while the CPU builds the
        // next paths (it would otherwise start only at `finish`).
        if stats.calls % FLUSH_EVERY == 0 {
            let rc = fw::vg_flush();
            if rc != 0 {
                stats.errors += 1;
                stats.last_error = rc;
            }
        }
        if rc != 0 {
            stats.errors += 1;
            stats.last_error = rc;
        }
    }
}

fn blend(b: BlendMode) -> u32 {
    match b {
        BlendMode::Normal => BLEND_SRC_OVER,
        BlendMode::Additive => BLEND_ADDITIVE,
        BlendMode::Multiply => BLEND_MULTIPLY,
        BlendMode::Screen => BLEND_SCREEN,
    }
}

/// Premultiplied tint as VGLite's 0xAABBGGRR.
fn tint(c: [f32; 4]) -> u32 {
    use bal2d_spine::math::clamp;
    let a = clamp(c[3], 0.0, 1.0);
    let q = |v: f32| ((clamp(v, 0.0, 1.0) * a * 255.0) + 0.5) as u32;
    let qa = (a * 255.0 + 0.5) as u32;
    (qa << 24) | (q(c[2]) << 16) | (q(c[1]) << 8) | q(c[0])
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    /// Sizes the per-batch buffers for the largest batch up front: nothing large is allocated
    /// while drawing (the firmware heap asserts on failure).
    pub fn reserve(&mut self, max_triangles: usize) -> bool {
        let fp32_ok = match &mut self.fp32 {
            // One record of 10 elements per triangle, END and padding.
            Some(v) => v.try_reserve_exact((max_triangles * 10 + 2).saturating_sub(v.capacity())).is_ok(),
            None => true,
        };
        fp32_ok && self.paths.reserve(max_triangles)
    }

    /// Draws FP32 paths instead of S16 ones from now on.
    pub fn use_fp32_paths(&mut self) {
        self.path[17] = PATH_FP32;
        self.fp32 = Some(Vec::new());
    }

    /// Lets the pattern outside an image be transparent instead of its edge.
    pub fn use_pattern_color(&mut self) {
        self.pattern_mode = PATTERN_COLOR;
    }

    pub fn new() -> Renderer {
        // vg_lite_matrix_t: 3 x 3, then the scale factors vg_lite_scale keeps alongside.
        let unit = 1.0 / bal2d_render::PATH_SCALE;
        let mut path_matrix = Box::new([0f32; 12]);
        path_matrix[0] = unit;
        path_matrix[4] = unit;
        path_matrix[8] = 1.0;
        path_matrix[9] = unit;
        path_matrix[10] = unit;
        let mut path = Box::new([0u8; 72]);
        path[16] = QUALITY_LOW;
        path[17] = PATH_S16;
        path[50] = 2; // fill path
        Renderer {
            path,
            paths: bal2d_render::Paths::new(),
            path_matrix,
            image_matrix: Box::new([0f32; 12]),
            stats: Stats::default(),
            fp32: None,
            pattern_mode: PATTERN_PAD,
        }
    }

    /// Starts a frame: scissor to the target (LVGL leaves its own clip enabled), clear unless the
    /// frame is covered anyway.
    pub fn begin(&mut self, target: &mut Target, clear_abgr: Option<u32>) {
        self.stats = Stats::default();
        let (w, h) = (target.buffer.width as i32, target.buffer.height as i32);
        fw::vg_set_scissor(0, 0, w, h);
        fw::vg_enable_scissor();
        if let Some(color) = clear_abgr {
            let rc = fw::vg_clear(target.buffer.ptr(), core::ptr::null(), color);
            if rc != 0 {
                self.stats.errors += 1;
                self.stats.last_error = rc;
            }
        }
    }

    /// Copies the baked bottom of the scene in (no blending: it is opaque and drawn first).
    /// `origin` is where the target's top-left corner sits on the pack's screen.
    pub fn draw_bake(&mut self, target: &mut Target, bake: &mut BakeImage, origin: (i32, i32)) {
        let rect = [0i32, 0, bake.buffer.width as i32, bake.buffer.height as i32];
        let m = &mut self.image_matrix;
        **m = [1.0, 0.0, (bake.x - origin.0) as f32, 0.0, 1.0, (bake.y - origin.1) as f32, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
        let rc = fw::vg_blit_rect(target.buffer.ptr(), bake.buffer.ptr(), rect.as_ptr(), m.as_ptr(), BLEND_NONE, 0, FILTER_POINT);
        self.stats.calls += 1;
        if rc != 0 {
            self.stats.errors += 1;
            self.stats.last_error = rc;
        }
    }

    /// Draws a list. `cam` maps skeleton to target pixels: x' = (x - cx) * s + w/2,
    /// y' = h/2 - (y - cy) * s.
    pub fn draw(&mut self, target: &mut Target, layer: usize, list: &DrawList, pages: &mut [Page], cam: (f32, f32, f32)) {
        let size = (target.buffer.width as f32, target.buffer.height as f32);
        let sizes = page_sizes(pages);
        let page_count = pages.len();
        let Renderer { path, paths, path_matrix, image_matrix, stats, fp32, pattern_mode } = self;
        let mut sink = Sink { path, path_matrix, image_matrix, stats, fp32, pattern_mode: *pattern_mode, clut_page: usize::MAX };
        let triangles = paths.triangles;
        paths.draw(layer, list, cam, size, |p| ((p as usize) < page_count.min(16)).then(|| sizes[p as usize]), |call| sink.submit(target, pages, call));
        sink.stats.triangles += paths.triangles - triangles;
    }

    /// Draws calls made elsewhere (the baked idle): `produce` gets the page sizes and the
    /// callback that submits a call, and returns the triangles it drew.
    pub fn draw_calls(&mut self, target: &mut Target, pages: &mut [Page], produce: impl FnOnce(&dyn Fn(u32) -> Option<(f32, f32)>, &mut dyn FnMut(&bal2d_render::Call)) -> u32) {
        let sizes = page_sizes(pages);
        let page_count = pages.len();
        let Renderer { path, path_matrix, image_matrix, stats, fp32, pattern_mode, .. } = self;
        let mut sink = Sink { path, path_matrix, image_matrix, stats, fp32, pattern_mode: *pattern_mode, clut_page: usize::MAX };
        let size = |p: u32| ((p as usize) < page_count.min(16)).then(|| sizes[p as usize]);
        let triangles = produce(&size, &mut |call| sink.submit(target, pages, call));
        sink.stats.triangles += triangles;
    }

    /// Submits what is queued without waiting.
    pub fn flush(&mut self) {
        let rc = fw::vg_flush();
        if rc != 0 {
            self.stats.errors += 1;
            self.stats.last_error = rc;
        }
    }

    pub fn finish(&mut self) {
        let rc = fw::vg_finish();
        if rc != 0 {
            self.stats.errors += 1;
            self.stats.last_error = rc;
        }
    }
}

/// S16 path data to FP32 (the same units): opcodes in 32-bit slots, coordinates as floats.
pub fn path_to_fp32(src: &[i16], dst: &mut Vec<u32>) {
    dst.clear();
    let mut i = 0;
    while i < src.len() {
        let op = src[i] as u32;
        dst.push(op);
        i += 1;
        let args = match op {
            bal2d_render::PATH_MOVE | bal2d_render::PATH_LINE => 2,
            0 => break,
            _ => 0,
        };
        for k in 0..args {
            dst.push((src[i + k] as f32).to_bits());
        }
        i += args;
    }
}

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
/// LVGL's XRGB8888 / RGB888 layers.
pub const VG_FMT_BGRX8888: u16 = 3;
pub const VG_FMT_BGR888: u16 = 119;
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
/// Set by the self-test when draws batched into a command stream (`Stream`) came out wrong:
/// every draw goes through the driver then.
pub static STREAM_OFF: AtomicBool = AtomicBool::new(false);
/// Bytes of the per-frame command stream (a heavy baked frame takes ~35 KB, a live one with
/// triangle paths ~80 KB; when full the GPU is waited for and it starts over).
const STREAM_BYTES: usize = 96 * 1024;
/// Page formats the self-test found drawn wrong (bit = `PageFormat as u8`): converted to
/// BGRA8888 on upload.
pub static BAD_FORMATS: AtomicU32 = AtomicU32::new(0);

/// The non-cached alias of a cached PSRAM address (0x3C.. -> 0x38..), when it is one.
pub fn uncached(addr: usize) -> Option<usize> {
    (0x3C00_0000..0x3E00_0000).contains(&addr).then_some(addr - 0x0400_0000)
}

/// The address the GPU is given for CPU memory at `addr`: PSRAM through its non-cached alias.
/// On the watch the GPU's accesses to the cached alias go through the CPU's cache: what it
/// writes there stays in dirty lines (the third real-watch log: nothing of a finished draw in
/// memory, all of it after a cache flush) and what it reads competes with the CPU for a small
/// cache. The vendor driver keeps its own GPU memory (command and tessellation buffers) in the
/// non-cached alias too. CPU writes still go through the cache and are cleaned before the GPU
/// reads them (`GpuMem::clean`).
pub fn gpu_addr(addr: usize) -> u32 {
    if UNCACHED.load(Ordering::Relaxed) { uncached(addr).unwrap_or(addr) as u32 } else { addr as u32 }
}

/// GPU addresses through the non-cached alias (`gpu_addr`; a feature the watch tries, see
/// `trust`: the old stable drawing gave the GPU the cached alias). Buffers made while it is off
/// keep the cached alias until re-addressed (`VgBuffer::readdress`) or made again.
pub static UNCACHED: AtomicBool = AtomicBool::new(true);
/// Submits also once the draws add up to `SPLIT_PX` pixels (`FLUSH_PIXELS` until the start-up
/// measurement sets it from the GPU's speed, see `bench`). The fifth real-watch log: with
/// every submit of 48 draws the heavy phases of an animation ran into the driver's timeout.
pub static SPLIT: AtomicBool = AtomicBool::new(true);
pub static SPLIT_PX: AtomicU32 = AtomicU32::new(FLUSH_PIXELS);
/// Draw calls between submits (`vg_lite_flush`) within a frame.
const FLUSH_EVERY: u32 = 48;
/// Pixels of drawing (the draws' screen rectangles, an upper bound of what they fill) between
/// submits: one screen. The firmware waits 100 ms for a submit and then resets the GPU and
/// reports success, so the rest of that submit is lost (on the watch: parts of the character
/// missing for a frame, some cut into bands). A screen of the heaviest layers takes the
/// watch's GPU about 10 ms; capping each submit there keeps every wait far below the limit.
const FLUSH_PIXELS: u32 = 336 * 480 / 2;
/// A wait this long was the driver's 100 ms timeout (and its GPU reset).
pub const TIMEOUT_MS: u32 = 90;
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
        // `memory` (the CPU's pointer) and `address` (the GPU's).
        let p = mem.ptr() as usize;
        raw[20..24].copy_from_slice(&(p as u32).to_le_bytes());
        raw[24..28].copy_from_slice(&gpu_addr(p).to_le_bytes());
        // Normal image mode until set; opaque transparency mode 0.
        raw[80..82].copy_from_slice(&0x1F00u16.to_le_bytes());
        VgBuffer { raw, width, height, stride }
    }

    /// A buffer over memory someone else owns (LVGL's layer): no `GpuMem`, nothing to free.
    pub fn wrap(data: *mut u8, width: u32, height: u32, stride: u32, format: u16) -> VgBuffer {
        let mut b = VgBuffer { raw: Box::new([0u8; 176]), width: 0, height: 0, stride: 0 };
        b.rebind(data, width, height, stride, format);
        b
    }

    /// Points the buffer at other memory (a wrapped layer that moved or changed size).
    pub fn rebind(&mut self, data: *mut u8, width: u32, height: u32, stride: u32, format: u16) {
        let raw = &mut self.raw;
        raw.fill(0);
        raw[0..4].copy_from_slice(&(width as i32).to_le_bytes());
        raw[4..8].copy_from_slice(&(height as i32).to_le_bytes());
        raw[8..12].copy_from_slice(&(stride as i32).to_le_bytes());
        raw[14..16].copy_from_slice(&format.to_le_bytes());
        let p = data as usize;
        raw[20..24].copy_from_slice(&(p as u32).to_le_bytes());
        raw[24..28].copy_from_slice(&gpu_addr(p).to_le_bytes());
        raw[80..82].copy_from_slice(&0x1F00u16.to_le_bytes());
        (self.width, self.height, self.stride) = (width, height, stride);
    }

    /// Gives the GPU the memory's address as `gpu_addr` now says (cached or non-cached alias).
    pub fn readdress(&mut self) {
        let p = u32::from_le_bytes([self.raw[20], self.raw[21], self.raw[22], self.raw[23]]) as usize;
        self.raw[24..28].copy_from_slice(&gpu_addr(p).to_le_bytes());
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

/// Draws batched into a command stream of our own, run by the GPU through one driver call.
///
/// The driver programs some 34 state registers and copies the path for every draw
/// (`vg_lite_draw_pattern`), into a command buffer in uncached memory, and takes a critical
/// section and reports power-management activity each time: on the watch that was ~150 µs a call,
/// a third of a heavy frame. Here a run of draws with one page and blend goes to the driver as
/// a single draw whose path is "uploaded" (`uploaded.property` bit 0): the driver sets the
/// page, blend, path matrix and the rest up for it and then CALLs our stream, which per draw
/// only reprograms what differs (the image coordinate generator, the colour, the tessellation
/// window) and passes the path, as the driver would (registers and values as Mirage's
/// captures of the driver show them; the generator math is the driver's own, see `generator`).
pub struct Stream {
    mem: GpuMem,
    /// Bytes written; where the current batch starts; its draws.
    at: usize,
    batch: usize,
    n: u32,
    page: u32,
    blend: BlendMode,
    /// The batch's first draw (the carrier draws with it) and the union of the bounds (path units).
    matrix: [f32; 6],
    color: u32,
    lo: [i32; 2],
    hi: [i32; 2],
    carrier: Box<[u8; 72]>,
}

const ST_COLOR: u32 = 0x0A02;
const ST_FLUSH: u32 = 0x0A1B;
/// Written by the driver right before every path's window (0x3A9 for every S16, low-quality,
/// non-zero path of the frames: Mirage captures of 9248 driver draws).
const ST_PATH_SETUP: u32 = 0x0A3D;
const PATH_SETUP: u32 = 0x3A9;
const ST_TESS_ORIGIN: u32 = 0x0A39;
const ST_TESS_SIZE: u32 = 0x0A3A;
/// The image coordinate generator, register by register as the driver writes it: c0..c2 at
/// 0x0A18-0x0A1A, x0..x2 at 0x0A1C-0x0A1E, 0x0A1F = 1, y0..y2 at 0x0A20-0x0A22.
const GENERATOR_REGS: [u32; 10] = [0x0A18, 0x0A19, 0x0A1A, 0x0A1C, 0x0A1D, 0x0A1E, 0x0A1F, 0x0A20, 0x0A21, 0x0A22];
/// What the driver writes to 0x0A1B before each path's window, and the generator's 0x0A1F.
const FLUSH_BEFORE_PATH: u32 = 0x0001_1000;
const GENERATOR_ON: u32 = 1;
/// Bytes a draw takes besides its path: 15 single-register states of 8, the data header 8;
/// and the end.
const DRAW_OVERHEAD: usize = 15 * 8 + 8;
const STREAM_END: usize = 8;

impl Stream {
    pub fn new(bytes: usize) -> Option<Stream> {
        let mem = GpuMem::new(bytes)?;
        let mut carrier = Box::new([0u8; 72]);
        carrier[16] = QUALITY_LOW;
        carrier[17] = PATH_S16;
        carrier[50] = 2; // fill path
        Some(Stream { mem, at: 0, batch: 0, n: 0, page: 0, blend: BlendMode::Normal, matrix: [0.0; 6], color: 0, lo: [0; 2], hi: [0; 2], carrier })
    }

    /// Starts over at the beginning (the GPU is done with what was there).
    pub fn reset(&mut self) {
        (self.at, self.batch, self.n) = (0, 0, 0);
    }

    pub fn pending(&self) -> u32 {
        self.n
    }

    pub fn fits(&self, path_i16: usize) -> bool {
        self.at + DRAW_OVERHEAD + (path_i16 * 2).div_ceil(8) * 8 + STREAM_END <= self.mem.len
    }

    fn put(&mut self, w: u32) {
        let at = self.at;
        self.mem.slice()[at..at + 4].copy_from_slice(&w.to_le_bytes());
        self.at += 4;
    }

    /// One register, one STATE command (as the driver writes every per-draw register; the
    /// multi-register form is only known to work on the watch for the palette).
    fn state(&mut self, reg: u32, value: u32) {
        self.put(0x3001_0000 | reg);
        self.put(value);
    }

    /// Appends a draw of `path` (S16, END-terminated) with the image coordinate generator
    /// `generator`, colour `color` and tessellation window `window` ((x, y, w, h) in target
    /// pixels). The first draw of a batch also sets what the carrier draws with.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, page: u32, blend: BlendMode, matrix: &[f32; 6], generator: &[u32; 9], color: u32, window: [i32; 4], bounds: [i16; 4], path: &[i16]) {
        if self.n == 0 {
            self.batch = self.at;
            (self.page, self.blend, self.matrix, self.color) = (page, blend, *matrix, color);
            self.lo = [bounds[0] as i32, bounds[1] as i32];
            self.hi = [bounds[2] as i32, bounds[3] as i32];
        } else {
            self.lo = [self.lo[0].min(bounds[0] as i32), self.lo[1].min(bounds[1] as i32)];
            self.hi = [self.hi[0].max(bounds[2] as i32), self.hi[1].max(bounds[3] as i32)];
        }
        // In the driver's order: the generator, the colour, then right before the path the
        // flush, the path setup and the tessellation window (the batched draws of an earlier
        // build wrote no path setup and used multi-register writes; the watch's GPU hung).
        let g = generator;
        let values = [g[0], g[1], g[2], g[3], g[4], g[5], GENERATOR_ON, g[6], g[7], g[8]];
        for (reg, v) in GENERATOR_REGS.into_iter().zip(values) {
            self.state(reg, v);
        }
        self.state(ST_COLOR, color);
        self.state(ST_FLUSH, FLUSH_BEFORE_PATH);
        self.state(ST_PATH_SETUP, PATH_SETUP);
        self.state(ST_TESS_ORIGIN, (window[0] as u32) | (window[1] as u32) << 16);
        self.state(ST_TESS_SIZE, (window[2] as u32) | (window[3] as u32) << 16);
        let bytes = path.len() * 2;
        let units = bytes.div_ceil(8);
        self.put(0x4000_0000 | units as u32);
        self.put(0);
        let at = self.at;
        let dst = &mut self.mem.slice()[at..at + units * 8];
        for (d, v) in dst.chunks_exact_mut(2).zip(path) {
            d.copy_from_slice(&v.to_le_bytes());
        }
        dst[bytes..].fill(0);
        self.at += units * 8;
        self.n += 1;
    }

    /// Ends the batch and prepares the carrier path: returns it with the batch's page, blend,
    /// first matrix and colour (None when the batch is empty). The stream is cleaned out of the
    /// cache for the GPU.
    pub fn close(&mut self) -> Option<(*mut u8, u32, BlendMode, [f32; 6], u32)> {
        if self.n == 0 {
            return None;
        }
        self.put(0x7000_0000);
        self.put(0);
        let start = self.batch;
        let len = self.at - start;
        let addr = self.mem.ptr() as usize + start;
        fw::cache_clean(addr as *const u8, len);
        let c = &mut self.carrier;
        for (k, v) in [self.lo[0], self.lo[1], self.hi[0], self.hi[1]].iter().enumerate() {
            c[k * 4..k * 4 + 4].copy_from_slice(&(*v as f32).to_le_bytes());
        }
        // uploaded: handle, memory, address, bytes, property (bit 0: uploaded, CALL it).
        c[20..24].copy_from_slice(&0u32.to_le_bytes());
        c[24..28].copy_from_slice(&(addr as u32).to_le_bytes());
        // What the driver CALLs: the GPU fetches the stream from memory, past the cache.
        c[28..32].copy_from_slice(&gpu_addr(addr).to_le_bytes());
        c[32..36].copy_from_slice(&(len as u32).to_le_bytes());
        c[36..40].copy_from_slice(&1u32.to_le_bytes());
        c[40..44].copy_from_slice(&(len as u32).to_le_bytes());
        c[44..48].copy_from_slice(&(addr as u32).to_le_bytes());
        // Not changed: the driver would free the "upload" and copy the path anew.
        c[48] = 0;
        self.n = 0;
        Some((c.as_mut_ptr(), self.page, self.blend, self.matrix, self.color))
    }
}

/// The image coordinate generator the driver programs (0x0A18-0x0A1A, 0x0A1C-0x0A1E,
/// 0x0A20-0x0A22) for an affine texel -> target matrix `m` and a `w` x `h` image, bilinear:
/// the inverse matrix sampled at pixel centres, half a texel back, in image units (as
/// `vg_lite_draw_pattern` computes it in 4.0.47). None for a singular matrix.
pub fn generator(m: &[f32; 6], w: f32, h: f32) -> Option<[u32; 9]> {
    let det = m[0] * m[4] - m[1] * m[3];
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let a = m[4] / det;
    let b = -m[1] / det;
    let c = (m[1] * m[5] - m[4] * m[2]) / det;
    let d = -m[3] / det;
    let e = m[0] / det;
    let f = (m[3] * m[2] - m[0] * m[5]) / det;
    let c0 = ((a + b) * 0.5 + c - 0.5) / w;
    let c1 = ((d + e) * 0.5 + f - 0.5) / h;
    Some([c0, c1, 1.0, a / w, d / h, 0.0, b / w, e / h, 0.0].map(f32::to_bits))
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub triangles: u32,
    pub calls: u32,
    /// Milliseconds spent inside `vg_lite_draw_pattern` (the driver; on Mirage also the model's
    /// rasterisation), summed over calls.
    pub submit_ms: u32,
    /// Milliseconds inside `vg_lite_flush` within the frame: the CPU waiting for the GPU to free
    /// a command buffer (it is that far behind).
    pub flush_ms: u32,
    /// Driver calls that carried a batch of draws (`Stream`).
    pub batches: u32,
    pub errors: u32,
    pub last_error: i32,
    /// Draws and their pixels since the last submit (`FLUSH_EVERY`, `FLUSH_PIXELS`).
    queued: u32,
    queued_px: u32,
    /// Waits for the GPU that ran into the driver's timeout (it reset the GPU), and the first
    /// few of them: (where, ms) with where 1 flush, 2 finish, 3 batch carry, 4 draw, 5 blit, 6
    /// stream full.
    pub resets: u32,
    pub long_waits: [(u8, u16); 4],
    /// Draws left out as degenerate (`degenerate`), and the first of them: (det, bounds).
    pub skipped: u32,
    pub first_skip: Option<(f32, [i16; 4])>,
    /// The farthest path coordinate from the target's middle this frame (pixels), and that
    /// draw's bounds (path units).
    pub extent: u32,
    pub extent_bounds: [i16; 4],
}

/// Why a draw is left out, if it is: a texel -> pixel affine that is not finite or (nearly)
/// singular, which the driver inverts for the image coordinate generator (an attachment scaled
/// to nothing gives one; Mirage's model then draws nothing, the watch's GPU may hang until the
/// driver's 100 ms timeout), or empty bounds. What it would draw is less than a pixel.
fn degenerate(call: &bal2d_render::Call) -> Option<f32> {
    let m = call.matrix;
    let det = m[0] * m[4] - m[1] * m[3];
    let finite = m.iter().all(|v| v.is_finite());
    let b = call.bounds;
    let empty = call.rect.is_none() && (b[2] <= b[0] || b[3] <= b[1]);
    (!finite || !(det.abs() >= MIN_DET) || det.abs() > MAX_DET || empty).then_some(det)
}

/// |det| of a texel -> pixel affine kept: a texel no smaller than 1/32 pixel each way on
/// average, and no larger than 256 pixels.
const MIN_DET: f32 = 1.0 / 1024.0;
const MAX_DET: f32 = 65536.0;

impl Stats {
    /// Counts a draw of `px` pixels; true when it is time to submit.
    fn queue(&mut self, px: u32) -> bool {
        self.queued += 1;
        self.queued_px = self.queued_px.saturating_add(px);
        self.queued >= FLUSH_EVERY || self.split_due()
    }

    fn split_due(&self) -> bool {
        SPLIT.load(Ordering::Relaxed) && self.queued_px >= SPLIT_PX.load(Ordering::Relaxed)
    }

    /// A wait of `ms` for the GPU at `at` (see `long_waits`): the driver's timeout when that long.
    fn waited(&mut self, ms: u32, at: u8) {
        if ms >= TIMEOUT_MS {
            if let Some(slot) = self.long_waits.get_mut(self.resets as usize) {
                *slot = (at, ms.min(65535) as u16);
            }
            self.resets += 1;
        }
    }
}

/// A draw as the hang diagnostics see it.
#[derive(Clone, Copy, Default, Debug)]
pub struct DrawRec {
    /// `Call::tag` with the layer in bits 28..30: the same draw from frame to frame.
    pub sig: u32,
    pub page: u8,
    pub blend: u8,
    /// Path length (16-bit words), 0 for a blit.
    pub len: u16,
    pub bounds: [i16; 4],
    pub det: f32,
}

/// What went into the submits that hang the watch's GPU (the driver resets it after 100 ms and
/// drops the submit), for the log. Each submit's draws are kept until the GPU has been waited
/// for past them. (Leaving out the draws of hung submits was tried: the culprit changed from
/// hang to hang, every long triangle-soup path of a live layer was one, and parts of the
/// character went missing one by one. Outline paths are the fix; this stays as evidence.)
#[derive(Default)]
pub struct Hangs {
    /// Draws since the last submit, and those of the last submit (what the next wait is for).
    pub cur: Vec<DrawRec>,
    pub prev: Vec<DrawRec>,
    /// Draws of the submits that timed out this frame (for the log).
    pub hung: Vec<DrawRec>,
    /// The layer being drawn (part of `DrawRec::sig`).
    pub layer: u32,
    /// Frames since a timeout.
    pub quiet_frames: u32,
    /// Development (`dev-gpu-hang`): a submit holding this draw counts as timed out.
    pub fake: u32,
    /// The last frame's draws (development: picking one to fake).
    pub seen: Vec<u32>,
}


impl Hangs {
    /// The wait just made (`ms`) was for the last submit, or (`both`) for it and the one being
    /// made: a timeout there makes its draws suspects, or names the culprit when it held a lone
    /// suspect. True when it timed out (or `fake` says so).
    fn waited(&mut self, ms: u32, both: bool) -> bool {
        let fake = self.fake != 0 && (self.prev.iter().any(|r| r.sig == self.fake) || both && self.cur.iter().any(|r| r.sig == self.fake));
        if ms < TIMEOUT_MS && !fake {
            return false;
        }
        self.quiet_frames = 0;
        let mut waited: Vec<DrawRec> = core::mem::take(&mut self.prev);
        if both {
            waited.extend_from_slice(&self.cur);
        }
        let room = 96usize.saturating_sub(self.hung.len());
        self.hung.extend(waited.iter().take(room).copied());
        true
    }

    /// A submit was made: its draws are what the next wait is for.
    fn submitted(&mut self) {
        core::mem::swap(&mut self.prev, &mut self.cur);
        self.cur.clear();
    }

    /// Waited for everything mid-frame: nothing outstanding.
    fn drained(&mut self) {
        self.prev.clear();
        self.cur.clear();
    }

    /// The frame is done (the GPU waited for): nothing outstanding.
    fn finished(&mut self) {
        self.seen.clear();
        self.seen.extend(self.prev.iter().chain(self.cur.iter()).map(|r| r.sig));
        self.prev.clear();
        self.cur.clear();
        self.quiet_frames += 1;
    }
}

/// Pixels of the rectangle `b` (path units) moved by `offset` and cut to `clip` (target pixels).
fn bounds_px(b: [i16; 4], offset: (f32, f32), clip: [i32; 4]) -> u32 {
    let k = 1.0 / bal2d_render::PATH_SCALE;
    let x0 = ((b[0] as f32 * k + offset.0) as i32).max(clip[0]);
    let y0 = ((b[1] as f32 * k + offset.1) as i32).max(clip[1]);
    let x1 = ((b[2] as f32 * k + offset.0) as i32 + 1).min(clip[2]);
    let y1 = ((b[3] as f32 * k + offset.1) as i32 + 1).min(clip[3]);
    if x1 <= x0 || y1 <= y0 { 0 } else { ((x1 - x0) * (y1 - y0)) as u32 }
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
    /// Layer pixels -> target pixels for this frame (`begin`).
    offset: (f32, f32),
    /// The frame's clip (target pixels: x, y, right, bottom).
    clip: [i32; 4],
    /// Draws batched for the GPU (None: each through the driver), and whether they are wanted.
    pub stream: Option<Stream>,
    stream_wanted: bool,
    pub hangs: Hangs,
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
    /// Layer pixels -> target pixels (where the layer's top-left corner is in the target).
    offset: (f32, f32),
    clip: [i32; 4],
    stream: &'a mut Option<Stream>,
    hangs: &'a mut Hangs,
}

impl Sink<'_> {
    fn submit(&mut self, target: &mut VgBuffer, pages: &mut [Page], call: &bal2d_render::Call) {
        if let Some(det) = degenerate(call) {
            self.stats.skipped += 1;
            if self.stats.first_skip.is_none() {
                self.stats.first_skip = Some((det, call.bounds));
            }
            return;
        }
        if call.rect.is_none() {
            let k = 1.0 / bal2d_render::PATH_SCALE;
            let b = call.bounds;
            let (ox, oy) = self.offset;
            // Wholly outside the clip: nothing to draw, nothing to tessellate.
            let c = self.clip;
            if (b[2] as f32 * k + ox) < c[0] as f32 || (b[0] as f32 * k + ox) > c[2] as f32 || (b[3] as f32 * k + oy) < c[1] as f32 || (b[1] as f32 * k + oy) > c[3] as f32 {
                return;
            }
            let far = [(b[0] as f32 * k + ox - 168.0), (b[2] as f32 * k + ox - 168.0), (b[1] as f32 * k + oy - 240.0), (b[3] as f32 * k + oy - 240.0)]
                .iter()
                .fold(0f32, |m, v| m.max(v.abs())) as u32;
            if far > self.stats.extent {
                self.stats.extent = far;
                self.stats.extent_bounds = b;
            }
        }
        // Kept for the log: what went into a submit that times out.
        let sig = (call.tag & 0x8FFF_FFFF) | (self.hangs.layer & 7) << 28;
        let m = call.matrix;
        let det = m[0] * m[4] - m[1] * m[3];
        if self.hangs.cur.len() < 256 {
            self.hangs.cur.push(DrawRec { sig, page: call.page as u8, blend: call.blend as u8, len: call.path.len().min(65535) as u16, bounds: call.bounds, det });
        }
        self.submit_one(target, pages, call);
    }

    fn submit_one(&mut self, target: &mut VgBuffer, pages: &mut [Page], call: &bal2d_render::Call) {
        if call.rect.is_none() && self.fp32.is_none() && self.stream.is_some() {
            self.batch(target, pages, call);
            return;
        }
        self.carry(target, pages);
        let Sink { path, path_matrix, image_matrix, stats, fp32, pattern_mode, clut_page, offset, clip, hangs, .. } = self;
        let pattern_mode = *pattern_mode;
        let Some(page) = pages.get_mut(call.page as usize) else { return };
        if let Some(c) = &page.clut {
            if *clut_page != call.page as usize {
                fw::vg_set_clut(256, c.as_ptr());
                *clut_page = call.page as usize;
            }
        }
        let m = &mut ***image_matrix;
        *m = [call.matrix[0], call.matrix[1], call.matrix[2] + offset.0, call.matrix[3], call.matrix[4], call.matrix[5] + offset.1, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
        if let Some(rect) = &call.rect {
            // An atlas rectangle drawn rigidly: an image blit (no path). The rectangle is
            // the image for the GPU, so the matrix takes rectangle-relative texels.
            let (rx, ry) = (rect[0] as f32, rect[1] as f32);
            m[2] += m[0] * rx + m[1] * ry;
            m[5] += m[3] * rx + m[4] * ry;
            let timed = stats.calls % 8 == 0;
            let t0 = if timed { super::lv::now_ms() } else { 0 };
            let rc = fw::vg_blit_rect(target.ptr(), page.buffer.ptr(), rect.as_ptr(), m.as_ptr(), blend(call.blend), tint(call.color), FILTER_BILINEAR);
            if timed {
                let dt = super::lv::now_ms().wrapping_sub(t0);
                stats.waited(dt, 5);
                stats.submit_ms += dt * 8;
            }
            stats.calls += 1;
            let px = ((m[0] * m[4] - m[1] * m[3]).abs() * rect[2] as f32 * rect[3] as f32) as u32;
            if stats.queue(px) {
                timed_flush(stats, hangs);
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
            target.ptr(),
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
            let dt = super::lv::now_ms().wrapping_sub(t0);
            stats.waited(dt, 4);
            stats.submit_ms += dt * 8;
        }
        stats.calls += 1;
        // Hand the GPU work as it accumulates, so it rasterizes while the CPU builds the
        // next paths (it would otherwise start only at `finish`), and in pieces it gets through
        // well within the driver's timeout.
        if stats.queue(bounds_px(call.bounds, *offset, *clip)) {
            timed_flush(stats, hangs);
        }
        if rc != 0 {
            stats.errors += 1;
            stats.last_error = rc;
        }
    }
}

impl Sink<'_> {
    /// Adds a path draw to the stream's batch (a new batch for another page or blend, or after
    /// `FLUSH_EVERY` draws, handing the GPU what it has).
    fn batch(&mut self, target: &mut VgBuffer, pages: &mut [Page], call: &bal2d_render::Call) {
        let Some(page) = pages.get(call.page as usize) else { return };
        let (pw, ph) = (page.buffer.width as f32, page.buffer.height as f32);
        let (ox, oy) = self.offset;
        let m = [call.matrix[0], call.matrix[1], call.matrix[2] + ox, call.matrix[3], call.matrix[4], call.matrix[5] + oy];
        let Some(g) = generator(&m, pw, ph) else { return };
        // The tessellation window: the bounds in target pixels (a pixel of margin either way: a
        // larger window only costs time), cut to the clip.
        let k = 1.0 / bal2d_render::PATH_SCALE;
        let b = call.bounds;
        let c = self.clip;
        let x0 = ((b[0] as f32 * k + ox) as i32 - 2).max(c[0]);
        let y0 = ((b[1] as f32 * k + oy) as i32 - 2).max(c[1]);
        let x1 = ((b[2] as f32 * k + ox) as i32 + 2).min(c[2]);
        let y1 = ((b[3] as f32 * k + oy) as i32 + 2).min(c[3]);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let (pending, same, fits) = match self.stream.as_ref() {
            Some(st) => (st.pending(), st.page == call.page && st.blend == call.blend, st.fits(call.path.len())),
            None => return,
        };
        if pending > 0 && (!same || self.stats.queued >= FLUSH_EVERY || self.stats.split_due() || !fits) {
            self.carry(target, pages);
        }
        if !fits {
            // Full: the GPU runs what is there before it is written over.
            let t0 = super::lv::now_ms();
            let rc = fw::vg_finish();
            let dt = super::lv::now_ms().wrapping_sub(t0);
            if self.hangs.waited(dt, true) {
                self.stats.waited(dt.max(TIMEOUT_MS), 6);
            }
            self.hangs.drained();
            if rc != 0 {
                self.stats.errors += 1;
                self.stats.last_error = rc;
            }
            if let Some(st) = self.stream.as_mut() {
                st.reset();
                if !st.fits(call.path.len()) {
                    return;
                }
            }
        }
        if let Some(cl) = &pages[call.page as usize].clut {
            if self.clut_page != call.page as usize {
                fw::vg_set_clut(256, cl.as_ptr());
                self.clut_page = call.page as usize;
            }
        }
        if let Some(st) = self.stream.as_mut() {
            st.draw(call.page, call.blend, &m, &g, tint(call.color), [x0, y0, x1 - x0, y1 - y0], call.bounds, call.path);
        }
        self.stats.calls += 1;
        self.stats.queued += 1;
        self.stats.queued_px = self.stats.queued_px.saturating_add(((x1 - x0) * (y1 - y0)) as u32);
    }

    /// Hands the open batch to the driver: one draw with the batch's page, blend and first
    /// matrix whose path CALLs the stream.
    fn carry(&mut self, target: &mut VgBuffer, pages: &mut [Page]) {
        let Some(st) = self.stream.as_mut() else { return };
        let Some((path, page, blend_mode, matrix, color)) = st.close() else { return };
        let Some(page) = pages.get_mut(page as usize) else { return };
        let m = &mut **self.image_matrix;
        *m = [matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5], 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
        let t0 = super::lv::now_ms();
        let rc = fw::vg_draw_pattern(
            target.ptr(),
            path,
            FILL_NON_ZERO,
            self.path_matrix.as_ptr(),
            page.buffer.ptr(),
            m.as_ptr(),
            blend(blend_mode),
            self.pattern_mode,
            0,
            color,
            FILTER_BILINEAR,
        );
        let dt = super::lv::now_ms().wrapping_sub(t0);
        self.stats.waited(dt, 3);
        self.stats.submit_ms += dt;
        self.stats.batches += 1;
        if rc != 0 {
            self.stats.errors += 1;
            self.stats.last_error = rc;
        }
        timed_flush(self.stats, self.hangs);
    }
}

/// `vg_lite_flush`, timed: it returns once the GPU has freed the other command buffer, so its
/// time is the CPU waiting for the GPU.
fn timed_flush(stats: &mut Stats, hangs: &mut Hangs) {
    let t0 = super::lv::now_ms();
    let rc = fw::vg_flush();
    let dt = super::lv::now_ms().wrapping_sub(t0);
    if hangs.waited(dt, false) {
        stats.waited(dt.max(TIMEOUT_MS), 1);
    }
    hangs.submitted();
    stats.flush_ms += dt;
    stats.queued = 0;
    stats.queued_px = 0;
    if rc != 0 {
        stats.errors += 1;
        stats.last_error = rc;
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
        if self.stream.is_none() && self.stream_wanted && !STREAM_OFF.load(Ordering::Relaxed) {
            // Fallible: without it every draw goes through the driver.
            self.stream = Stream::new(STREAM_BYTES);
        }
        fp32_ok && self.paths.reserve(max_triangles)
    }

    /// Batched draws on or off (`options::SINGLE`; the self-test can rule them out anyway).
    /// Turned on, the stream is allocated `now` (a pack is shown) or with the next pack.
    pub fn want_stream(&mut self, on: bool, now: bool) {
        self.stream_wanted = on;
        if !on || STREAM_OFF.load(Ordering::Relaxed) {
            self.stream = None;
        } else if now && self.stream.is_none() {
            self.stream = Stream::new(STREAM_BYTES);
        }
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

    /// PAD pattern edges (the image's edge texels outside it) or transparent ones.
    pub fn set_pad(&mut self, pad: bool) {
        self.pattern_mode = if pad { PATTERN_PAD } else { PATTERN_COLOR };
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
            offset: (0.0, 0.0),
            clip: [0; 4],
            stream: None,
            stream_wanted: true,
            hangs: Hangs::default(),
        }
    }

    /// Starts a frame into `target`, whose pixel (`offset`) is the layer's top-left corner:
    /// scissor to `clip` (target pixels: x, y, right, bottom; LVGL leaves its own clip
    /// enabled), and clear the layer's `clear` rectangle (target pixels: x, y, w, h) unless the
    /// frame covers it anyway.
    pub fn begin(&mut self, target: &mut VgBuffer, offset: (i32, i32), clip: [i32; 4], clear: Option<([i32; 4], u32)>) {
        self.stats = Stats::default();
        self.offset = (offset.0 as f32, offset.1 as f32);
        self.path_matrix[2] = self.offset.0;
        self.path_matrix[5] = self.offset.1;
        self.clip = clip;
        // The GPU finished the last frame: the stream starts over.
        if let Some(st) = self.stream.as_mut() {
            st.reset();
        }
        fw::vg_set_scissor(clip[0], clip[1], clip[2], clip[3]);
        fw::vg_enable_scissor();
        if let Some((rect, color)) = clear {
            self.stats.queued_px = (rect[2].max(0) * rect[3].max(0)) as u32;
            let rc = fw::vg_clear(target.ptr(), rect.as_ptr(), color);
            if rc != 0 {
                self.stats.errors += 1;
                self.stats.last_error = rc;
            }
        }
    }

    /// Leaves the scissor over the whole of `target` (a layer LVGL goes on drawing into).
    pub fn end_scissor(&mut self, target: &VgBuffer) {
        fw::vg_set_scissor(0, 0, target.width as i32, target.height as i32);
    }

    /// Copies the baked bottom of the scene in (no blending: it is opaque and drawn first), the
    /// part of it inside `clip` (target pixels: x, y, right, bottom; the copy is a rectangle of
    /// the image, so nothing outside is written whatever the scissor). `origin` is where the
    /// layer's top-left corner sits on the pack's screen.
    pub fn draw_bake(&mut self, target: &mut VgBuffer, bake: &mut BakeImage, origin: (i32, i32), clip: [i32; 4]) {
        let (bx, by) = (bake.x - origin.0 + self.offset.0 as i32, bake.y - origin.1 + self.offset.1 as i32);
        let (x0, y0) = (clip[0].max(bx), clip[1].max(by));
        let (x1, y1) = (clip[2].min(bx + bake.buffer.width as i32), clip[3].min(by + bake.buffer.height as i32));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let rect = [x0 - bx, y0 - by, x1 - x0, y1 - y0];
        let m = &mut self.image_matrix;
        **m = [1.0, 0.0, x0 as f32, 0.0, 1.0, y0 as f32, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
        let rc = fw::vg_blit_rect(target.ptr(), bake.buffer.ptr(), rect.as_ptr(), m.as_ptr(), BLEND_NONE, 0, FILTER_POINT);
        self.stats.calls += 1;
        if rc != 0 {
            self.stats.errors += 1;
            self.stats.last_error = rc;
        }
    }

    /// Draws a list into a layer of `size` pixels. `cam` maps skeleton to layer pixels:
    /// x' = (x - cx) * s + w/2, y' = h/2 - (y - cy) * s.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, target: &mut VgBuffer, size: (f32, f32), layer: usize, list: &DrawList, pages: &mut [Page], cam: (f32, f32, f32)) {
        let sizes = page_sizes(pages);
        let page_count = pages.len();
        let Renderer { path, paths, path_matrix, image_matrix, stats, fp32, pattern_mode, offset, clip, stream, hangs, .. } = self;
        let mut sink = Sink { path, path_matrix, image_matrix, stats, fp32, pattern_mode: *pattern_mode, clut_page: usize::MAX, offset: *offset, clip: *clip, stream, hangs };
        let triangles = paths.triangles;
        paths.draw(layer, list, cam, size, |p| ((p as usize) < page_count.min(16)).then(|| sizes[p as usize]), |call| sink.submit(target, pages, call));
        sink.carry(target, pages);
        sink.stats.triangles += paths.triangles - triangles;
    }

    /// Draws calls made elsewhere (the baked idle): `produce` gets the page sizes and the
    /// callback that submits a call, and returns the triangles it drew.
    pub fn draw_calls(&mut self, target: &mut VgBuffer, pages: &mut [Page], produce: impl FnOnce(&dyn Fn(u32) -> Option<(f32, f32)>, &mut dyn FnMut(&bal2d_render::Call)) -> u32) {
        let sizes = page_sizes(pages);
        let page_count = pages.len();
        let Renderer { path, path_matrix, image_matrix, stats, fp32, pattern_mode, offset, clip, stream, hangs, .. } = self;
        let mut sink = Sink { path, path_matrix, image_matrix, stats, fp32, pattern_mode: *pattern_mode, clut_page: usize::MAX, offset: *offset, clip: *clip, stream, hangs };
        let size = |p: u32| ((p as usize) < page_count.min(16)).then(|| sizes[p as usize]);
        let triangles = produce(&size, &mut |call| sink.submit(target, pages, call));
        sink.carry(target, pages);
        sink.stats.triangles += triangles;
    }

    /// Submits what is queued (waiting only for a free command buffer).
    pub fn flush(&mut self) {
        timed_flush(&mut self.stats, &mut self.hangs);
    }

    pub fn finish(&mut self) {
        let t0 = super::lv::now_ms();
        let rc = fw::vg_finish();
        let dt = super::lv::now_ms().wrapping_sub(t0);
        if self.hangs.waited(dt, true) {
            self.stats.waited(dt.max(TIMEOUT_MS), 2);
            if dt >= 2 * TIMEOUT_MS {
                self.stats.waited(dt, 2);
            }
        }
        self.hangs.finished();
        self.stats.queued = 0;
        self.stats.queued_px = 0;
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

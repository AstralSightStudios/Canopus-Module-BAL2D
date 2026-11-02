//! `.bal2d` container.
//!
//! ```text
//! header (32 bytes)
//!   0  u32 magic "BL2D"            4  u16 version (1)        6  u16 flags
//!   8  u16 screen width           10  u16 screen height
//!  12  f32 camera cx              16  f32 camera cy          20  f32 camera scale
//!  24  u8 layer count             25  u8 page format         26  u16 reserved
//!  28  u32 section count
//! section table: per section 24 bytes
//!   u32 kind, u16 layer, u16 index, u32 offset, u32 stored length, u32 raw length, u32 crc32(raw)
//! section data (deflate when stored length != raw length)
//! ```
//! Layers are drawn in order (background first). A layer has one SKEL (Spine 4.2 binary),
//! one ATLS (atlas text) and its PAGE sections. INTR holds the interaction table, NAME the
//! display name. An optional BAKE section holds the static bottom of the scene, composited at
//! compile time: it is drawn first, then the layers (whose baked slots are gone). All integers
//! little-endian.
#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub const MAGIC: u32 = u32::from_le_bytes(*b"BL2D");
pub const VERSION: u16 = 1;

pub const KIND_SKEL: u32 = 1;
pub const KIND_ATLAS: u32 = 2;
pub const KIND_PAGE: u32 = 3;
pub const KIND_INTERACTIONS: u32 = 4;
pub const KIND_NAME: u32 = 5;
pub const KIND_BAKE: u32 = 6;
pub const KIND_BAKE_SLOTS: u32 = 7;

/// Page pixel formats (what the GPU samples directly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PageFormat {
    /// 32-bit, premultiplied, bytes B, G, R, A in memory.
    Bgra8888 = 0,
    /// 16-bit, premultiplied, A in bits 15..12, R 11..8, G 7..4, B 3..0 (LE u16).
    Argb4444 = 1,
    /// 8-bit palette index; the payload starts with 256 premultiplied BGRA8888 entries.
    Index8 = 2,
    /// 24-bit, premultiplied: a LE u16 RGB565 colour (R in bits 15..11) then an alpha byte
    /// (VGLite BGRA5658, LVGL ARGB8565). The target is 16-bit, so this loses nothing on screen.
    Argb8565 = 3,
}

impl PageFormat {
    pub fn from_u8(v: u8) -> Option<PageFormat> {
        Some(match v {
            0 => PageFormat::Bgra8888,
            1 => PageFormat::Argb4444,
            2 => PageFormat::Index8,
            3 => PageFormat::Argb8565,
            _ => return None,
        })
    }

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            PageFormat::Bgra8888 => 4,
            PageFormat::Argb4444 => 2,
            PageFormat::Index8 => 1,
            PageFormat::Argb8565 => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Header {
    pub screen_width: u16,
    pub screen_height: u16,
    pub camera_cx: f32,
    pub camera_cy: f32,
    pub camera_scale: f32,
    pub layer_count: u8,
    pub page_format: PageFormat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub kind: u32,
    pub layer: u16,
    pub index: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PackError {
    Truncated,
    Magic,
    Version(u16),
    Corrupt(&'static str),
}

impl core::fmt::Display for PackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PackError::Truncated => write!(f, "pack truncated"),
            PackError::Magic => write!(f, "not a .bal2d pack"),
            PackError::Version(v) => write!(f, "unsupported pack version {v}"),
            PackError::Corrupt(w) => write!(f, "corrupt pack: {w}"),
        }
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Raw deflate into an exactly sized buffer, with the decompressor state on the heap.
pub fn inflate(input: &[u8], out: &mut [u8]) -> Result<(), PackError> {
    inflate_impl(input, out, false)
}

/// Inflates only the first `out.len()` bytes of a stream.
pub fn inflate_prefix(input: &[u8], out: &mut [u8]) -> Result<(), PackError> {
    inflate_impl(input, out, true)
}

fn inflate_impl(input: &[u8], out: &mut [u8], prefix: bool) -> Result<(), PackError> {
    use miniz_oxide::inflate::TINFLStatus;
    use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};
    let layout = core::alloc::Layout::new::<DecompressorOxide>();
    // SAFETY: an all-zero DecompressorOxide is its documented initial state (State::Start = 0).
    let state = unsafe { alloc::alloc::alloc_zeroed(layout) as *mut DecompressorOxide };
    if state.is_null() {
        return Err(PackError::Corrupt("out of memory"));
    }
    let mut state = unsafe { alloc::boxed::Box::from_raw(state) };
    state.init();
    let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let (status, _, written) = decompress(&mut state, input, out, 0, flags);
    match status {
        TINFLStatus::Done if written == out.len() => Ok(()),
        TINFLStatus::HasMoreOutput if prefix && written == out.len() => Ok(()),
        _ => Err(PackError::Corrupt("deflate")),
    }
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Serializes a pack; sections larger than 64 bytes are deflated when that helps.
pub fn write(header: &Header, sections: &[Section], level: u8) -> Vec<u8> {
    let mut stored: Vec<(Vec<u8>, u32, u32)> = Vec::with_capacity(sections.len());
    for s in sections {
        let crc = crc32(&s.data);
        let raw_len = s.data.len() as u32;
        let compressed = if s.data.len() > 64 { miniz_oxide::deflate::compress_to_vec(&s.data, level) } else { Vec::new() };
        if !compressed.is_empty() && compressed.len() < s.data.len() {
            stored.push((compressed, raw_len, crc));
        } else {
            stored.push((s.data.clone(), raw_len, crc));
        }
    }
    let table = 32 + sections.len() * 24;
    let mut out = Vec::with_capacity(table + stored.iter().map(|s| s.0.len()).sum::<usize>());
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&header.screen_width.to_le_bytes());
    out.extend_from_slice(&header.screen_height.to_le_bytes());
    out.extend_from_slice(&header.camera_cx.to_le_bytes());
    out.extend_from_slice(&header.camera_cy.to_le_bytes());
    out.extend_from_slice(&header.camera_scale.to_le_bytes());
    out.push(header.layer_count);
    out.push(header.page_format as u8);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(sections.len() as u32).to_le_bytes());
    let mut offset = table as u32;
    for (s, (data, raw_len, crc)) in sections.iter().zip(&stored) {
        out.extend_from_slice(&s.kind.to_le_bytes());
        out.extend_from_slice(&s.layer.to_le_bytes());
        out.extend_from_slice(&s.index.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&raw_len.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        offset += data.len() as u32;
    }
    for (data, _, _) in &stored {
        out.extend_from_slice(data);
    }
    out
}

/// A section entry of a parsed pack (data not yet inflated).
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub kind: u32,
    pub layer: u16,
    pub index: u16,
    pub offset: u32,
    pub stored_len: u32,
    pub raw_len: u32,
    pub crc: u32,
}

pub struct Reader<'a> {
    pub bytes: &'a [u8],
    pub header: Header,
    pub entries: Vec<Entry>,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Reader<'a>, PackError> {
        if bytes.len() < 32 {
            return Err(PackError::Truncated);
        }
        if le32(bytes, 0) != MAGIC {
            return Err(PackError::Magic);
        }
        let version = le16(bytes, 4);
        if version != VERSION {
            return Err(PackError::Version(version));
        }
        let header = Header {
            screen_width: le16(bytes, 8),
            screen_height: le16(bytes, 10),
            camera_cx: f32::from_bits(le32(bytes, 12)),
            camera_cy: f32::from_bits(le32(bytes, 16)),
            camera_scale: f32::from_bits(le32(bytes, 20)),
            layer_count: bytes[24],
            page_format: PageFormat::from_u8(bytes[25]).ok_or(PackError::Corrupt("page format"))?,
        };
        let count = le32(bytes, 28) as usize;
        if bytes.len() < 32 + count * 24 {
            return Err(PackError::Truncated);
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let o = 32 + i * 24;
            let e = Entry {
                kind: le32(bytes, o),
                layer: le16(bytes, o + 4),
                index: le16(bytes, o + 6),
                offset: le32(bytes, o + 8),
                stored_len: le32(bytes, o + 12),
                raw_len: le32(bytes, o + 16),
                crc: le32(bytes, o + 20),
            };
            if (e.offset as usize).checked_add(e.stored_len as usize).is_none_or(|end| end > bytes.len()) {
                return Err(PackError::Truncated);
            }
            entries.push(e);
        }
        Ok(Reader { bytes, header, entries })
    }

    pub fn find(&self, kind: u32, layer: u16, index: u16) -> Option<&Entry> {
        self.entries.iter().find(|e| e.kind == kind && e.layer == layer && e.index == index)
    }

    pub fn count(&self, kind: u32, layer: u16) -> usize {
        self.entries.iter().filter(|e| e.kind == kind && e.layer == layer).count()
    }

    /// Inflates (if needed) and CRC-checks a section into `out`, which must be exactly
    /// `raw_len` bytes. The decompressor state (~11 KB) lives on the heap, not the stack, and
    /// nothing is allocated beyond it: big sections can go straight into their final buffer.
    pub fn read_into_slice(&self, e: &Entry, out: &mut [u8]) -> Result<(), PackError> {
        if out.len() != e.raw_len as usize {
            return Err(PackError::Corrupt("section size"));
        }
        let stored = &self.bytes[e.offset as usize..(e.offset + e.stored_len) as usize];
        if e.stored_len == e.raw_len {
            out.copy_from_slice(stored);
        } else {
            inflate(stored, out)?;
        }
        if crc32(out) != e.crc {
            return Err(PackError::Corrupt("section checksum"));
        }
        Ok(())
    }

    /// Like `read_into_slice` into a vector resized to the section's size.
    pub fn read_into(&self, e: &Entry, out: &mut Vec<u8>) -> Result<(), PackError> {
        out.clear();
        out.try_reserve_exact(e.raw_len as usize).map_err(|_| PackError::Corrupt("out of memory"))?;
        out.resize(e.raw_len as usize, 0);
        self.read_into_slice(e, out)
    }

    pub fn read(&self, e: &Entry) -> Result<Vec<u8>, PackError> {
        let mut v = Vec::new();
        self.read_into(e, &mut v)?;
        Ok(v)
    }
}

/// Random-access byte source (a file on the watch, a slice on the host).
pub trait Source {
    fn read_at(&mut self, offset: u32, out: &mut [u8]) -> bool;
}

impl Source for &[u8] {
    fn read_at(&mut self, offset: u32, out: &mut [u8]) -> bool {
        match self.get(offset as usize..offset as usize + out.len()) {
            Some(b) => {
                out.copy_from_slice(b);
                true
            }
            None => false,
        }
    }
}

/// The header and section table, read without loading the sections.
#[derive(Clone, Debug)]
pub struct Index {
    pub header: Header,
    pub entries: Vec<Entry>,
}

impl Index {
    pub fn read(src: &mut dyn Source) -> Result<Index, PackError> {
        let mut head = [0u8; 32];
        if !src.read_at(0, &mut head) {
            return Err(PackError::Truncated);
        }
        let count = le32(&head, 28) as usize;
        if count > 4096 {
            return Err(PackError::Corrupt("section count"));
        }
        let mut all = alloc::vec![0u8; 32 + count * 24];
        if !src.read_at(0, &mut all) {
            return Err(PackError::Truncated);
        }
        // Validate with the slice reader (bounds are checked against the table only).
        if le32(&all, 0) != MAGIC {
            return Err(PackError::Magic);
        }
        let version = le16(&all, 4);
        if version != VERSION {
            return Err(PackError::Version(version));
        }
        let header = Header {
            screen_width: le16(&all, 8),
            screen_height: le16(&all, 10),
            camera_cx: f32::from_bits(le32(&all, 12)),
            camera_cy: f32::from_bits(le32(&all, 16)),
            camera_scale: f32::from_bits(le32(&all, 20)),
            layer_count: all[24],
            page_format: PageFormat::from_u8(all[25]).ok_or(PackError::Corrupt("page format"))?,
        };
        let entries = (0..count)
            .map(|i| {
                let o = 32 + i * 24;
                Entry {
                    kind: le32(&all, o),
                    layer: le16(&all, o + 4),
                    index: le16(&all, o + 6),
                    offset: le32(&all, o + 8),
                    stored_len: le32(&all, o + 12),
                    raw_len: le32(&all, o + 16),
                    crc: le32(&all, o + 20),
                }
            })
            .collect();
        Ok(Index { header, entries })
    }

    pub fn find(&self, kind: u32, layer: u16, index: u16) -> Option<&Entry> {
        self.entries.iter().find(|e| e.kind == kind && e.layer == layer && e.index == index)
    }

    pub fn count(&self, kind: u32, layer: u16) -> usize {
        self.entries.iter().filter(|e| e.kind == kind && e.layer == layer).count()
    }
}

/// Reads, inflates and checks a section into `out` (exactly `raw_len` bytes). The compressed
/// bytes are staged in a temporary buffer of `stored_len`.
pub fn read_section(src: &mut dyn Source, e: &Entry, out: &mut [u8]) -> Result<(), PackError> {
    if out.len() != e.raw_len as usize {
        return Err(PackError::Corrupt("section size"));
    }
    if e.stored_len == e.raw_len {
        if !src.read_at(e.offset, out) {
            return Err(PackError::Truncated);
        }
    } else {
        let mut stored = Vec::new();
        stored.try_reserve_exact(e.stored_len as usize).map_err(|_| PackError::Corrupt("out of memory"))?;
        stored.resize(e.stored_len as usize, 0);
        if !src.read_at(e.offset, &mut stored) {
            return Err(PackError::Truncated);
        }
        inflate(&stored, out)?;
    }
    if crc32(out) != e.crc {
        return Err(PackError::Corrupt("section checksum"));
    }
    Ok(())
}

pub fn read_section_vec(src: &mut dyn Source, e: &Entry) -> Result<Vec<u8>, PackError> {
    let mut v = Vec::new();
    v.try_reserve_exact(e.raw_len as usize).map_err(|_| PackError::Corrupt("out of memory"))?;
    v.resize(e.raw_len as usize, 0);
    read_section(src, e, &mut v)?;
    Ok(v)
}

/// The first `out.len()` bytes of a section (page headers), reading at most 4 KB.
pub fn read_section_prefix(src: &mut dyn Source, e: &Entry, out: &mut [u8]) -> Result<(), PackError> {
    if e.stored_len == e.raw_len {
        return if src.read_at(e.offset, out) { Ok(()) } else { Err(PackError::Truncated) };
    }
    let n = (e.stored_len as usize).min(4096);
    let mut stored = alloc::vec![0u8; n];
    if !src.read_at(e.offset, &mut stored) {
        return Err(PackError::Truncated);
    }
    inflate_prefix(&stored, out)
}

/// Page section payload: 8-byte header then pixels (Index8: palette first).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageHeader {
    pub width: u16,
    pub height: u16,
    pub format: PageFormat,
}

pub fn page_header(data: &[u8]) -> Option<PageHeader> {
    if data.len() < 8 {
        return None;
    }
    Some(PageHeader { width: le16(data, 0), height: le16(data, 2), format: PageFormat::from_u8(data[4])? })
}

pub fn page_payload(h: &PageHeader, pixels: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + pixels.len());
    v.extend_from_slice(&h.width.to_le_bytes());
    v.extend_from_slice(&h.height.to_le_bytes());
    v.push(h.format as u8);
    v.extend_from_slice(&[0, 0, 0]);
    v.extend_from_slice(pixels);
    v
}

// ---- baked background ----

/// Where the baked image goes on the pack's full screen; pixels follow the 8-byte header as
/// opaque BGR565 (LE u16, R in bits 15..11), `width * height` of them, rows unpadded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BakeHeader {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

pub fn bake_header(data: &[u8]) -> Option<BakeHeader> {
    if data.len() < 8 {
        return None;
    }
    Some(BakeHeader { x: le16(data, 0), y: le16(data, 2), width: le16(data, 4), height: le16(data, 6) })
}

pub fn bake_payload(h: &BakeHeader, pixels: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + pixels.len());
    for x in [h.x, h.y, h.width, h.height] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(pixels);
    v
}

/// Which slots the baked image replaces: `after_intro` packs keep those slots in the skeleton
/// for the intro (which moves them) and leave them out once the idle loop has settled; other
/// packs have removed them altogether.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BakeSlots {
    pub after_intro: bool,
    /// (layer, slot)
    pub slots: Vec<(u16, u16)>,
}

impl BakeSlots {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(5 + self.slots.len() * 4);
        v.push(self.after_intro as u8);
        v.extend_from_slice(&(self.slots.len() as u32).to_le_bytes());
        for (l, s) in &self.slots {
            v.extend_from_slice(&l.to_le_bytes());
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    pub fn decode(d: &[u8]) -> Option<BakeSlots> {
        let n = u32::from_le_bytes([*d.get(1)?, *d.get(2)?, *d.get(3)?, *d.get(4)?]) as usize;
        if d.len() != 5 + n * 4 {
            return None;
        }
        let slots = (0..n).map(|i| (le16(d, 5 + i * 4), le16(d, 7 + i * 4))).collect();
        Some(BakeSlots { after_intro: d[0] != 0, slots })
    }
}

// ---- interactions ----

/// A drag interaction (head pat, eye follow, cheek pinch...): pressing inside the box and
/// holding past `delay` starts the "ing" clips; the bone's local position follows the finger
/// within [min, max]; releasing plays the end clips and lets the bone spring back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drag {
    pub bone: String,
    pub ing_m: String,
    pub ing_a: String,
    pub end_m: String,
    pub end_a: String,
    /// Absolute local position range of the bone, skeleton units.
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
    /// Fraction of the remaining distance covered per 1/60 s while dragging / after release.
    pub follow: f32,
    pub release: f32,
    /// Seconds the press must last before the drag starts.
    pub delay: f32,
    /// Hit box centered on the bone, skeleton units (0 x 0: the whole screen).
    pub box_w: f32,
    pub box_h: f32,
}

/// Per-clip mix override (negative: use the default mix).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipMix {
    pub name: String,
    pub intro: f32,
    pub outro: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Interactions {
    /// Layer the interactions drive.
    pub layer: u8,
    pub default_mix: f32,
    pub intro: String,
    pub idle: String,
    /// (M, A) clip pairs played on tracks 1 and 2 by a tap, in order.
    pub talks: Vec<(String, String)>,
    /// Checked in order: put small boxes (pat, pinch) before the full-screen eye follow.
    pub drags: Vec<Drag>,
    pub mixes: Vec<ClipMix>,
}

impl Interactions {
    pub fn intro_mix(&self, clip: &str) -> f32 {
        self.mixes.iter().find(|m| m.name == clip && m.intro >= 0.0).map_or(self.default_mix, |m| m.intro)
    }

    pub fn outro_mix(&self, clip: &str) -> f32 {
        self.mixes.iter().find(|m| m.name == clip && m.outro >= 0.0).map_or(self.default_mix, |m| m.outro)
    }
}

struct W(Vec<u8>);

impl W {
    fn s(&mut self, v: &str) {
        self.0.extend_from_slice(&(v.len() as u16).to_le_bytes());
        self.0.extend_from_slice(v.as_bytes());
    }
    fn f(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn n(&mut self, v: usize) {
        self.0.extend_from_slice(&(v as u16).to_le_bytes());
    }
}

struct R<'a>(&'a [u8], usize);

impl R<'_> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.0.get(self.1)?;
        self.1 += 1;
        Some(v)
    }
    fn n(&mut self) -> Option<usize> {
        Some(u16::from_le_bytes([self.u8()?, self.u8()?]) as usize)
    }
    fn s(&mut self) -> Option<String> {
        let n = self.n()?;
        let b = self.0.get(self.1..self.1 + n)?;
        self.1 += n;
        Some(String::from_utf8_lossy(b).into_owned())
    }
    fn f(&mut self) -> Option<f32> {
        let b = self.0.get(self.1..self.1 + 4)?;
        self.1 += 4;
        Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

impl Interactions {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W(Vec::new());
        w.0.push(2); // version
        w.0.push(self.layer);
        w.f(self.default_mix);
        w.s(&self.intro);
        w.s(&self.idle);
        w.n(self.talks.len());
        for (m, a) in &self.talks {
            w.s(m);
            w.s(a);
        }
        w.n(self.drags.len());
        for d in &self.drags {
            for s in [&d.bone, &d.ing_m, &d.ing_a, &d.end_m, &d.end_a] {
                w.s(s);
            }
            for f in [d.min_x, d.min_y, d.max_x, d.max_y, d.follow, d.release, d.delay, d.box_w, d.box_h] {
                w.f(f);
            }
        }
        w.n(self.mixes.len());
        for m in &self.mixes {
            w.s(&m.name);
            w.f(m.intro);
            w.f(m.outro);
        }
        w.0
    }

    pub fn decode(b: &[u8]) -> Option<Interactions> {
        let mut r = R(b, 0);
        if r.u8()? != 2 {
            return None;
        }
        let layer = r.u8()?;
        let default_mix = r.f()?;
        let intro = r.s()?;
        let idle = r.s()?;
        let n = r.n()?;
        let mut talks = Vec::with_capacity(n);
        for _ in 0..n {
            talks.push((r.s()?, r.s()?));
        }
        let n = r.n()?;
        let mut drags = Vec::with_capacity(n);
        for _ in 0..n {
            drags.push(Drag {
                bone: r.s()?,
                ing_m: r.s()?,
                ing_a: r.s()?,
                end_m: r.s()?,
                end_a: r.s()?,
                min_x: r.f()?,
                min_y: r.f()?,
                max_x: r.f()?,
                max_y: r.f()?,
                follow: r.f()?,
                release: r.f()?,
                delay: r.f()?,
                box_w: r.f()?,
                box_h: r.f()?,
            });
        }
        let n = r.n()?;
        let mut mixes = Vec::with_capacity(n);
        for _ in 0..n {
            mixes.push(ClipMix { name: r.s()?, intro: r.f()?, outro: r.f()? });
        }
        Some(Interactions { layer, default_mix, intro, idle, talks, drags, mixes })
    }
}

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
/// Framing per lobby phase ([`PhaseCameras`]); without it every phase uses the header's camera.
pub const KIND_CAMERAS: u32 = 8;
/// A layer's idle loop as ready GPU calls ([`IdleBake`]; section layer = the layer).
pub const KIND_IDLE: u32 = 9;
/// What the talks say ([`Dialogue`]): the lines the watch shows in a speech bubble.
pub const KIND_DIALOGUE: u32 = 10;

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

/// The lobby phases a pack may frame differently (`PhaseCameras`).
pub mod phase {
    /// The idle loop (and anything without its own framing): the header's camera.
    pub const IDLE: u8 = 0;
    pub const INTRO: u8 = 1;
    pub const TALK: u8 = 2;
    /// A drag interaction (head pat and the like), from the drag starting to the spring-back.
    pub const TOUCH: u8 = 3;
    pub const COUNT: usize = 4;
}

/// Per-phase framing: (phase, centre x, centre y, scale), skeleton units like the header's
/// camera. Phases not listed use the header's camera.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhaseCameras {
    pub cameras: Vec<(u8, f32, f32, f32)>,
}

impl PhaseCameras {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(1 + self.cameras.len() * 13);
        v.push(self.cameras.len() as u8);
        for (p, x, y, s) in &self.cameras {
            v.push(*p);
            v.extend_from_slice(&x.to_le_bytes());
            v.extend_from_slice(&y.to_le_bytes());
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    pub fn decode(d: &[u8]) -> Option<PhaseCameras> {
        let n = *d.first()? as usize;
        if d.len() != 1 + n * 13 {
            return None;
        }
        let f = |at: usize| f32::from_bits(le32(d, at));
        let cameras = (0..n)
            .map(|i| {
                let o = 1 + i * 13;
                (d[o], f(o + 1), f(o + 5), f(o + 9))
            })
            .filter(|c| c.3.is_finite() && c.3 > 0.0 && c.1.is_finite() && c.2.is_finite())
            .collect();
        Some(PhaseCameras { cameras })
    }
}

// ---- baked idle ----

/// Triangles of one attachment drawn with one affine in a baked idle.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IdleShape {
    pub slot: u16,
    pub attachment: u32,
    /// Texel the call matrices are anchored at (page texels; the centre of the group's box)
    /// and the box's half size (at least a texel each way).
    pub anchor: [f32; 2],
    pub span: [f32; 2],
    /// Triangle numbers within the attachment's own triangle list (or its [`IdleMesh`] when
    /// the bake has one for it).
    pub tris: Vec<u16>,
    /// Which of the bake's [`IdleBake::regions`] its calls are clipped to: bit 0 what the
    /// layer's later calls always cover ([`IDLE_CLIP_LATER`]), bit 1 what the layers above
    /// always cover, while they are drawn from their bakes too ([`IDLE_CLIP_ABOVE`]); bit 2
    /// ([`IDLE_BEFORE`]): it is drawn before the slots that make the first region, so a call of
    /// it meeting none of its rectangles is left out (as is any call meeting none of the second
    /// region's, while the layers above are drawn from their bakes).
    pub clip: u8,
}

pub const IDLE_CLIP_LATER: u8 = 1;
pub const IDLE_CLIP_ABOVE: u8 = 2;
pub const IDLE_BEFORE: u8 = 4;

/// Finer triangles for an attachment of a baked idle (its own split evenly), so that parts no
/// frame shows (fully transparent texels, hidden under opaque pixels drawn later) can be left
/// out of the shapes: the GPU then fills less. Vertices are texture coordinates like the
/// attachment's (0..1 of the page); the attachment's own come first, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IdleMesh {
    pub attachment: u32,
    pub uvs: Vec<f32>,
    /// Vertex triples, wound like the attachment's.
    pub tris: Vec<u16>,
    /// One bit per vertex (LSB first): no two shapes drawn in one frame have it on their
    /// outlines, so an outline running straight through it may leave it out.
    pub solo: Vec<u8>,
}

/// One call of a baked idle frame: shape, tint (index into [`IdleBake::colors`]) and the
/// texel -> screen affine at the frame ([`IdleBake::matrix`] decodes it) and at the next one
/// (`None`: the same shape's in the next frame).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IdleCall {
    pub shape: u16,
    pub color: u16,
    pub m0: [i16; 6],
    pub m1: Option<[i16; 6]>,
}

/// A layer's idle loop baked into GPU calls by the compiler: the watch draws them, the
/// matrices interpolated between frames, instead of animating, skinning and grouping. Frame `f`
/// is the pose at track time `start + f / rate` of `animation` (looping, `period` seconds);
/// its calls hold over the interval up to frame `f + 1`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IdleBake {
    pub animation: u16,
    pub rate: f32,
    pub period: f32,
    pub start: f32,
    pub shapes: Vec<IdleShape>,
    /// Finer geometry for some attachments (sorted by attachment; see [`IdleMesh`]).
    pub meshes: Vec<IdleMesh>,
    /// What stays visible (screen rectangles x0, y0, x1, y1, pixels, x1 and y1 exclusive) when
    /// a shape's calls are clipped by [`IdleShape::clip`] 1, 2 and 3: the screen less what is
    /// always covered by opaque pixels drawn later, in every frame, with a margin. Empty: none.
    pub regions: [Vec<[i16; 4]>; 3],
    /// One bit per slot (LSB first; empty: none): slots the lobby's talks leave where the idle
    /// loop has them, drawn from the bake during a talk while the rest is drawn live (the main
    /// layer only).
    pub steady: Vec<u8>,
    /// Straight RGBA8 tints (R in the low byte).
    pub colors: Vec<u32>,
    /// Runs of slots drawn live, not from the bake (clipped ones, with their clipping slot): a
    /// call with shape [`IDLE_LIVE`] draws run `color` there.
    pub live: Vec<Vec<u16>>,
    pub frames: u16,
    /// The frames' calls (see [`IdleCursor`]).
    pub stream: Vec<u8>,
}

/// The shape of a call that draws a run of live slots ([`IdleBake::live`]).
pub const IDLE_LIVE: u16 = u16::MAX;

/// Fixed-point scale of [`IdleCall`] matrices: they are the screen positions (x, y) of three
/// texels of the shape, its anchor and the anchor moved by its span along x and along y.
pub const IDLE_POSITION: f32 = 16.0;

impl IdleBake {
    /// The texel -> screen affine of parameters `p` (fixed point, possibly interpolated) for
    /// shape `s` (x' = m[0] tx + m[1] ty + m[2], ...).
    pub fn matrix(s: &IdleShape, p: &[f32; 6]) -> [f32; 6] {
        let k = 1.0 / IDLE_POSITION;
        let (x0, y0) = (p[0] * k, p[1] * k);
        let (a, c) = ((p[2] * k - x0) / s.span[0], (p[3] * k - y0) / s.span[0]);
        let (b, d) = ((p[4] * k - x0) / s.span[1], (p[5] * k - y0) / s.span[1]);
        [a, b, x0 - a * s.anchor[0] - b * s.anchor[1], c, d, y0 - c * s.anchor[0] - d * s.anchor[1]]
    }

    /// Fixed-point parameters of the affine `m` for shape `s` (the inverse of [`IdleBake::matrix`]).
    pub fn params(s: &IdleShape, m: &[f32; 6]) -> [i16; 6] {
        let q = |v: f32| {
            let x = v * IDLE_POSITION;
            (if x >= 0.0 { x + 0.5 } else { x - 0.5 }).clamp(-32768.0, 32767.0) as i16
        };
        let at = |tx: f32, ty: f32| [m[0] * tx + m[1] * ty + m[2], m[3] * tx + m[4] * ty + m[5]];
        let (ax, ay) = (s.anchor[0], s.anchor[1]);
        let (p0, p1, p2) = (at(ax, ay), at(ax + s.span[0], ay), at(ax, ay + s.span[1]));
        [q(p0[0]), q(p0[1]), q(p1[0]), q(p1[1]), q(p2[0]), q(p2[1])]
    }

    /// Encodes `frames` (each a list of calls) as the stream: per frame a call count, per call
    /// shape, color, flags (1: m0 is a delta from the shape's m0 in the previous frame, 2: m1
    /// follows as a delta from m0), m0 and m1.
    pub fn encode_frames(frames: &[Vec<IdleCall>], shapes: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut last: Vec<Option<[i16; 6]>> = alloc::vec![None; shapes];
        let mut seen_now: Vec<Option<[i16; 6]>> = alloc::vec![None; shapes];
        for f in frames {
            out.extend_from_slice(&(f.len() as u16).to_le_bytes());
            seen_now.iter_mut().for_each(|x| *x = None);
            for c in f {
                out.extend_from_slice(&c.shape.to_le_bytes());
                out.extend_from_slice(&c.color.to_le_bytes());
                let base = last.get(c.shape as usize).copied().flatten();
                let flags = base.is_some() as u8 | (c.m1.is_some() as u8) << 1;
                out.push(flags);
                for k in 0..6 {
                    out.extend_from_slice(&c.m0[k].wrapping_sub(base.map_or(0, |b| b[k])).to_le_bytes());
                }
                if let Some(m1) = c.m1 {
                    for k in 0..6 {
                        out.extend_from_slice(&m1[k].wrapping_sub(c.m0[k]).to_le_bytes());
                    }
                }
                if let Some(x) = seen_now.get_mut(c.shape as usize) {
                    *x = Some(c.m0);
                }
            }
            core::mem::swap(&mut last, &mut seen_now);
        }
        out
    }

    /// The mesh standing in for attachment `att`'s own geometry, if any.
    pub fn mesh(&self, att: u32) -> Option<&IdleMesh> {
        self.meshes.binary_search_by_key(&att, |m| m.attachment).ok().map(|i| &self.meshes[i])
    }

    /// Version 3, or 4 when there are meshes or clipped shapes (older modules then go without
    /// the bake rather than draw it wrong). Version 4 stores triangle lists as runs.
    pub fn encode(&self) -> Vec<u8> {
        let v4 = !self.meshes.is_empty() || self.shapes.iter().any(|s| s.clip != 0) || self.regions.iter().any(|r| !r.is_empty()) || !self.steady.is_empty();
        let mut v = Vec::new();
        v.push(if v4 { 4u8 } else { 3 });
        v.extend_from_slice(&self.animation.to_le_bytes());
        for f in [self.rate, self.period, self.start] {
            v.extend_from_slice(&f.to_le_bytes());
        }
        v.extend_from_slice(&(self.shapes.len() as u16).to_le_bytes());
        for s in &self.shapes {
            v.extend_from_slice(&s.slot.to_le_bytes());
            v.extend_from_slice(&s.attachment.to_le_bytes());
            for f in [s.anchor[0], s.anchor[1], s.span[0], s.span[1]] {
                v.extend_from_slice(&f.to_le_bytes());
            }
            if v4 {
                v.push(s.clip);
                let mut runs: Vec<(u16, u16)> = Vec::new();
                for &t in &s.tris {
                    match runs.last_mut() {
                        Some((a, n)) if a.wrapping_add(*n) == t && *n < u16::MAX => *n += 1,
                        _ => runs.push((t, 1)),
                    }
                }
                v.extend_from_slice(&(runs.len() as u16).to_le_bytes());
                for (a, n) in runs {
                    v.extend_from_slice(&a.to_le_bytes());
                    v.extend_from_slice(&n.to_le_bytes());
                }
            } else {
                v.extend_from_slice(&(s.tris.len() as u16).to_le_bytes());
                for t in &s.tris {
                    v.extend_from_slice(&t.to_le_bytes());
                }
            }
        }
        if v4 {
            v.extend_from_slice(&(self.meshes.len() as u16).to_le_bytes());
            for m in &self.meshes {
                v.extend_from_slice(&m.attachment.to_le_bytes());
                v.extend_from_slice(&((m.uvs.len() / 2) as u16).to_le_bytes());
                for f in &m.uvs {
                    v.extend_from_slice(&f.to_le_bytes());
                }
                v.extend_from_slice(&((m.tris.len() / 3) as u16).to_le_bytes());
                for t in &m.tris {
                    v.extend_from_slice(&t.to_le_bytes());
                }
                let mut solo = m.solo.clone();
                solo.resize(m.uvs.len() / 2 / 8 + 1, 0);
                v.extend_from_slice(&solo[..m.uvs.len() / 2 / 8 + 1]);
            }
            for r in &self.regions {
                v.extend_from_slice(&(r.len() as u16).to_le_bytes());
                for q in r {
                    for c in q {
                        v.extend_from_slice(&c.to_le_bytes());
                    }
                }
            }
            v.extend_from_slice(&(self.steady.len() as u16).to_le_bytes());
            v.extend_from_slice(&self.steady);
        }
        v.extend_from_slice(&(self.colors.len() as u16).to_le_bytes());
        for c in &self.colors {
            v.extend_from_slice(&c.to_le_bytes());
        }
        v.extend_from_slice(&(self.live.len() as u16).to_le_bytes());
        for run in &self.live {
            v.extend_from_slice(&(run.len() as u16).to_le_bytes());
            for s in run {
                v.extend_from_slice(&s.to_le_bytes());
            }
        }
        v.extend_from_slice(&self.frames.to_le_bytes());
        v.extend_from_slice(&(self.stream.len() as u32).to_le_bytes());
        v.extend_from_slice(&self.stream);
        v
    }

    pub fn decode(d: &[u8]) -> Option<IdleBake> {
        Self::decode_owned(d.to_vec())
    }

    /// `decode`, keeping the section's own buffer as the stream (no second copy of it).
    pub fn decode_owned(mut d: Vec<u8>) -> Option<IdleBake> {
        let mut r = Bytes { d: &d, at: 0 };
        let version = r.u8()?;
        if version != 3 && version != 4 {
            return None;
        }
        let animation = r.u16()?;
        let (rate, period, start) = (r.f32()?, r.f32()?, r.f32()?);
        if !(rate > 0.0 && period > 0.0 && start.is_finite()) {
            return None;
        }
        let n = r.u16()? as usize;
        let mut shapes = Vec::with_capacity(n);
        for _ in 0..n {
            let slot = r.u16()?;
            let attachment = r.u32()?;
            let anchor = [r.f32()?, r.f32()?];
            let span = [r.f32()?, r.f32()?];
            if !(span[0] > 0.0 && span[1] > 0.0) {
                return None;
            }
            let clip = if version == 4 { r.u8()? } else { 0 };
            let k = r.u16()? as usize;
            let mut tris = Vec::with_capacity(k);
            if version == 4 {
                for _ in 0..k {
                    let (a, n) = (r.u16()?, r.u16()?);
                    if a as usize + n as usize > u16::MAX as usize + 1 {
                        return None;
                    }
                    tris.extend((0..n).map(|i| a + i));
                }
                tris.shrink_to_fit();
            } else {
                for _ in 0..k {
                    tris.push(r.u16()?);
                }
            }
            shapes.push(IdleShape { slot, attachment, anchor, span, tris, clip });
        }
        let mut meshes = Vec::new();
        let mut regions: [Vec<[i16; 4]>; 3] = Default::default();
        let mut steady = Vec::new();
        if version == 4 {
            let nm = r.u16()? as usize;
            meshes.reserve_exact(nm);
            for _ in 0..nm {
                let attachment = r.u32()?;
                let nv = r.u16()? as usize;
                let mut uvs = Vec::with_capacity(nv * 2);
                for _ in 0..nv * 2 {
                    uvs.push(r.f32()?);
                }
                let nt = r.u16()? as usize;
                let mut tris = Vec::with_capacity(nt * 3);
                for _ in 0..nt * 3 {
                    let t = r.u16()?;
                    if t as usize >= nv {
                        return None;
                    }
                    tris.push(t);
                }
                let solo = r.d.get(r.at..r.at + nv / 8 + 1)?.to_vec();
                r.at += nv / 8 + 1;
                meshes.push(IdleMesh { attachment, uvs, tris, solo });
            }
            if !meshes.windows(2).all(|w| w[0].attachment < w[1].attachment) {
                return None;
            }
            for reg in &mut regions {
                let n = r.u16()? as usize;
                reg.reserve_exact(n);
                for _ in 0..n {
                    let q = [r.u16()? as i16, r.u16()? as i16, r.u16()? as i16, r.u16()? as i16];
                    if q[2] <= q[0] || q[3] <= q[1] {
                        return None;
                    }
                    reg.push(q);
                }
            }
            let n = r.u16()? as usize;
            steady = r.d.get(r.at..r.at + n)?.to_vec();
            r.at += n;
        }
        let nc = r.u16()? as usize;
        let mut colors = Vec::with_capacity(nc);
        for _ in 0..nc {
            colors.push(r.u32()?);
        }
        let nl = r.u16()? as usize;
        let mut live = Vec::with_capacity(nl);
        for _ in 0..nl {
            let k = r.u16()? as usize;
            let mut run = Vec::with_capacity(k);
            for _ in 0..k {
                run.push(r.u16()?);
            }
            live.push(run);
        }
        let frames = r.u16()?;
        let len = r.u32()? as usize;
        let at = r.at;
        if at + len != d.len() || frames == 0 {
            return None;
        }
        d.drain(..at);
        // The header (shapes, colours, runs) now lives in its own vectors: give its room back
        // (shrinking leaves the block where it is). Kept, it made a baked idle take about 1.6 to
        // 2 times its size on the watch.
        d.shrink_to_fit();
        Some(IdleBake { animation, rate, period, start, shapes, meshes, regions, steady, colors, live, frames, stream: d })
    }
}

struct Bytes<'a> {
    d: &'a [u8],
    at: usize,
}

impl Bytes<'_> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.d.get(self.at)?;
        self.at += 1;
        Some(v)
    }
    fn u16(&mut self) -> Option<u16> {
        let b = self.d.get(self.at..self.at + 2)?;
        self.at += 2;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.d.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f32(&mut self) -> Option<f32> {
        self.u32().map(f32::from_bits)
    }
}

/// Decodes an [`IdleBake`] stream frame after frame (the matrices are deltas from the frame
/// before, so frames come in order; after the last it starts over).
#[derive(Clone, Debug, Default)]
pub struct IdleCursor {
    at: usize,
    /// The frame `next` decodes.
    pub frame: u16,
    /// Per shape: its matrix in the frame it last appeared in, and that frame's number in
    /// decoding order (`tick`): a delta applies when it appeared in the frame before. Updated in
    /// place (each shape appears once per frame): no per-frame reset of every shape.
    last: Vec<[i16; 6]>,
    seen: Vec<u32>,
    tick: u32,
}

impl IdleCursor {
    pub fn new(shapes: usize) -> IdleCursor {
        IdleCursor { at: 0, frame: 0, last: alloc::vec![[0; 6]; shapes], seen: alloc::vec![0; shapes], tick: 1 }
    }

    pub fn rewind(&mut self) {
        self.at = 0;
        self.frame = 0;
        // Nothing appeared in "the frame before" the first one.
        self.tick += 1;
    }

    /// Decodes frame `self.frame` into `out` (replacing it) and moves on; `None` if the stream
    /// is malformed (the cursor is then rewound).
    pub fn next(&mut self, bake: &IdleBake, out: &mut Vec<IdleCall>) -> Option<u16> {
        if self.frame >= bake.frames {
            self.rewind();
        }
        out.clear();
        let d = &bake.stream[..];
        let mut at = self.at;
        let prev = self.tick;
        let tick = prev + 1;
        let ok = (|| -> Option<()> {
            let n = u16::from_le_bytes([*d.get(at)?, *d.get(at + 1)?]) as usize;
            at += 2;
            out.reserve(n);
            for _ in 0..n {
                // The longest record: shape, colour, flags, two matrices.
                let rec = d.get(at..at + 29).or_else(|| d.get(at..))?;
                if rec.len() < 5 {
                    return None;
                }
                let shape = u16::from_le_bytes([rec[0], rec[1]]);
                let color = u16::from_le_bytes([rec[2], rec[3]]);
                let flags = rec[4];
                let len = 5 + 12 + if flags & 2 != 0 { 12 } else { 0 };
                if rec.len() < len {
                    return None;
                }
                let at_m = |k: usize| i16::from_le_bytes([rec[5 + 2 * k], rec[6 + 2 * k]]);
                let s = shape as usize;
                let base = if flags & 1 != 0 {
                    if self.seen.get(s).copied() != Some(prev) {
                        return None;
                    }
                    self.last[s]
                } else {
                    [0; 6]
                };
                let mut m0 = [0i16; 6];
                for k in 0..6 {
                    m0[k] = base[k].wrapping_add(at_m(k));
                }
                let m1 = (flags & 2 != 0).then(|| {
                    let mut m = [0i16; 6];
                    for k in 0..6 {
                        m[k] = m0[k].wrapping_add(at_m(6 + k));
                    }
                    m
                });
                // Live-run markers name no shape.
                if s < self.last.len() {
                    self.last[s] = m0;
                    self.seen[s] = tick;
                }
                out.push(IdleCall { shape, color, m0, m1 });
                at += len;
            }
            Some(())
        })();
        if ok.is_none() {
            self.rewind();
            out.clear();
            return None;
        }
        self.tick = tick;
        let f = self.frame;
        self.at = at;
        self.frame += 1;
        Some(f)
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

/// One line of a talk, shown in the bubble for `ms` milliseconds.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Line {
    pub text: String,
    pub ms: u32,
    /// An inner thought (the game draws it in parentheses, in a softer bubble).
    pub thought: bool,
}

/// The lines of one talk, keyed by its animation's name without the `_M` / `_A` suffix
/// (`Talk_01` for the pair `Talk_01_M`, `Talk_01_A`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TalkLines {
    pub talk: String,
    pub lines: Vec<Line>,
}

/// What the lobby's talks say, from the game's memorial-lobby dialogue table.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Dialogue {
    pub talks: Vec<TalkLines>,
}

/// `Talk_01_M` -> `Talk_01`: a talk animation's name without its track suffix.
pub fn talk_key(anim: &str) -> &str {
    let base = anim.rsplit('/').next().unwrap_or(anim);
    match base.rsplit_once('_') {
        Some((k, s)) if s.eq_ignore_ascii_case("m") || s.eq_ignore_ascii_case("a") => k,
        _ => base,
    }
}

impl Dialogue {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W(Vec::new());
        w.0.push(1); // version
        w.n(self.talks.len());
        for t in &self.talks {
            w.s(&t.talk);
            w.n(t.lines.len());
            for l in &t.lines {
                w.0.push(l.thought as u8);
                w.0.extend_from_slice(&l.ms.min(u16::MAX as u32 * 10).div_ceil(10).to_le_bytes()[..2]);
                w.s(&l.text);
            }
        }
        w.0
    }

    pub fn decode(b: &[u8]) -> Option<Dialogue> {
        let mut r = R(b, 0);
        if r.u8()? != 1 {
            return None;
        }
        let n = r.n()?;
        let mut talks = Vec::with_capacity(n.min(256));
        for _ in 0..n {
            let talk = r.s()?;
            let k = r.n()?;
            let mut lines = Vec::with_capacity(k.min(64));
            for _ in 0..k {
                let thought = r.u8()? != 0;
                let ms = r.n()? as u32 * 10;
                lines.push(Line { text: r.s()?, ms, thought });
            }
            talks.push(TalkLines { talk, lines });
        }
        Some(Dialogue { talks })
    }

    /// The lines of the talk played by the clip pair (m, a), if the dialogue has them.
    pub fn lines_for(&self, m: &str, a: &str) -> Option<&[Line]> {
        let (km, ka) = (talk_key(m), talk_key(a));
        self.talks
            .iter()
            .find(|t| t.talk.eq_ignore_ascii_case(km) || t.talk.eq_ignore_ascii_case(ka))
            .map(|t| t.lines.as_slice())
    }
}

#[cfg(test)]
mod dialogue_tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn a_dialogue_round_trips_and_finds_its_talks() {
        let d = Dialogue {
            talks: vec![
                TalkLines { talk: "Talk_01".into(), lines: vec![Line { text: "老师，过来这边。\n要和我一起坐吗？".into(), ms: 5300, thought: false }] },
                TalkLines {
                    talk: "Talk_02".into(),
                    lines: vec![Line { text: "（只能更加努力了。）".into(), ms: 8170, thought: true }, Line { text: "现在…".into(), ms: 7000, thought: false }],
                },
            ],
        };
        let back = Dialogue::decode(&d.encode()).unwrap();
        assert_eq!(back.talks[0], d.talks[0]);
        // Durations are kept in 10 ms steps, rounded up.
        assert_eq!(back.talks[1].lines[0].ms, 8170);
        assert_eq!(back.lines_for("Talk_02_M", "Talk_02_A").map(|l| l.len()), Some(2));
        assert_eq!(back.lines_for("Talk_03_M", "Talk_03_A"), None);
        assert_eq!(talk_key("Talk_01_M"), "Talk_01");
        assert_eq!(talk_key("Talk_01"), "Talk_01");
    }
}

#[cfg(test)]
mod idle_tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn a_baked_idle_round_trips_and_decodes_frame_after_frame() {
        let shape = |n: u16| IdleShape { slot: n, attachment: 7 + n as u32, anchor: [10.5, 20.0], span: [4.0, 9.0], tris: vec![0, 2, n], clip: 0 };
        let call = |shape: u16, x: i16, m1: Option<[i16; 6]>| IdleCall { shape, color: 1, m0: [x, 2, 3, -4, 5, 6], m1 };
        let frames = vec![
            vec![call(0, 100, None), call(1, -7, Some([9; 6]))],
            vec![call(1, 30, None)],
            vec![call(0, 101, Some([1, 2, 3, 4, 5, 6])), call(1, 31, None)],
        ];
        let stream = IdleBake::encode_frames(&frames, 2);
        let bake = IdleBake { animation: 3, rate: 30.0, period: 0.1, start: 0.25, shapes: vec![shape(0), shape(1)], meshes: Vec::new(), regions: Default::default(), steady: Vec::new(), colors: vec![0xFFFF_FFFF, 0x8000_00FF], live: vec![vec![4, 5]], frames: 3, stream };
        let back = IdleBake::decode(&bake.encode()).unwrap();
        assert_eq!(back, bake);
        let mut c = IdleCursor::new(2);
        let mut out = Vec::new();
        for round in 0..2 {
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(c.next(&back, &mut out), Some(i as u16), "round {round}");
                assert_eq!(&out, f);
            }
        }
        // Parameters and matrices invert each other (to the fixed point).
        let s = shape(0);
        let m = [0.9f32, -0.3, 120.0, 0.25, 1.1, -40.0];
        let p = IdleBake::params(&s, &m).map(|v| v as f32);
        let m2 = IdleBake::matrix(&s, &p);
        for (tx, ty) in [(10.5, 20.0), (0.0, 0.0), (30.0, 40.0)] {
            let a = [m[0] * tx + m[1] * ty + m[2], m[3] * tx + m[4] * ty + m[5]];
            let b = [m2[0] * tx + m2[1] * ty + m2[2], m2[3] * tx + m2[4] * ty + m2[5]];
            assert!((a[0] - b[0]).abs() < 0.2 && (a[1] - b[1]).abs() < 0.2, "{a:?} {b:?}");
        }
    }

    #[test]
    fn meshes_and_clipped_shapes_round_trip_as_version_4() {
        let mut a = IdleShape { slot: 1, attachment: 9, anchor: [1.0, 2.0], span: [3.0, 4.0], tris: vec![0, 1, 2, 7, 8, 3], clip: IDLE_CLIP_ABOVE };
        let b = IdleShape { clip: 0, ..a.clone() };
        let stream = IdleBake::encode_frames(&[vec![IdleCall { shape: 0, color: 0, m0: [0; 6], m1: None }]], 2);
        let mesh = |att: u32| IdleMesh { attachment: att, uvs: vec![0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.5, 0.5], tris: vec![0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4], solo: vec![0x12] };
        let regions = [vec![[0, 0, 10, 20]], Vec::new(), vec![[5, 5, 6, 6], [-3, 0, 400, 480]]];
        let bake = IdleBake { animation: 0, rate: 30.0, period: 1.0, start: 0.0, shapes: vec![a.clone(), b], meshes: vec![mesh(2), mesh(9)], regions, steady: vec![0b101, 1], colors: vec![0], live: Vec::new(), frames: 1, stream };
        let bytes = bake.encode();
        assert_eq!(bytes[0], 4);
        let back = IdleBake::decode(&bytes).unwrap();
        assert_eq!(back, bake);
        assert_eq!(back.mesh(9).map(|m| m.tris.len()), Some(12));
        assert!(back.mesh(3).is_none());
        // Without either, the old version.
        a.clip = 0;
        let old = IdleBake { shapes: vec![a], meshes: Vec::new(), regions: Default::default(), steady: Vec::new(), ..bake };
        assert_eq!(old.encode()[0], 3);
        assert_eq!(IdleBake::decode(&old.encode()).unwrap(), old);
    }
}

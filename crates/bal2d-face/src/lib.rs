//! Adds a BAL2D layer to a finished Xiaomi Vela watchface.
//!
//! The layer is a placeholder Image record (a fully transparent Indexed8 image, RLE-compressed,
//! whose palette also carries [`MARKER_PALETTE`]) with UID [`MARKER_UID`], placed by a new Layout
//! record at a chosen position in the layout list — the list order is the z order. On the watch
//! the firmware instantiates it as a `wfimg` LVGL object; the BAL2D module finds that object by
//! class and UID and draws into it.
//!
//! The patch only appends: each patched theme's Image and Layout record tables are copied to
//! the end of the file with the new records inserted, the theme row is pointed at the copies,
//! and the payloads are appended. Every existing absolute address stays valid, so resource types
//! this tool does not understand are carried over untouched.
//!
//! Firmware facts the layout follows (`binary_parse_get_layout_list`; decompiled from the Band 11
//! .179 image, the same `wf` parser runs on the Band 10 Pro .043 this targets, verified there):
//! * the layout payloads (16 bytes each) are read in one piece starting at the *first* record's
//!   address, so they must be contiguous and in record order: the whole block is rewritten;
//! * images are looked up by their full UID with the theme's color index ORed into bits 19..22, so
//!   the placeholder gets one image record per color index;
//! * images must be RLE method 0 or 1 (`decode_image_record` rejects any other method).

pub mod sha256;
pub mod zip;

pub const MAGIC: u32 = 0x1234_A55A;
const HEADER_BASE: usize = 168;
const RECORD: usize = 16;
const TYPE_LAYOUT: usize = 0;
const TYPE_IMAGE: usize = 2;
/// Every layout payload is this long (the firmware steps through them 16 bytes at a time).
const LAYOUT_PAYLOAD: usize = 16;

/// Placeholder image UID. Firmware UID bits: 24..27 type (2 = image), 23 "builtin" (looked up in
/// `/watchface/builtin/builtin.bin`, not in the face), 19..22 the color index the firmware ORs in,
/// 0..18 the index. So bits 19..23 must be clear; the module masks the color bits when matching.
pub const MARKER_UID: u32 = 0x0207_BA12;
pub const MARKER_LAYOUT_UID: u32 = 0x0007_BA13;
const COLOR_BITS: u32 = 0x0078_0000;
const BUILTIN_BIT: u32 = 0x0080_0000;
/// Palette entries 1..=3 of the placeholder (alpha byte 0, so still invisible): "BAL", "2Dk",
/// format version. A second way to recognise the image.
pub const MARKER_PALETTE: [u32; 3] = [0x004C_4142, 0x006B_4432, 0x0000_0001];

#[derive(Debug, Clone, PartialEq)]
pub enum FaceError {
    Truncated,
    Magic,
    Protocol(u32),
    Themes,
    AlreadyPatched,
    Zip(&'static str),
    NoResourceBin,
}

impl std::fmt::Display for FaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FaceError::Truncated => write!(f, "watchface file is truncated"),
            FaceError::Magic => write!(f, "not a Xiaomi Vela watchface BIN"),
            FaceError::Protocol(p) => write!(f, "unsupported watchface protocol 0x{p:08x} (need 8.x or 9.x)"),
            FaceError::Themes => write!(f, "could not decode the theme tables"),
            FaceError::AlreadyPatched => write!(f, "the watchface already has a BAL2D layer"),
            FaceError::Zip(w) => write!(f, "MWZ archive: {w}"),
            FaceError::NoResourceBin => write!(f, "MWZ archive has no resource.bin"),
        }
    }
}

fn u32_at(d: &[u8], o: usize) -> Result<u32, FaceError> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or(FaceError::Truncated)
}

fn put32(d: &mut [u8], o: usize, v: u32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

#[derive(Debug, Clone)]
pub struct Table {
    pub count: usize,
    pub address: usize,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub offset: usize,
    pub name: String,
    pub aod: bool,
    /// Color variants (the color index the firmware ORs into resource UIDs).
    pub colors: usize,
    pub tables: Vec<Table>,
}

#[derive(Debug, Clone)]
pub struct LayoutInfo {
    pub uid: u32,
    pub resource_uid: u32,
    pub x: i16,
    pub y: i16,
}

#[derive(Debug, Clone)]
pub struct Face {
    pub protocol: u32,
    pub name: String,
    pub themes: Vec<Theme>,
    table_count: usize,
}

fn c_string(d: &[u8], o: usize, n: usize) -> String {
    let b = d.get(o..o + n).unwrap_or(&[]);
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Reads one candidate theme layout (basic or extended rows) and scores how plausible it is.
fn themes_candidate(d: &[u8], header_len: usize, theme_count: usize, tables: usize, extended: bool) -> Option<(usize, Vec<Theme>)> {
    let base = 8 + tables * 8;
    let mut offset = header_len;
    let mut score = 0usize;
    let mut themes = Vec::new();
    for _ in 0..theme_count {
        d.get(offset..offset + base)?;
        let mut ts = Vec::new();
        for t in 0..tables {
            let o = offset + 8 + t * 8;
            let count = u32_at(d, o).ok()? as usize;
            let address = u32_at(d, o + 4).ok()? as usize;
            if count > 100_000 {
                return None;
            }
            if count > 0 {
                d.get(address..address + count * RECORD)?;
                for r in 0..count.min(8) {
                    let uid = u32_at(d, address + r * RECORD).ok()?;
                    if (uid >> 24) as usize == t {
                        score += 3;
                    }
                }
            } else {
                score += 1;
            }
            ts.push(Table { count, address });
        }
        let mut name = String::new();
        let mut aod = false;
        let mut color_count = 0;
        let mut row = base;
        if extended {
            let ext = offset + base;
            d.get(ext..ext + 72)?;
            name = c_string(d, ext, 64);
            if !name.is_empty() {
                score += 12;
            }
            let type_and_count = u32_at(d, ext + 68).ok()?;
            if type_and_count & 3 > 1 {
                return None;
            }
            aod = type_and_count & 3 == 1;
            let colors = (type_and_count >> 2) as usize;
            if colors > 256 {
                return None;
            }
            color_count = colors;
            row += 72 + colors * 4;
        }
        themes.push(Theme { offset, name, aod, colors: color_count, tables: ts });
        offset += row;
    }
    Some((score, themes))
}

impl Face {
    pub fn parse(d: &[u8]) -> Result<Face, FaceError> {
        if d.len() < HEADER_BASE {
            return Err(FaceError::Truncated);
        }
        if u32_at(d, 0)? != MAGIC {
            return Err(FaceError::Magic);
        }
        let protocol = u32_at(d, 16)?;
        let major = (protocol >> 8) & 0xff;
        if !(8..=9).contains(&major) {
            return Err(FaceError::Protocol(protocol));
        }
        let color_groups = d[24] as usize;
        let theme_count = d[28] as usize;
        let recolors = d[29] as usize;
        let header_len = HEADER_BASE + color_groups.max(recolors) * 4;
        let tables = if major > 8 { 12 } else { 10 };
        let best = [false, true]
            .into_iter()
            .filter_map(|ext| themes_candidate(d, header_len, theme_count, tables, ext).map(|c| (c.0, ext, c.1)))
            .max_by_key(|c| (c.0, c.1 as usize))
            .ok_or(FaceError::Themes)?;
        let name = if u32_at(d, 104)? == u32::MAX { String::new() } else { c_string(d, 104, 64) };
        Ok(Face { protocol, name, themes: best.2, table_count: tables })
    }

    /// The layout list of a theme in z order (bottom first).
    pub fn layouts(&self, d: &[u8], theme: usize) -> Vec<LayoutInfo> {
        let mut out = Vec::new();
        let t = &self.themes[theme].tables[TYPE_LAYOUT];
        for i in 0..t.count {
            let r = t.address + i * RECORD;
            let (Ok(uid), Ok(addr), Ok(len)) = (u32_at(d, r), u32_at(d, r + 8), u32_at(d, r + 12)) else { continue };
            if len < 8 {
                continue;
            }
            let p = addr as usize;
            let Some(b) = d.get(p..p + 8) else { continue };
            out.push(LayoutInfo {
                uid,
                resource_uid: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                x: i16::from_le_bytes([b[4], b[5]]),
                y: i16::from_le_bytes([b[6], b[7]]),
            });
        }
        out
    }

    pub fn has_marker(&self, d: &[u8]) -> bool {
        self.themes.iter().any(|t| {
            let img = &t.tables[TYPE_IMAGE];
            (0..img.count).any(|i| {
                let r = img.address + i * RECORD;
                if u32_at(d, r).is_ok_and(|u| u & !COLOR_BITS == MARKER_UID) {
                    return true;
                }
                match (u32_at(d, r + 8), u32_at(d, r + 12)) {
                    (Ok(a), Ok(n)) => d.get(a as usize..(a as usize + n as usize)).is_some_and(is_marker_block),
                    _ => false,
                }
            })
        })
    }
}

/// Whether an image block is the BAL2D placeholder.
pub fn is_marker_block(b: &[u8]) -> bool {
    let mut magic = Vec::with_capacity(12);
    for w in MARKER_PALETTE {
        magic.extend_from_slice(&w.to_le_bytes());
    }
    b.len() >= 12 + 8 + 3 + 12 && b[0] == 16 && b[12 + 8..12 + 8 + 3] == [4, 0, 0x80 | 12] && b[23..35] == magic[..]
}

/// Where and how large the layer is.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    /// Index in the layout list to insert at (0 = bottom, `usize::MAX` = top).
    pub z: usize,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

/// Image block: Indexed8, RLE method 1, fully transparent (every pixel uses palette entry 0).
/// This is the format and compression every official face uses (Band 10 Pro .043 creates no
/// object for an A8 / RLE method 2 image). Palette entries 1..=3 carry [`MARKER_PALETTE`].
fn marker_image(w: u16, h: u16) -> Vec<u8> {
    let n = 1024 + w as usize * h as usize;
    let mut rle = Vec::new();
    rle.extend_from_slice(&0x5AA5_21E0u32.to_le_bytes());
    rle.extend_from_slice(&(((n as u32) << 4) | 1).to_le_bytes());
    // Method 1: a control byte with bit 7 clear repeats the next unit (control & 0x7f) times,
    // with bit 7 set copies the next (control & 0x7f) units.
    rle.extend_from_slice(&[4, 0, 0x80 | 12]);
    for word in MARKER_PALETTE {
        rle.extend_from_slice(&word.to_le_bytes());
    }
    let mut left = n - 16;
    while left > 0 {
        let run = left.min(127);
        rle.push(run as u8);
        rle.push(0);
        left -= run;
    }
    let mut out = Vec::with_capacity(12 + rle.len());
    out.push(16); // Indexed8
    out.push(1 << 2); // RLE method 1, no recolor
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&(rle.len() as u32).to_le_bytes());
    out.extend_from_slice(&rle);
    out
}

fn align4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

/// Returns the patched BIN. AOD themes are left alone.
pub fn patch_bin(d: &[u8], place: Placement) -> Result<Vec<u8>, FaceError> {
    let face = Face::parse(d)?;
    debug_assert_eq!(MARKER_UID & (COLOR_BITS | BUILTIN_BIT), 0);
    if face.has_marker(d) {
        return Err(FaceError::AlreadyPatched);
    }
    let mut out = d.to_vec();
    align4(&mut out);
    let image_addr = out.len();
    let image = marker_image(place.width, place.height);
    out.extend_from_slice(&image);
    align4(&mut out);
    let mut marker_payload = [0u8; LAYOUT_PAYLOAD];
    put32(&mut marker_payload, 0, MARKER_UID);
    marker_payload[4..6].copy_from_slice(&place.x.to_le_bytes());
    marker_payload[6..8].copy_from_slice(&place.y.to_le_bytes());
    let read = |table: &Table| -> Vec<[u8; RECORD]> {
        (0..table.count)
            .map(|i| {
                let mut r = [0u8; RECORD];
                r.copy_from_slice(&d[table.address + i * RECORD..table.address + (i + 1) * RECORD]);
                r
            })
            .collect()
    };
    let uid = |r: &[u8; RECORD]| u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
    let mut patched = 0;
    for theme in &face.themes {
        if theme.aod {
            continue;
        }
        // Image table: one record per color index, in UID order when the table is sorted.
        let mut records = read(&theme.tables[TYPE_IMAGE]);
        let sorted = records.windows(2).all(|w| uid(&w[0]) <= uid(&w[1]));
        for color in 0..theme.colors.clamp(1, 16) as u32 {
            let mut r = [0u8; RECORD];
            put32(&mut r, 0, MARKER_UID | color << 19);
            put32(&mut r, 8, image_addr as u32);
            put32(&mut r, 12, image.len() as u32);
            let at = if sorted { records.partition_point(|x| uid(x) < uid(&r)) } else { records.len() };
            records.insert(at, r);
        }
        let new_img = out.len();
        for r in &records {
            out.extend_from_slice(r);
        }
        // Layouts: records and their payloads, both in the new order, the payloads contiguous.
        let mut layouts: Vec<([u8; RECORD], [u8; LAYOUT_PAYLOAD])> = read(&theme.tables[TYPE_LAYOUT])
            .into_iter()
            .map(|r| {
                let addr = u32::from_le_bytes([r[8], r[9], r[10], r[11]]) as usize;
                let mut p = [0u8; LAYOUT_PAYLOAD];
                let have = d.len().saturating_sub(addr).min(LAYOUT_PAYLOAD);
                p[..have].copy_from_slice(&d[addr..addr + have]);
                (r, p)
            })
            .collect();
        let mut marker_record = [0u8; RECORD];
        put32(&mut marker_record, 0, MARKER_LAYOUT_UID);
        layouts.insert(place.z.min(layouts.len()), (marker_record, marker_payload));
        let payloads = out.len();
        for (_, p) in &layouts {
            out.extend_from_slice(p);
        }
        let new_lay = out.len();
        for (i, (r, _)) in layouts.iter().enumerate() {
            let mut r = *r;
            put32(&mut r, 8, (payloads + i * LAYOUT_PAYLOAD) as u32);
            put32(&mut r, 12, LAYOUT_PAYLOAD as u32);
            out.extend_from_slice(&r);
        }
        let row = theme.offset + 8;
        put32(&mut out, row + TYPE_IMAGE * 8, records.len() as u32);
        put32(&mut out, row + TYPE_IMAGE * 8 + 4, new_img as u32);
        put32(&mut out, row + TYPE_LAYOUT * 8, layouts.len() as u32);
        put32(&mut out, row + TYPE_LAYOUT * 8 + 4, new_lay as u32);
        patched += 1;
    }
    if patched == 0 {
        return Err(FaceError::Themes);
    }
    let _ = face.table_count;
    let id = derived_id(d, &place);
    set_identity(&mut out, &id, NAME_SUFFIX);
    Ok(out)
}

/// Package id (ASCII, NUL padded) and display name (UTF-8, NUL padded; `u32::MAX`: none) in the
/// BIN header.
const ID_OFFSET: usize = 0x28;
const ID_FIELD: usize = 32;
const NAME_OFFSET: usize = 104;
const NAME_FIELD: usize = 64;
/// Appended to the patched face's name, so it is told apart from the original in the list.
pub const NAME_SUFFIX: &str = " BA";

/// The package id the original face uses ("" when there is none).
pub fn package_id(bin: &[u8]) -> String {
    c_string(bin, ID_OFFSET, ID_FIELD)
}

/// A package id for the patched face: "B2D" + 9 digits from the original id, name and
/// placement. Many community faces share one id (often "000000000000"), and the phone side
/// installs every face with the same version: a patched face under the original's id could
/// neither replace an installed copy nor sit next to another face with that id.
pub fn derived_id(bin: &[u8], place: &Placement) -> String {
    let mut seed = package_id(bin).into_bytes();
    seed.extend_from_slice(c_string(bin, NAME_OFFSET, NAME_FIELD).as_bytes());
    seed.extend_from_slice(&(place.z as u32).to_le_bytes());
    seed.extend_from_slice(&place.x.to_le_bytes());
    seed.extend_from_slice(&place.y.to_le_bytes());
    seed.extend_from_slice(&place.width.to_le_bytes());
    seed.extend_from_slice(&place.height.to_le_bytes());
    format!("B2D{:09}", zip::crc32(&seed) % 1_000_000_000)
}

/// Writes the package id and appends `suffix` to the display name (within the header field).
pub fn set_identity(bin: &mut [u8], id: &str, suffix: &str) {
    if bin.len() < NAME_OFFSET + NAME_FIELD {
        return;
    }
    let f = &mut bin[ID_OFFSET..ID_OFFSET + ID_FIELD];
    f.fill(0);
    let n = id.len().min(ID_FIELD - 1);
    f[..n].copy_from_slice(&id.as_bytes()[..n]);
    if u32_at(bin, NAME_OFFSET).ok() == Some(u32::MAX) {
        return;
    }
    let name = renamed(&c_string(bin, NAME_OFFSET, NAME_FIELD), suffix, NAME_FIELD - 1);
    let f = &mut bin[NAME_OFFSET..NAME_OFFSET + NAME_FIELD];
    f.fill(0);
    f[..name.len()].copy_from_slice(name.as_bytes());
}

/// `name` + `suffix` in at most `max` bytes (the name is shortened at a character boundary).
fn renamed(name: &str, suffix: &str, max: usize) -> String {
    if name.ends_with(suffix) {
        return name.to_string();
    }
    let mut keep = max.saturating_sub(suffix.len()).min(name.len());
    while !name.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}{suffix}", &name[..keep])
}

/// Replaces the text of the first `<tag>...</tag>`.
fn set_xml_tag(text: &str, tag: &str, f: impl Fn(&str) -> String) -> String {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let Some(a) = text.find(&open) else { return text.to_string() };
    let start = a + open.len();
    let Some(len) = text[start..].find(&close) else { return text.to_string() };
    format!("{}{}{}", &text[..start], f(&text[start..start + len]), &text[start + len..])
}

/// Replaces attribute `attr`'s value in the first `<element ...>` tag.
fn set_xml_attr(text: &str, element: &str, attr: &str, f: impl Fn(&str) -> String) -> String {
    let Some(a) = text.find(&format!("<{element} ")) else { return text.to_string() };
    let tag_end = text[a..].find('>').map(|e| a + e).unwrap_or(text.len());
    let key = format!(" {attr}=\"");
    let Some(k) = text[a..tag_end].find(&key) else { return text.to_string() };
    let start = a + k + key.len();
    let Some(len) = text[start..tag_end].find('"') else { return text.to_string() };
    format!("{}{}{}", &text[..start], f(&text[start..start + len]), &text[start + len..])
}

/// Patches the `resource.bin` of an MWZ and recomputes `hashCode`. Returns (mwz, bin).
pub fn patch_mwz(mwz: &[u8], place: Placement) -> Result<(Vec<u8>, Vec<u8>), FaceError> {
    let mut entries = zip::read(mwz).map_err(FaceError::Zip)?;
    let bin_index = entries.iter().position(|e| e.0 == "resource.bin").ok_or(FaceError::NoResourceBin)?;
    let bin = patch_bin(&entries[bin_index].1, place)?;
    entries[bin_index].1 = bin.clone();
    // The new identity in the package's descriptions too (the phone reads the id from them).
    let id = package_id(&bin);
    for (name, data) in entries.iter_mut() {
        let Ok(text) = core::str::from_utf8(data) else { continue };
        let edited = if name == "description.xml" {
            let t = set_xml_tag(text, "pkgName", |_| id.clone());
            set_xml_tag(&t, "name", |n| renamed(n, NAME_SUFFIX, usize::MAX))
        } else if name == "resources/manifest.xml" {
            let t = set_xml_attr(text, "Watchface", "id", |_| id.clone());
            set_xml_attr(&t, "Watchface", "name", |n| renamed(n, NAME_SUFFIX, usize::MAX))
        } else {
            continue;
        };
        *data = edited.into_bytes();
    }
    let get = |name: &str| entries.iter().find(|e| e.0 == name).map(|e| e.1.clone());
    if let (Some(cap), Some(manifest)) = (get("capability.json"), get("resources/manifest.xml")) {
        let hash = [sha256::hex(&cap), sha256::hex(&manifest), sha256::hex(&bin)].join(",");
        match entries.iter_mut().find(|e| e.0 == "hashCode") {
            Some(e) => e.1 = hash.into_bytes(),
            None => entries.push(("hashCode".into(), hash.into_bytes())),
        }
    }
    Ok((zip::write(&entries), bin))
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    fn header(id: &str, name: &str) -> Vec<u8> {
        let mut bin = vec![0u8; 256];
        bin[ID_OFFSET..ID_OFFSET + id.len()].copy_from_slice(id.as_bytes());
        bin[NAME_OFFSET..NAME_OFFSET + name.len()].copy_from_slice(name.as_bytes());
        bin
    }

    #[test]
    fn a_patched_face_gets_its_own_id_and_a_marked_name() {
        let place = Placement { z: 1, x: 0, y: 0, width: 336, height: 480 };
        let mut bin = header("000000000000", "超级岛相册表盘");
        let id = derived_id(&bin, &place);
        assert_eq!(id.len(), 12);
        assert!(id.starts_with("B2D") && id.bytes().all(|b| b.is_ascii_alphanumeric()), "{id}");
        // Same face and placement: the same id; another layer position: another one.
        assert_eq!(id, derived_id(&bin, &place));
        assert_ne!(id, derived_id(&bin, &Placement { z: 2, ..place }));
        set_identity(&mut bin, &id, NAME_SUFFIX);
        assert_eq!(package_id(&bin), id);
        assert_eq!(c_string(&bin, NAME_OFFSET, NAME_FIELD), "超级岛相册表盘 BA");
        // Applying it twice does not stack suffixes.
        set_identity(&mut bin, &id, NAME_SUFFIX);
        assert_eq!(c_string(&bin, NAME_OFFSET, NAME_FIELD), "超级岛相册表盘 BA");
    }

    #[test]
    fn long_names_are_cut_at_a_character_boundary() {
        let long = "表".repeat(21); // 63 bytes
        let r = renamed(&long, NAME_SUFFIX, NAME_FIELD - 1);
        assert!(r.len() <= NAME_FIELD - 1 && r.ends_with(NAME_SUFFIX), "{r}");
        assert!(r.is_char_boundary(r.len() - NAME_SUFFIX.len()));
    }

    #[test]
    fn package_descriptions_follow_the_new_identity() {
        let xml = r#"<Watchface name="月薪喵" width="336" id="09802" SKU="false">"#;
        let t = set_xml_attr(xml, "Watchface", "id", |_| "B2D123456789".into());
        let t = set_xml_attr(&t, "Watchface", "name", |n| renamed(n, NAME_SUFFIX, usize::MAX));
        assert_eq!(t, r#"<Watchface name="月薪喵 BA" width="336" id="B2D123456789" SKU="false">"#);
        let d = "<watch>\n<name>月薪喵</name>\n<pkgName>09802</pkgName>\n</watch>";
        let d = set_xml_tag(d, "pkgName", |_| "B2D123456789".into());
        assert!(d.contains("<pkgName>B2D123456789</pkgName>") && d.contains("<name>月薪喵</name>"));
    }
}

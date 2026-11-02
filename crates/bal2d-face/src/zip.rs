//! Minimal ZIP reading (stored / deflate) and writing (deflate), for MWZ packages.

pub type Entries = Vec<(String, Vec<u8>)>;

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
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

/// Reads every file entry in central-directory order (directories included as empty data
/// with a trailing '/').
pub fn read(d: &[u8]) -> Result<Entries, &'static str> {
    let eocd = (0..d.len().saturating_sub(21)).rev().find(|&i| u32_at(d, i) == Some(0x0605_4b50)).ok_or("no end of central directory")?;
    let count = u16_at(d, eocd + 10).ok_or("eocd")? as usize;
    let mut cd = u32_at(d, eocd + 16).ok_or("eocd")? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(d, cd) != Some(0x0201_4b50) {
            return Err("bad central directory entry");
        }
        let method = u16_at(d, cd + 10).ok_or("cd")?;
        let csize = u32_at(d, cd + 20).ok_or("cd")? as usize;
        let usize_ = u32_at(d, cd + 24).ok_or("cd")? as usize;
        let nlen = u16_at(d, cd + 28).ok_or("cd")? as usize;
        let elen = u16_at(d, cd + 30).ok_or("cd")? as usize;
        let clen = u16_at(d, cd + 32).ok_or("cd")? as usize;
        let local = u32_at(d, cd + 42).ok_or("cd")? as usize;
        let name = String::from_utf8_lossy(d.get(cd + 46..cd + 46 + nlen).ok_or("name")?).into_owned();
        cd += 46 + nlen + elen + clen;
        if u32_at(d, local) != Some(0x0403_4b50) {
            return Err("bad local header");
        }
        let lnlen = u16_at(d, local + 26).ok_or("lh")? as usize;
        let lelen = u16_at(d, local + 28).ok_or("lh")? as usize;
        let start = local + 30 + lnlen + lelen;
        let raw = d.get(start..start + csize).ok_or("entry data")?;
        let data = match method {
            0 => raw.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(raw, usize_.max(1)).map_err(|_| "inflate")?,
            _ => return Err("unsupported compression"),
        };
        out.push((name, data));
    }
    Ok(out)
}

pub fn write(entries: &Entries) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let dir = name.ends_with('/');
        let compressed = if dir || data.is_empty() { Vec::new() } else { miniz_oxide::deflate::compress_to_vec(data, 9) };
        let (method, body): (u16, &[u8]) = if !dir && !data.is_empty() && compressed.len() < data.len() { (8, &compressed) } else { (0, data) };
        let crc = crc32(data);
        let offset = out.len() as u32;
        let n = name.as_bytes();
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // time/date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(n.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(n);
        out.extend_from_slice(body);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(body.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(n.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&(if dir { 0x10u32 } else { 0 }).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(n);
    }
    let cd_offset = out.len() as u32;
    let cd_len = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

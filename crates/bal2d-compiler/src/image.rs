//! RGBA images: PNG decoding, premultiplication, area resampling, GPU page encoding.

use bal2d_pack::PageFormat;

/// 8-bit RGBA image (straight or premultiplied, by context).
#[derive(Clone, Debug)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    pub fn new(width: u32, height: u32) -> Rgba {
        Rgba { width, height, data: vec![0; (width * height * 4) as usize] }
    }

    pub fn premultiplied(mut self) -> Rgba {
        for p in self.data.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
        self
    }

    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Rgba {
        let mut out = Rgba::new(w, h);
        for row in 0..h {
            let sy = (y + row).min(self.height - 1);
            for col in 0..w {
                let sx = (x + col).min(self.width - 1);
                let s = ((sy * self.width + sx) * 4) as usize;
                let d = ((row * w + col) * 4) as usize;
                out.data[d..d + 4].copy_from_slice(&self.data[s..s + 4]);
            }
        }
        out
    }

    /// Area-average resampling to (w, h) (any ratio; meant for downscaling).
    pub fn resize(&self, w: u32, h: u32) -> Rgba {
        if w == self.width && h == self.height {
            return self.clone();
        }
        // Horizontal pass into f32, then vertical.
        let horiz = resample_axis(&to_f32(self), self.width as usize, self.height as usize, w as usize, true);
        let both = resample_axis(&horiz, w as usize, self.height as usize, h as usize, false);
        let mut out = Rgba::new(w, h);
        for (d, s) in out.data.iter_mut().zip(both.iter()) {
            *d = (s.clamp(0.0, 255.0) + 0.5) as u8;
        }
        out
    }

    /// Copies `src` to (x, y) and extrudes its border by `pad` pixels (bilinear bleed guard).
    pub fn blit_padded(&mut self, src: &Rgba, x: u32, y: u32, pad: u32) {
        let (w, h) = (src.width as i64, src.height as i64);
        for dy in -(pad as i64)..h + pad as i64 {
            for dx in -(pad as i64)..w + pad as i64 {
                let tx = x as i64 + dx;
                let ty = y as i64 + dy;
                if tx < 0 || ty < 0 || tx >= self.width as i64 || ty >= self.height as i64 {
                    continue;
                }
                let sx = dx.clamp(0, w - 1) as u32;
                let sy = dy.clamp(0, h - 1) as u32;
                let s = ((sy * src.width + sx) * 4) as usize;
                let d = ((ty as u32 * self.width + tx as u32) * 4) as usize;
                self.data[d..d + 4].copy_from_slice(&src.data[s..s + 4]);
            }
        }
    }
}

fn to_f32(img: &Rgba) -> Vec<f32> {
    img.data.iter().map(|v| *v as f32).collect()
}

/// Box-filter resample along one axis with exact fractional coverage.
fn resample_axis(src: &[f32], w: usize, h: usize, n: usize, horizontal: bool) -> Vec<f32> {
    let (len, other) = if horizontal { (w, h) } else { (h, w) };
    let (out_w, out_h) = if horizontal { (n, h) } else { (w, n) };
    let mut out = vec![0f32; out_w * out_h * 4];
    let ratio = len as f64 / n as f64;
    for o in 0..other {
        for i in 0..n {
            let start = i as f64 * ratio;
            let end = (i + 1) as f64 * ratio;
            let mut acc = [0f64; 4];
            let mut wsum = 0f64;
            let mut k = start.floor() as usize;
            while (k as f64) < end && k < len {
                let lo = (k as f64).max(start);
                let hi = ((k + 1) as f64).min(end);
                let wgt = hi - lo;
                if wgt > 0.0 {
                    let idx = if horizontal { (o * w + k) * 4 } else { (k * w + o) * 4 };
                    for c in 0..4 {
                        acc[c] += src[idx + c] as f64 * wgt;
                    }
                    wsum += wgt;
                }
                k += 1;
            }
            let didx = if horizontal { (o * out_w + i) * 4 } else { (i * out_w + o) * 4 };
            for c in 0..4 {
                out[didx + c] = if wsum > 0.0 { (acc[c] / wsum) as f32 } else { 0.0 };
            }
        }
    }
    out
}

pub fn decode_png(bytes: &[u8]) -> Result<Rgba, String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = dec.read_info().map_err(|e| format!("png: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("png: {e}"))?;
    let px = &buf[..info.buffer_size()];
    let data: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => px.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        other => return Err(format!("unsupported png color type {other:?}")),
    };
    Ok(Rgba { width: info.width, height: info.height, data })
}

pub fn encode_png(img: &Rgba) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&img.data).unwrap();
    }
    out
}

const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Encodes a premultiplied page in the device format.
pub fn encode_page(img: &Rgba, format: PageFormat) -> Vec<u8> {
    match format {
        PageFormat::Bgra8888 => img.data.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect(),
        PageFormat::Argb4444 => {
            let mut out = Vec::with_capacity((img.width * img.height * 2) as usize);
            for y in 0..img.height {
                for x in 0..img.width {
                    let i = ((y * img.width + x) * 4) as usize;
                    let p = &img.data[i..i + 4];
                    let t = BAYER4[(y & 3) as usize][(x & 3) as usize] as u32;
                    // Ordered dither to 4 bits; keep premultiplied color <= alpha after rounding.
                    // floor(v * 15 / 255 + (t + 0.5) / 16), in integers.
                    let q = |v: u8| ((v as u32 * 15 * 32 + (2 * t + 1) * 255) / (255 * 32)).min(15);
                    let a = q(p[3]);
                    let r = q(p[0]).min(a);
                    let g = q(p[1]).min(a);
                    let b = q(p[2]).min(a);
                    let v = ((a << 12) | (r << 8) | (g << 4) | b) as u16;
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
            out
        }
        PageFormat::Index8 => crate::image::index8(img),
        PageFormat::Argb8565 => {
            // Rounded to 5 / 6 bits, colour kept <= alpha (premultiplied). No dithering: the
            // screen is 16-bit too, the GPU filters the texture anyway, and dithering costs
            // 8-16% of the pack size for no measurable gain.
            let mut out = Vec::with_capacity((img.width * img.height * 3) as usize);
            for p in img.data.chunks_exact(4) {
                let a = p[3] as u32;
                let q = |v: u8, max: u32| ((v as u32 * max + 127) / 255).min((a * max + 127) / 255);
                let v = ((q(p[0], 31) << 11) | (q(p[1], 63) << 5) | q(p[2], 31)) as u16;
                out.extend_from_slice(&v.to_le_bytes());
                out.push(p[3]);
            }
            out
        }
    }
}

/// 256-color palette (median cut on premultiplied RGBA) + indices; palette first, BGRA8888.
pub fn index8(img: &Rgba) -> Vec<u8> {
    let pixels: Vec<[u8; 4]> = img.data.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
    let palette = median_cut(&pixels, 256);
    let mut out = Vec::with_capacity(1024 + pixels.len());
    for c in &palette {
        out.extend_from_slice(&[c[2], c[1], c[0], c[3]]);
    }
    for _ in palette.len()..256 {
        out.extend_from_slice(&[0, 0, 0, 0]);
    }
    let mut cache = std::collections::HashMap::new();
    for p in &pixels {
        let idx = *cache.entry(*p).or_insert_with(|| nearest(&palette, p));
        out.push(idx);
    }
    out
}

fn nearest(pal: &[[u8; 4]], p: &[u8; 4]) -> u8 {
    let mut best = (u32::MAX, 0u8);
    for (i, c) in pal.iter().enumerate() {
        let d: u32 = (0..4).map(|k| (c[k] as i32 - p[k] as i32).pow(2) as u32 * if k == 3 { 2 } else { 1 }).sum();
        if d < best.0 {
            best = (d, i as u8);
        }
    }
    best.1
}

fn median_cut(px: &[[u8; 4]], n: usize) -> Vec<[u8; 4]> {
    let mut boxes: Vec<Vec<[u8; 4]>> = vec![px.iter().filter(|p| p[3] > 0).copied().collect()];
    let mut out = vec![[0, 0, 0, 0]];
    while boxes.len() < n - 1 {
        // Split the box with the largest channel range.
        let (bi, ch, _) = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| {
                let mut best = (0usize, 0i32);
                for c in 0..4 {
                    let lo = b.iter().map(|p| p[c]).min().unwrap() as i32;
                    let hi = b.iter().map(|p| p[c]).max().unwrap() as i32;
                    if hi - lo > best.1 {
                        best = (c, hi - lo);
                    }
                }
                (i, best.0, best.1 as i64 * b.len() as i64)
            })
            .max_by_key(|x| x.2)
            .unwrap_or((usize::MAX, 0, 0));
        if bi == usize::MAX {
            break;
        }
        let mut b = boxes.swap_remove(bi);
        b.sort_unstable_by_key(|p| p[ch]);
        let tail = b.split_off(b.len() / 2);
        boxes.push(b);
        boxes.push(tail);
    }
    for b in boxes {
        if b.is_empty() {
            continue;
        }
        let mut s = [0u64; 4];
        for p in &b {
            for k in 0..4 {
                s[k] += p[k] as u64;
            }
        }
        let n = b.len() as u64;
        out.push([(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8, (s[3] / n) as u8]);
    }
    out
}

/// Decodes a device page back to premultiplied RGBA (for previews and tests).
pub fn decode_page(width: u32, height: u32, format: PageFormat, data: &[u8]) -> Rgba {
    let mut img = Rgba::new(width, height);
    match format {
        PageFormat::Bgra8888 => {
            for (d, s) in img.data.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
            }
        }
        PageFormat::Argb4444 => {
            for (d, s) in img.data.chunks_exact_mut(4).zip(data.chunks_exact(2)) {
                let v = u16::from_le_bytes([s[0], s[1]]);
                let e = |n: u16| ((n & 15) * 17) as u8;
                d.copy_from_slice(&[e(v >> 8), e(v >> 4), e(v), e(v >> 12)]);
            }
        }
        PageFormat::Argb8565 => {
            for (d, s) in img.data.chunks_exact_mut(4).zip(data.chunks_exact(3)) {
                let v = u16::from_le_bytes([s[0], s[1]]);
                let e5 = |n: u16| ((n & 31) as u32 * 255 / 31) as u8;
                let e6 = |n: u16| ((n & 63) as u32 * 255 / 63) as u8;
                d.copy_from_slice(&[e5(v >> 11), e6(v >> 5), e5(v), s[2]]);
            }
        }
        PageFormat::Index8 => {
            let pal = &data[..1024];
            for (d, &i) in img.data.chunks_exact_mut(4).zip(&data[1024..]) {
                let c = &pal[i as usize * 4..i as usize * 4 + 4];
                d.copy_from_slice(&[c[2], c[1], c[0], c[3]]);
            }
        }
    }
    img
}

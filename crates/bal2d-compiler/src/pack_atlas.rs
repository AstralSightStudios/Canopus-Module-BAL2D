//! Region cropping, resampling and repacking into new atlas pages (shelf packing, padded,
//! extruded).
//!
//! Each region is taken in its logical (unrotated) orientation, cropped to the part that is
//! ever visible, resampled, and stored unrotated. The crop is recorded the way Spine records
//! whitespace stripping (offsets inside the original size), so mesh UVs and region attachment
//! geometry are unchanged.

use bal2d_spine::Atlas;

use crate::image::Rgba;

pub struct Item {
    pub region: usize,
    pub scale: f32,
    /// Stored rotated in the source page (any of 90, 180, 270 degrees).
    pub rotated: bool,
    /// Visible part in logical pixels (x0, y0, x1, y1; y down).
    pub crop: [u32; 4],
}

pub struct Packed {
    pub atlas_text: String,
    pub pages: Vec<Rgba>,
}

const PAD: u32 = 2;

struct Placed {
    item: usize,
    page: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// The region in logical orientation (width x height). Source atlases rotate regions by 90,
/// 180 or 270 degrees; the page position of logical pixel (lx, ly) follows Spine's
/// `MeshAttachment.updateRegion`: 90 -> (x + ly, y + w-1-lx), 180 -> (x + w-1-lx, y + h-1-ly),
/// 270 -> (x + h-1-ly, y + lx).
fn logical(atlas: &Atlas, pages: &[Rgba], region: usize) -> Rgba {
    let r = &atlas.regions[region];
    let (w, h) = (r.width as u32, r.height as u32);
    let page = &pages[r.page as usize];
    if r.degrees == 0 {
        return page.crop(r.x as u32, r.y as u32, w, h);
    }
    let (x, y) = (r.x as u32, r.y as u32);
    let mut out = Rgba::new(w, h);
    for ly in 0..h {
        for lx in 0..w {
            let (px, py) = match r.degrees {
                90 => (x + ly, y + w - 1 - lx),
                180 => (x + w - 1 - lx, y + h - 1 - ly),
                _ => (x + h - 1 - ly, y + lx),
            };
            let (px, py) = (px.min(page.width - 1), py.min(page.height - 1));
            let s = ((py * page.width + px) * 4) as usize;
            let d = ((ly * w + lx) * 4) as usize;
            out.data[d..d + 4].copy_from_slice(&page.data[s..s + 4]);
        }
    }
    out
}

/// The resampled size of an item: its crop times its scale, at least one texel.
pub fn item_size(it: &Item) -> (u32, u32) {
    let [x0, y0, x1, y1] = it.crop;
    let (cw, ch) = (x1 - x0, y1 - y0);
    (((cw as f32 * it.scale).round() as u32).max(1), ((ch as f32 * it.scale).round() as u32).max(1))
}

/// Crops and resamples the items from decoded pages, then packs them ([`build_images`]).
pub fn build(atlas: &Atlas, pages: &[Rgba], items: &[Item], max_page: u32) -> Result<Packed, String> {
    let mut images = Vec::with_capacity(items.len());
    for it in items {
        let [x0, y0, x1, y1] = it.crop;
        let (w, h) = item_size(it);
        if w + 2 * PAD > max_page || h + 2 * PAD > max_page {
            return Err(format!("region {} ({}x{}) does not fit a {max_page} page", atlas.regions[it.region].name, w, h));
        }
        let src = logical(atlas, pages, it.region).crop(x0, y0, x1 - x0, y1 - y0);
        images.push(src.resize(w, h));
    }
    build_images(atlas, items, images, max_page)
}

/// Packs already resampled item images (`images[i]` is `items[i]` at [`item_size`]).
pub fn build_images(atlas: &Atlas, items: &[Item], images: Vec<Rgba>, max_page: u32) -> Result<Packed, String> {
    for (it, img) in items.iter().zip(&images) {
        if img.width + 2 * PAD > max_page || img.height + 2 * PAD > max_page {
            return Err(format!("region {} ({}x{}) does not fit a {max_page} page", atlas.regions[it.region].name, img.width, img.height));
        }
    }
    // Skyline (bottom-left) packing, tallest first; try several page widths and keep the
    // smallest total area. Regions that do not fit under max_page open another page.
    let sizes: Vec<(u32, u32)> = images.iter().map(|i| (i.width + 2 * PAD, i.height + 2 * PAD)).collect();
    let area: u64 = sizes.iter().map(|&(w, h)| w as u64 * h as u64).sum();
    let widest = sizes.iter().map(|s| s.0).max().unwrap_or(16);
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse((sizes[i].1, sizes[i].0)));
    let mut best: Option<(u64, Vec<Placed>, Vec<(u32, u32)>)> = None;
    let base = (area as f64).sqrt();
    for k in 0..12 {
        let width = (((base * (0.85 + 0.05 * k as f64)) as u32).div_ceil(16) * 16).max(widest.div_ceil(16) * 16).min(max_page);
        let (placed, dims) = skyline(&order, &sizes, width, max_page);
        let total: u64 = dims.iter().map(|&(w, h)| w as u64 * h as u64).sum();
        if best.as_ref().is_none_or(|b| total < b.0) {
            best = Some((total, placed, dims));
        }
    }
    let (_, placed, dims) = best.unwrap();
    let page_used: Vec<u32> = dims.iter().map(|d| d.1).collect();
    // Page sizes rounded up to 16 pixels (the GPU's stride alignment).
    let mut out_pages = Vec::new();
    for (pi, used_h) in page_used.iter().enumerate() {
        let used_w = placed.iter().filter(|p| p.page == pi).map(|p| p.x + p.w + PAD).max().unwrap_or(16);
        out_pages.push(Rgba::new(used_w.div_ceil(16) * 16, used_h.div_ceil(16).max(1) * 16));
    }
    let _ = &dims;
    for p in &placed {
        out_pages[p.page].blit_padded(&images[p.item], p.x, p.y, PAD);
    }
    let mut text = String::new();
    for (pi, page) in out_pages.iter().enumerate() {
        text.push_str(&format!("page{pi}.png\nsize: {},{}\nfilter: Linear,Linear\npma: true\n", page.width, page.height));
        for p in placed.iter().filter(|p| p.page == pi) {
            let it = &items[p.item];
            let r = &atlas.regions[it.region];
            let [x0, _y0, x1, y1] = it.crop;
            let (cw, ch) = ((x1 - x0) as f32, (y1 - it.crop[1]) as f32);
            let sx = p.w as f32 / cw;
            let sy = p.h as f32 / ch;
            // Offsets are from the bottom-left of the original image.
            let ox = ((r.offset_x as f32 + x0 as f32) * sx).round() as i32;
            let oy = ((r.offset_y as f32 + (r.height as u32 - y1) as f32) * sy).round() as i32;
            let ow = ((r.original_width as f32 * sx).round() as i32).max(ox + p.w as i32);
            let oh = ((r.original_height as f32 * sy).round() as i32).max(oy + p.h as i32);
            text.push_str(&format!("{}\nbounds: {},{},{},{}\noffsets: {ox},{oy},{ow},{oh}\n", r.name, p.x, p.y, p.w, p.h));
        }
        text.push('\n');
    }
    Ok(Packed { atlas_text: text, pages: out_pages })
}

/// Skyline bottom-left packing of `order` (padded sizes) into pages `width` wide and at most
/// `max_h` tall. Returns placements (inner rects) and per-page (width, used height).
fn skyline(order: &[usize], sizes: &[(u32, u32)], width: u32, max_h: u32) -> (Vec<Placed>, Vec<(u32, u32)>) {
    let mut placed = Vec::with_capacity(order.len());
    let mut dims: Vec<(u32, u32)> = Vec::new();
    // Skyline segments of the current page: (x, y, w).
    let mut sky: Vec<(u32, u32, u32)> = vec![(0, 0, width)];
    let mut page = 0usize;
    dims.push((width, 0));
    let mut pending: Vec<usize> = order.to_vec();
    while !pending.is_empty() {
        let mut next = Vec::new();
        for &i in &pending {
            let (w, h) = sizes[i];
            // Best position: lowest top, then leftmost.
            let mut best: Option<(u32, u32, usize)> = None;
            for s in 0..sky.len() {
                let x = sky[s].0;
                if x + w > width {
                    break;
                }
                let mut y = 0;
                let mut covered = 0;
                let mut j = s;
                while covered < w && j < sky.len() {
                    y = y.max(sky[j].1);
                    covered += sky[j].2.min(w - covered);
                    j += 1;
                }
                if covered < w || y + h > max_h {
                    continue;
                }
                if best.is_none_or(|b| y + h < b.1 + sizes[i].1 || (y + h == b.1 + sizes[i].1 && x < b.0)) {
                    best = Some((x, y, s));
                }
            }
            match best {
                None => next.push(i),
                Some((x, y, _)) => {
                    placed.push(Placed { item: i, page, x: x + PAD, y: y + PAD, w: w - 2 * PAD, h: h - 2 * PAD });
                    dims[page].1 = dims[page].1.max(y + h);
                    // Raise the skyline over [x, x + w) to y + h.
                    let mut nsky: Vec<(u32, u32, u32)> = Vec::new();
                    for &(sx, sy, sw) in &sky {
                        let (s0, s1) = (sx, sx + sw);
                        if s1 <= x || s0 >= x + w {
                            nsky.push((sx, sy, sw));
                            continue;
                        }
                        if s0 < x {
                            nsky.push((s0, sy, x - s0));
                        }
                        if s1 > x + w {
                            nsky.push((x + w, sy, s1 - (x + w)));
                        }
                    }
                    nsky.push((x, y + h, w));
                    nsky.sort_by_key(|s| s.0);
                    // Merge equal neighbours.
                    let mut merged: Vec<(u32, u32, u32)> = Vec::new();
                    for seg in nsky {
                        match merged.last_mut() {
                            Some(last) if last.1 == seg.1 && last.0 + last.2 == seg.0 => last.2 += seg.2,
                            _ => merged.push(seg),
                        }
                    }
                    sky = merged;
                }
            }
        }
        if next.len() == pending.len() {
            // Nothing fits even an empty page: give up on the rest (caller checks sizes).
            break;
        }
        pending = next;
        if !pending.is_empty() {
            page += 1;
            dims.push((width, 0));
            sky = vec![(0, 0, width)];
        }
    }
    (placed, dims)
}

//! Atlas regions straight out of the PNG, row by row: the page is never held decoded.
//!
//! The AstroBox plugin runs in at most 128 MiB, and a 4096 x 4096 page alone is 64 MiB
//! decoded. Each wanted region is resampled while the page's rows stream past: a row is
//! resampled horizontally into the region's row buffer and added, with its box-filter weights,
//! to the one or two output rows it overlaps; finished output rows are stored. Memory is the
//! outputs plus one row per region, whatever the page size.
//!
//! The arithmetic is `Rgba::resize`'s (horizontal pass into f32, vertical pass accumulated in
//! f64 in source order, rounded to u8), in page orientation: a region stored unrotated comes
//! out exactly as cropping and resizing the decoded page did; a rotated one is resampled along
//! the page's axes and then turned (the same filter, the passes in the other order).

use bal2d_spine::Atlas;

use crate::image::Rgba;

/// One region to extract: its logical (unrotated) crop and output size.
#[derive(Clone, Copy, Debug)]
pub struct Want {
    pub region: usize,
    /// Visible part in logical pixels (x0, y0, x1, y1; y down).
    pub crop: [u32; 4],
    /// Output size in logical orientation.
    pub size: (u32, u32),
}

/// The streaming resampler of one region, in page orientation.
struct Job {
    /// Source rectangle on the page (may run past its edge: edge pixels repeat).
    x0: u32,
    y0: u32,
    sh: u32,
    /// Output size in page orientation.
    ow: usize,
    oh: usize,
    /// Horizontal taps per output column: (first source column, weights).
    taps: Vec<(u32, Vec<f64>)>,
    ratio_v: f64,
    /// The output row being accumulated, its weight, and the next source row expected.
    row: usize,
    acc: Vec<f64>,
    wsum: f64,
    next: u32,
    hrow: Vec<f32>,
    out: Vec<u8>,
}

impl Job {
    fn new(x0: u32, y0: u32, sw: u32, sh: u32, ow: usize, oh: usize) -> Job {
        let ratio = sw as f64 / ow as f64;
        let taps = (0..ow)
            .map(|i| {
                let (start, end) = (i as f64 * ratio, (i + 1) as f64 * ratio);
                let first = start.floor() as u32;
                let mut w = Vec::new();
                let mut k = first;
                while (k as f64) < end && k < sw {
                    let lo = (k as f64).max(start);
                    let hi = ((k + 1) as f64).min(end);
                    w.push(hi - lo);
                    k += 1;
                }
                (first, w)
            })
            .collect();
        Job {
            x0,
            y0,
            sh,
            ow,
            oh,
            taps,
            ratio_v: sh as f64 / oh as f64,
            row: 0,
            acc: vec![0.0; ow * 4],
            wsum: 0.0,
            next: 0,
            hrow: vec![0.0; ow * 4],
            out: vec![0; ow * oh * 4],
        }
    }

    fn done(&self) -> bool {
        self.row >= self.oh
    }

    /// Source row `k` (0-based within the rectangle) of the page, premultiplied RGBA.
    fn feed(&mut self, k: u32, page_row: &[u8], page_w: u32) {
        // Horizontal pass: the box filter over this row, as `resample_axis` does it.
        for (i, (first, w)) in self.taps.iter().enumerate() {
            let mut acc = [0f64; 4];
            let mut wsum = 0f64;
            for (j, wgt) in w.iter().enumerate() {
                if *wgt > 0.0 {
                    let px = (self.x0 + first + j as u32).min(page_w - 1) as usize * 4;
                    for c in 0..4 {
                        acc[c] += page_row[px + c] as f32 as f64 * wgt;
                    }
                    wsum += wgt;
                }
            }
            for c in 0..4 {
                self.hrow[i * 4 + c] = if wsum > 0.0 { (acc[c] / wsum) as f32 } else { 0.0 };
            }
        }
        // Vertical pass: this row's share of each output row it overlaps.
        while self.row < self.oh {
            let start = self.row as f64 * self.ratio_v;
            let end = (self.row + 1) as f64 * self.ratio_v;
            let lo = (k as f64).max(start);
            let hi = ((k + 1) as f64).min(end);
            let wgt = hi - lo;
            if wgt > 0.0 {
                for (a, h) in self.acc.iter_mut().zip(&self.hrow) {
                    *a += *h as f64 * wgt;
                }
                self.wsum += wgt;
            }
            // The output row is complete once the source has reached its end (or run out).
            if end <= (k + 1) as f64 || k + 1 >= self.sh {
                let base = self.row * self.ow * 4;
                for (d, a) in self.out[base..base + self.ow * 4].iter_mut().zip(&self.acc) {
                    let s = if self.wsum > 0.0 { (a / self.wsum) as f32 } else { 0.0 };
                    *d = (s.clamp(0.0, 255.0) + 0.5) as u8;
                }
                self.acc.iter_mut().for_each(|a| *a = 0.0);
                self.wsum = 0.0;
                self.row += 1;
                if k + 1 >= self.sh {
                    continue;
                }
            } else {
                break;
            }
        }
        self.next = k + 1;
    }
}

/// Decodes `png` (one atlas page) row by row and returns the wanted regions of that page, in
/// the order of `wants`, premultiplied when `premultiply` (straight-alpha sources).
pub fn extract(atlas: &Atlas, png: &[u8], premultiply: bool, wants: &[Want]) -> Result<Vec<Rgba>, String> {
    let mut jobs: Vec<Job> = Vec::with_capacity(wants.len());
    for w in wants {
        let r = &atlas.regions[w.region];
        let (rx, ry, rw, rh) = (r.x as u32, r.y as u32, r.width as u32, r.height as u32);
        let [x0, y0, x1, y1] = w.crop;
        let (ow, oh) = (w.size.0 as usize, w.size.1 as usize);
        // The crop's rectangle on the page and the output in page orientation (see
        // `pack_atlas::logical` for the rotations).
        let job = match r.degrees {
            90 => Job::new(rx + y0, ry + rw - x1, y1 - y0, x1 - x0, oh, ow),
            180 => Job::new(rx + rw - x1, ry + rh - y1, x1 - x0, y1 - y0, ow, oh),
            270 => Job::new(rx + rh - y1, ry + x0, y1 - y0, x1 - x0, oh, ow),
            _ => Job::new(rx + x0, ry + y0, x1 - x0, y1 - y0, ow, oh),
        };
        jobs.push(job);
    }
    let mut dec = png::Decoder::new(std::io::Cursor::new(png));
    dec.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = dec.read_info().map_err(|e| format!("png: {e}"))?;
    let (page_w, page_h, interlaced) = {
        let info = reader.info();
        (info.width, info.height, info.interlaced)
    };
    let (color, _) = reader.output_color_type();
    let to_rgba = |src: &[u8], dst: &mut Vec<u8>| -> Result<(), String> {
        dst.clear();
        match color {
            png::ColorType::Rgba => dst.extend_from_slice(src),
            png::ColorType::GrayscaleAlpha => dst.extend(src.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]])),
            png::ColorType::Rgb => dst.extend(src.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255])),
            png::ColorType::Grayscale => dst.extend(src.iter().flat_map(|&g| [g, g, g, 255])),
            other => return Err(format!("unsupported png color type {other:?}")),
        }
        if premultiply {
            for p in dst.chunks_exact_mut(4) {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = ((*c as u32 * a + 127) / 255) as u8;
                }
            }
        }
        Ok(())
    };
    let mut row = Vec::with_capacity(page_w as usize * 4);
    let feed_row = |y: u32, row: &[u8], jobs: &mut [Job]| {
        for j in jobs.iter_mut() {
            if !j.done() && y >= j.y0 && y < j.y0 + j.sh {
                j.feed(y - j.y0, row, page_w);
            }
        }
    };
    if interlaced {
        // Adam7 rows arrive by pass: decode the frame (rare in game atlases; memory as before).
        let img = crate::image::decode_png(png)?;
        let mut y = 0;
        for src in img.data.chunks_exact(page_w as usize * 4) {
            to_rgba(src, &mut row)?;
            feed_row(y, &row, &mut jobs);
            y += 1;
        }
    } else {
        let mut y = 0;
        while let Some(r) = reader.next_row().map_err(|e| format!("png: {e}"))? {
            to_rgba(r.data(), &mut row)?;
            feed_row(y, &row, &mut jobs);
            y += 1;
        }
    }
    // Rectangles that run past the bottom repeat the last row (as clamped cropping did).
    if page_h > 0 {
        for j in jobs.iter_mut() {
            while !j.done() {
                let k = j.next;
                j.feed(k, &row, page_w);
            }
        }
    }
    // Back to logical orientation.
    Ok(wants
        .iter()
        .zip(jobs)
        .map(|(w, j)| {
            let r = &atlas.regions[w.region];
            let (lw, lh) = (w.size.0 as usize, w.size.1 as usize);
            if r.degrees == 0 {
                return Rgba { width: w.size.0, height: w.size.1, data: j.out };
            }
            let mut out = Rgba::new(w.size.0, w.size.1);
            for ly in 0..lh {
                for lx in 0..lw {
                    let (pc, pr) = match r.degrees {
                        90 => (ly, lw - 1 - lx),
                        180 => (lw - 1 - lx, lh - 1 - ly),
                        _ => (lh - 1 - ly, lx),
                    };
                    let s = (pr * j.ow + pc) * 4;
                    let d = (ly * lw + lx) * 4;
                    out.data[d..d + 4].copy_from_slice(&j.out[s..s + 4]);
                }
            }
            out
        })
        .collect())
}

/// Alpha of the page at each point (x, y), reading `png` row by row (the plugin pairs atlas
/// pages with unnamed PNG files by where their regions are opaque).
pub fn alpha_at(png: &[u8], points: &[(u32, u32)]) -> Result<Vec<u8>, String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(png));
    dec.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = dec.read_info().map_err(|e| format!("png: {e}"))?;
    if reader.info().interlaced {
        let img = crate::image::decode_png(png)?;
        return Ok(points
            .iter()
            .map(|&(x, y)| if x < img.width && y < img.height { img.data[((y * img.width + x) * 4 + 3) as usize] } else { 0 })
            .collect());
    }
    let (color, _) = reader.output_color_type();
    let bpp = match color {
        png::ColorType::Rgba => 4,
        png::ColorType::GrayscaleAlpha => 2,
        _ => return Ok(vec![255; points.len()]),
    };
    let mut out = vec![0u8; points.len()];
    let mut y = 0u32;
    while let Some(r) = reader.next_row().map_err(|e| format!("png: {e}"))? {
        let data = r.data();
        for (o, &(px, py)) in out.iter_mut().zip(points) {
            if py == y && ((px as usize + 1) * bpp) <= data.len() {
                *o = data[px as usize * bpp + bpp - 1];
            }
        }
        y += 1;
    }
    Ok(out)
}

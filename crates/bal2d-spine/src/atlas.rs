//! Spine texture atlas (`.atlas` text) parser and attachment binding.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::data::*;

#[derive(Clone, Debug, Default)]
pub struct AtlasPage {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub pma: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AtlasRegion {
    pub name: String,
    pub page: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub offset_x: i32,
    pub offset_y: i32,
    pub original_width: i32,
    pub original_height: i32,
    pub degrees: i32,
    pub index: i32,
    pub u: f32,
    pub v: f32,
    pub u2: f32,
    pub v2: f32,
}

impl AtlasRegion {
    pub fn texture_region(&self, page: &AtlasPage) -> TextureRegion {
        let (pw, ph) = if self.degrees == 90 {
            (self.height as f32, self.width as f32)
        } else {
            (self.width as f32, self.height as f32)
        };
        TextureRegion {
            page: self.page,
            u: self.u,
            v: self.v,
            u2: self.u2,
            v2: self.v2,
            degrees: self.degrees,
            offset_x: self.offset_x as f32,
            offset_y: self.offset_y as f32,
            width: self.width as f32,
            height: self.height as f32,
            original_width: self.original_width as f32,
            original_height: self.original_height as f32,
            packed_width: pw,
            packed_height: ph,
            page_width: page.width as f32,
            page_height: page.height as f32,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Atlas {
    pub pages: Vec<AtlasPage>,
    pub regions: Vec<AtlasRegion>,
}

fn entry(line: &str) -> Option<(&str, Vec<&str>)> {
    let line = line.trim();
    let colon = line.find(':')?;
    let key = line[..colon].trim();
    let values = line[colon + 1..].split(',').take(4).map(|s| s.trim()).collect();
    Some((key, values))
}

fn int(v: &[&str], i: usize) -> i32 {
    v.get(i).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0)
}

impl Atlas {
    pub fn parse(text: &str) -> Atlas {
        let mut atlas = Atlas::default();
        let mut lines = text.lines().peekable();
        // Skip leading blank lines and the (ignored) header entries.
        while lines.peek().is_some_and(|l| l.trim().is_empty()) {
            lines.next();
        }
        while let Some(l) = lines.peek() {
            if l.trim().is_empty() || entry(l).is_none() {
                break;
            }
            lines.next();
        }
        let mut page: Option<usize> = None;
        while let Some(line) = lines.next() {
            if line.trim().is_empty() {
                page = None;
                continue;
            }
            match page {
                None => {
                    let mut p = AtlasPage { name: line.trim().to_string(), ..Default::default() };
                    while let Some((key, values)) = lines.peek().and_then(|l| entry(l)) {
                        match key {
                            "size" => {
                                p.width = int(&values, 0) as u32;
                                p.height = int(&values, 1) as u32;
                            }
                            "pma" => p.pma = values.first() == Some(&"true"),
                            _ => {}
                        }
                        lines.next();
                    }
                    atlas.pages.push(p);
                    page = Some(atlas.pages.len() - 1);
                }
                Some(pi) => {
                    let mut r = AtlasRegion { name: line.trim().to_string(), page: pi as u32, index: -1, ..Default::default() };
                    while let Some((key, values)) = lines.peek().and_then(|l| entry(l)) {
                        match key {
                            "xy" => {
                                r.x = int(&values, 0);
                                r.y = int(&values, 1);
                            }
                            "size" => {
                                r.width = int(&values, 0);
                                r.height = int(&values, 1);
                            }
                            "bounds" => {
                                r.x = int(&values, 0);
                                r.y = int(&values, 1);
                                r.width = int(&values, 2);
                                r.height = int(&values, 3);
                            }
                            "offset" => {
                                r.offset_x = int(&values, 0);
                                r.offset_y = int(&values, 1);
                            }
                            "orig" => {
                                r.original_width = int(&values, 0);
                                r.original_height = int(&values, 1);
                            }
                            "offsets" => {
                                r.offset_x = int(&values, 0);
                                r.offset_y = int(&values, 1);
                                r.original_width = int(&values, 2);
                                r.original_height = int(&values, 3);
                            }
                            "rotate" => {
                                let v = values.first().copied().unwrap_or("false");
                                r.degrees = match v {
                                    "true" => 90,
                                    "false" => 0,
                                    s => s.parse().unwrap_or(0),
                                };
                            }
                            "index" => r.index = int(&values, 0),
                            _ => {}
                        }
                        lines.next();
                    }
                    if r.original_width == 0 && r.original_height == 0 {
                        r.original_width = r.width;
                        r.original_height = r.height;
                    }
                    let p = &atlas.pages[pi];
                    let (pw, ph) = (p.width.max(1) as f32, p.height.max(1) as f32);
                    r.u = r.x as f32 / pw;
                    r.v = r.y as f32 / ph;
                    if r.degrees == 90 {
                        r.u2 = (r.x + r.height) as f32 / pw;
                        r.v2 = (r.y + r.width) as f32 / ph;
                    } else {
                        r.u2 = (r.x + r.width) as f32 / pw;
                        r.v2 = (r.y + r.height) as f32 / ph;
                    }
                    atlas.regions.push(r);
                }
            }
        }
        atlas
    }

    pub fn region(&self, i: usize) -> TextureRegion {
        let r = &self.regions[i];
        r.texture_region(&self.pages[r.page as usize])
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.regions.iter().position(|r| r.name == name)
    }
}

/// Resolves every region/mesh attachment (and sequence frame) against `atlas` and computes
/// their UVs / corner offsets. Returns the names of regions that were not found.
pub fn bind_atlas(data: &mut SkeletonData, atlas: &Atlas) -> Vec<String> {
    let mut missing = Vec::new();
    let lookup = |path: &str, missing: &mut Vec<String>| -> Option<TextureRegion> {
        match atlas.find(path) {
            Some(i) => Some(atlas.region(i)),
            None => {
                missing.push(path.to_string());
                None
            }
        }
    };
    for a in &mut data.attachments {
        match &mut a.kind {
            AttachmentKind::Region(r) => {
                if let Some(seq) = &mut r.sequence {
                    for i in 0..seq.count {
                        let p = seq.path(&r.path, i);
                        seq.regions[i as usize] = atlas.find(&p).map(|x| x as u32);
                        if seq.regions[i as usize].is_none() {
                            missing.push(p);
                        }
                    }
                    let setup = seq.regions.get(seq.setup_index as usize).copied().flatten();
                    r.region = setup.map(|i| atlas.region(i as usize));
                } else {
                    r.region = lookup(&r.path, &mut missing);
                }
                crate::attachment::update_region_attachment(r);
            }
            AttachmentKind::Mesh(m) => {
                if let Some(seq) = &mut m.sequence {
                    for i in 0..seq.count {
                        let p = seq.path(&m.path, i);
                        seq.regions[i as usize] = atlas.find(&p).map(|x| x as u32);
                        if seq.regions[i as usize].is_none() {
                            missing.push(p);
                        }
                    }
                    let setup = seq.regions.get(seq.setup_index as usize).copied().flatten();
                    m.region = setup.map(|i| atlas.region(i as usize));
                } else {
                    m.region = lookup(&m.path, &mut missing);
                }
                crate::attachment::update_mesh_uvs(m);
            }
            _ => {}
        }
    }
    missing
}

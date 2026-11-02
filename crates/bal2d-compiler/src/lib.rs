//! Phone-side compiler: original lobby assets in, device pack out.
//!
//! 1. Simulate every animation (over the idle loop, as the lobby plays them) and record, per
//!    attachment, whether any triangle reaches the viewport and the largest screen pixels per
//!    texel it is drawn at.
//! 2. Drop attachments that are never visible, and mesh triangles (with their vertices) that
//!    never are.
//! 3. Resample each atlas region to the density it is actually shown at (within a texel
//!    budget) and repack the regions into new pages.
//! 4. Convert pages to the GPU format and write the pack (skeletons re-encoded in the Spine 4.2
//!    binary format, so the watch reuses the same reader).
//!
//! Pure bytes in / bytes out: runs in the AstroBox plugin (wasm) and the host tool alike.

pub mod image;
mod bake;
mod hidden;
pub mod idle;
mod prune;
mod simplify;
mod trim;
pub mod interactions;
pub mod raster;
pub mod pack_atlas;
pub mod stream;

use std::collections::BTreeMap;

use bal2d_pack::{Header, Interactions, PageFormat, Section};
use bal2d_render::DrawList;
use bal2d_spine::state::{AnimationState, AnimationStateData};
use bal2d_spine::*;

pub use image::Rgba;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub cx: f32,
    pub cy: f32,
    pub scale: f32,
}

#[derive(Clone)]
pub struct LayerInput {
    pub name: String,
    pub skel: Vec<u8>,
    pub atlas: String,
    /// Atlas page file name -> PNG bytes (shared: the AstroBox plugin's 128 MiB cannot hold
    /// copies of a lobby's pages).
    pub pages: Vec<(String, std::sync::Arc<[u8]>)>,
}

/// What a compile is doing ([`compile_progress`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Reading the skeletons and atlases.
    Reading,
    /// Playing the animations to find what shows and how large.
    Analysing,
    /// Cutting the regions out of PNG page `page` (1-based) of `pages`.
    Textures { page: usize, pages: usize },
    /// Compositing the static background.
    Baking,
    /// Playing the idle loops into ready GPU calls.
    Idle,
    /// Encoding and writing the pack.
    Writing,
}

/// Progress reports with the overall fraction (0..=1) done.
struct Progress<'a> {
    report: &'a mut dyn FnMut(Stage, f32),
    lo: f32,
    hi: f32,
}

impl Progress<'_> {
    /// `at` (0..=1) of the current compile pass.
    fn at(&mut self, stage: Stage, at: f32) {
        (self.report)(stage, self.lo + (self.hi - self.lo) * at.clamp(0.0, 1.0));
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub width: u32,
    pub height: u32,
    pub camera: Option<Camera>,
    /// Other framings by lobby phase (`bal2d_pack::phase`, not IDLE: that is `camera`). Whatever
    /// any of them shows is kept, at the largest density any of them draws it.
    pub phase_cameras: Vec<(u8, Camera)>,
    pub zoom: f32,
    pub format: PageFormat,
    /// Total texel budget across all pages (pixels).
    pub budget: u32,
    /// Texels per screen pixel to keep (1.0 = exactly screen density).
    pub quality: f32,
    pub max_page: u32,
    /// Extra viewport margin in screen pixels when deciding visibility.
    pub margin: f32,
    /// Simulation step for sampling animations (seconds).
    pub step: f32,
    /// Weight of the intro's texel density demand (it plays once per load).
    pub intro_weight: f32,
    pub name: String,
    /// Composite the static bottom of the scene into one image (see `bake`).
    pub bake: bool,
    /// Keep every layer: no probe compile to find layers that never show (faster; for
    /// overviews of the whole scene).
    pub keep_layers: bool,
    /// Mesh simplification tolerance in target pixels (0: off): interior vertices go while no
    /// point of the mesh moves further than this in any simulated frame (see `simplify`).
    pub simplify: f32,
    /// Bake the idle loops into GPU calls (see `idle`), grouped within this many target pixels.
    pub idle: bool,
    pub idle_tolerance: f32,
    /// Pack size the idle bake may grow the pack to (it is baked less often a second if the
    /// pack would be larger; packs go to the watch at some 40-60 KB/s).
    pub max_pack_bytes: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            width: 336,
            height: 480,
            camera: None,
            phase_cameras: Vec::new(),
            zoom: 1.0,
            format: PageFormat::Bgra8888,
            budget: 1_200_000,
            quality: 1.0,
            max_page: 1024,
            margin: 24.0,
            step: 1.0 / 15.0,
            intro_weight: 0.4,
            name: String::new(),
            bake: true,
            keep_layers: false,
            simplify: 0.5,
            idle: true,
            idle_tolerance: idle::TOLERANCE,
            max_pack_bytes: 2_000_000,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LayerReport {
    pub name: String,
    /// Animations the player never starts, and timelines that cannot change a pixel, removed.
    pub animations_dropped: usize,
    pub timelines_dropped: usize,
    /// Mesh triangles never on screen, and the vertices only they used, removed.
    pub triangles_trimmed: usize,
    pub vertices_trimmed: usize,
    /// Removed by mesh simplification.
    pub triangles_simplified: usize,
    pub vertices_simplified: usize,
    pub attachments: usize,
    pub kept: usize,
    pub regions_before: usize,
    pub regions_after: usize,
    pub texels_before: u64,
    pub texels_after: u64,
    pub pages: Vec<(u32, u32)>,
    pub skel_bytes: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub camera: Option<Camera>,
    pub budget_scale: f32,
    pub layers: Vec<LayerReport>,
    pub pack_bytes: usize,
    /// Baked slots, the triangles they had, the baked rectangle (x, y, w, h), and whether the
    /// image only replaces them after the intro.
    pub baked: Option<(usize, usize, [u16; 4], bool)>,
    /// Skeletons left out because they never show (always behind another layer).
    pub hidden_layers: Vec<String>,
    /// The baked idle (see `idle`).
    pub idle: idle::IdleReport,
}

struct Layer {
    name: String,
    data: SkeletonData,
    atlas: Atlas,
    pages: Vec<Rgba>,
}

fn load(input: &LayerInput) -> Result<Layer, String> {
    let mut data = read_skeleton(&input.skel, 1.0).map_err(|e| format!("{}: {e}", input.name))?;
    let atlas = Atlas::parse(&input.atlas);
    let missing = bind_atlas(&mut data, &atlas);
    if !missing.is_empty() {
        return Err(format!("{}: atlas is missing {} regions (first: {})", input.name, missing.len(), missing[0]));
    }
    for p in &atlas.pages {
        if !input.pages.iter().any(|(n, _)| n == &p.name) {
            return Err(format!("{}: page {} not provided", input.name, p.name));
        }
    }
    // The pages are not decoded here: `work_atlases` cuts the needed regions out of the PNGs.
    Ok(Layer { name: input.name.clone(), data, atlas, pages: Vec::new() })
}

/// The work atlas may hold this many times the texel budget (its regions at the density they
/// are drawn at, before the budget scales them down): enough for the bake and for an exact
/// final resampling, bounded whatever the source pages are.
const WORK_BUDGET: f64 = 4.0;
/// Work pages live only in memory: no GPU limit on their size.
const WORK_MAX_PAGE: u32 = 16384;

/// Replaces every layer's atlas with a work atlas for the bake: the regions its live attachments
/// draw, cut out of the PNG pages row by row ([`stream::extract`]) at the density they are drawn
/// at (within `WORK_BUDGET`), packed unrotated and premultiplied. Attachments never seen are
/// culled first. The source pages are never decoded whole (the AstroBox plugin has 128 MiB, a
/// 4096 page alone is 64 MiB). Returns each layer's source texel count and source atlas (for
/// [`source_atlases`]).
fn work_atlases(layers: &mut [Layer], inputs: &[LayerInput], cam: Camera, opts: &Options, progress: &mut Progress) -> Result<(Vec<u64>, Vec<Atlas>), String> {
    let mut texels = Vec::with_capacity(layers.len());
    let mut sources = Vec::with_capacity(layers.len());
    let n = layers.len() as f32;
    for (li, (layer, input)) in layers.iter_mut().zip(inputs).enumerate() {
        let (lo, span) = (0.05 + 0.4 * li as f32 / n, 0.4 / n);
        progress.at(Stage::Analysing, lo);
        texels.push(layer.atlas.regions.iter().map(|r| r.width as u64 * r.height as u64).sum());
        let (seen, _) = simulate(layer, cam, opts, &mut None);
        cull(&mut layer.data, &seen);
        let needs = region_needs(layer, &seen);
        let mut scales: Vec<f32> = needs.iter().map(|n| (n.1 * opts.quality).clamp(1.0 / 64.0, 1.0)).collect();
        let area: f64 = needs
            .iter()
            .zip(&scales)
            .map(|(n, s)| ((n.2[2] - n.2[0]) as f64 * *s as f64 + 4.0) * ((n.2[3] - n.2[1]) as f64 * *s as f64 + 4.0))
            .sum();
        let cap = opts.budget as f64 * WORK_BUDGET;
        if area > cap {
            let k = (cap / area).sqrt() as f32;
            scales.iter_mut().for_each(|s| *s *= k);
        }
        let items: Vec<pack_atlas::Item> = needs
            .iter()
            .zip(&scales)
            .map(|(&(region, _, crop), &scale)| pack_atlas::Item { region, scale, rotated: layer.atlas.regions[region].degrees != 0, crop })
            .collect();
        let images = extract_items(&layer.atlas, input, &items, &mut |page, pages| {
            progress.at(Stage::Textures { page, pages }, lo + span * (0.3 + 0.7 * (page - 1) as f32 / pages.max(1) as f32))
        })?;
        let packed = pack_atlas::build_images(&layer.atlas, &items, images, WORK_MAX_PAGE)?;
        let atlas = Atlas::parse(&packed.atlas_text);
        // Culled attachments stay in the list (unbound now; never written): only the live ones
        // must find their regions.
        let missing = bind_atlas(&mut layer.data, &atlas);
        let live: std::collections::BTreeSet<String> = layer
            .data
            .skins
            .iter()
            .flat_map(|s| s.attachments.iter().map(|e| e.2))
            .flat_map(|id| region_paths(&layer.data.attachments[id as usize]))
            .collect();
        if let Some(m) = missing.iter().find(|m| live.contains(*m)) {
            return Err(format!("{}: work atlas misses region {m}", layer.name));
        }
        sources.push(std::mem::replace(&mut layer.atlas, atlas));
        layer.pages = packed.pages;
    }
    Ok((texels, sources))
}

/// Binds the layers back to their source atlases once the work atlases have served (the bake):
/// the final regions are cut from the sources at their final size in one resampling, with the
/// source offsets (a work atlas's are rounded to whole work texels: going through it would round
/// twice and shift regions by up to a pixel). Frees the work pages.
fn source_atlases(layers: &mut [Layer], sources: Vec<Atlas>) {
    for (layer, atlas) in layers.iter_mut().zip(sources) {
        // Culled attachments miss their regions in either atlas; the live ones bound before.
        let _ = bind_atlas(&mut layer.data, &atlas);
        layer.atlas = atlas;
        layer.pages = Vec::new();
    }
}

/// The items' images cut out of the layer's PNG pages row by row ([`stream::extract`]),
/// premultiplied, at [`pack_atlas::item_size`].
fn extract_items(atlas: &Atlas, input: &LayerInput, items: &[pack_atlas::Item], page_done: &mut dyn FnMut(usize, usize)) -> Result<Vec<Rgba>, String> {
    let mut images: Vec<Option<Rgba>> = (0..items.len()).map(|_| None).collect();
    for (pi, page) in atlas.pages.iter().enumerate() {
        page_done(pi + 1, atlas.pages.len());
        let on_page: Vec<usize> = (0..items.len()).filter(|&i| atlas.regions[items[i].region].page as usize == pi).collect();
        if on_page.is_empty() {
            continue;
        }
        let png = input.pages.iter().find(|(n, _)| n == &page.name).ok_or_else(|| format!("{}: page {} not provided", input.name, page.name))?;
        let wants: Vec<stream::Want> = on_page
            .iter()
            .map(|&i| stream::Want { region: items[i].region, crop: items[i].crop, size: pack_atlas::item_size(&items[i]) })
            .collect();
        // Straight alpha in the source atlases; premultiplied from here on.
        let out = stream::extract(atlas, &png.1, !page.pma, &wants).map_err(|e| format!("{}: {e}", page.name))?;
        for (i, img) in on_page.into_iter().zip(out) {
            images[i] = Some(img);
        }
    }
    Ok(images.into_iter().map(|i| i.unwrap_or_else(|| Rgba::new(1, 1))).collect())
}

/// The atlas paths an attachment draws: its own, or each frame of its sequence.
fn region_paths(a: &Attachment) -> Vec<String> {
    let (path, seq) = match &a.kind {
        AttachmentKind::Region(r) => (&r.path, &r.sequence),
        AttachmentKind::Mesh(m) => (&m.path, &m.sequence),
        _ => return Vec::new(),
    };
    match seq {
        Some(s) => (0..s.count).map(|i| s.path(path, i)).collect(),
        None => vec![path.clone()],
    }
}

/// Per atlas region of the live attachments: the highest density they are drawn at and the
/// visible crop in logical pixels (two texels of margin for bilinear filtering; the whole region
/// when its bounds are empty).
fn region_needs(layer: &Layer, seen: &[Seen]) -> Vec<(usize, f32, [u32; 4])> {
    let mut density: BTreeMap<usize, (f32, [f32; 4])> = BTreeMap::new();
    let live: Vec<u32> = layer.data.skins.iter().flat_map(|s| s.attachments.iter().map(|e| e.2)).collect();
    for id in live {
        let sn = &seen[id as usize];
        for path in region_paths(&layer.data.attachments[id as usize]) {
            if let Some(ri) = layer.atlas.find(&path) {
                let e = density.entry(ri).or_insert((0.0, [f32::MAX, f32::MAX, f32::MIN, f32::MIN]));
                e.0 = e.0.max(sn.density);
                e.1[0] = e.1[0].min(sn.bounds[0]);
                e.1[1] = e.1[1].min(sn.bounds[1]);
                e.1[2] = e.1[2].max(sn.bounds[2]);
                e.1[3] = e.1[3].max(sn.bounds[3]);
            }
        }
    }
    density
        .into_iter()
        .map(|(ri, (d, b))| {
            let r = &layer.atlas.regions[ri];
            let (w, h) = (r.width as f32, r.height as f32);
            let crop = if b[0] <= b[2] && b[1] <= b[3] {
                [
                    (b[0] - 2.0).floor().clamp(0.0, w - 1.0) as u32,
                    (b[1] - 2.0).floor().clamp(0.0, h - 1.0) as u32,
                    (b[2] + 2.0).ceil().clamp(1.0, w) as u32,
                    (b[3] + 2.0).ceil().clamp(1.0, h) as u32,
                ]
            } else {
                [0, 0, r.width as u32, r.height as u32]
            };
            let crop = if crop[2] > crop[0] && crop[3] > crop[1] { crop } else { [0, 0, r.width as u32, r.height as u32] };
            let crop = if std::env::var("BAL2D_NO_CROP").is_ok() { [0, 0, r.width as u32, r.height as u32] } else { crop };
            (ri, d, crop)
        })
        .collect()
}

/// Head-centered default framing (see `bal2d-tool`): the game's 1440-unit frame height.
pub fn auto_camera(data: &SkeletonData, width: u32, height: u32, zoom: f32) -> Camera {
    let mut sk = Skeleton::new(data);
    sk.update_world_transform(data, Physics::Update);
    let (hx, hy) = match data.find_bone("Touch_Point") {
        Some(b) => (sk.bones[b].world_x, sk.bones[b].world_y),
        None => (data.x + data.width / 2.0, data.y + data.height / 2.0),
    };
    let _ = width;
    let scale = height as f32 / 1440.0 * zoom;
    let cy = 720.0 + (hy - 720.0) * (1.0 - 1.0 / zoom);
    Camera { cx: hx, cy, scale }
}

/// Per-attachment observations from the simulation.
#[derive(Clone, Copy)]
struct Seen {
    visible: bool,
    /// Max screen pixels per source texel.
    density: f32,
    /// Visible part in the region's logical pixels (x0, y0, x1, y1; y down).
    bounds: [f32; 4],
}

impl Default for Seen {
    fn default() -> Seen {
        Seen { visible: false, density: 0.0, bounds: [f32::MAX, f32::MAX, f32::MIN, f32::MIN] }
    }
}

/// Sutherland-Hodgman clip of a polygon against an axis-aligned rectangle.
fn clip_rect(poly: &[[f32; 2]], r: [f32; 4]) -> Vec<[f32; 2]> {
    let mut out: Vec<[f32; 2]> = poly.to_vec();
    for edge in 0..4 {
        let input = std::mem::take(&mut out);
        if input.is_empty() {
            break;
        }
        let inside = |p: &[f32; 2]| match edge {
            0 => p[0] >= r[0],
            1 => p[0] <= r[2],
            2 => p[1] >= r[1],
            _ => p[1] <= r[3],
        };
        let cross = |a: &[f32; 2], b: &[f32; 2]| -> [f32; 2] {
            let t = match edge {
                0 => (r[0] - a[0]) / (b[0] - a[0]),
                1 => (r[2] - a[0]) / (b[0] - a[0]),
                2 => (r[1] - a[1]) / (b[1] - a[1]),
                _ => (r[3] - a[1]) / (b[1] - a[1]),
            };
            [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
        };
        for i in 0..input.len() {
            let cur = &input[i];
            let prev = &input[(i + input.len() - 1) % input.len()];
            match (inside(prev), inside(cur)) {
                (true, true) => out.push(*cur),
                (true, false) => out.push(cross(prev, cur)),
                (false, true) => {
                    out.push(cross(prev, cur));
                    out.push(*cur);
                }
                (false, false) => {}
            }
        }
    }
    out
}

/// Page texel -> region logical pixel (x right, y down, within the stripped region).
fn page_to_logical(r: &bal2d_spine::atlas::AtlasRegion, px: f32, py: f32) -> (f32, f32) {
    let (dx, dy) = (px - r.x as f32, py - r.y as f32);
    match r.degrees {
        90 => (r.width as f32 - dy, dx),
        180 => (r.width as f32 - dx, r.height as f32 - dy),
        270 => (dy, r.height as f32 - dx),
        _ => (dx, dy),
    }
}

fn sigma_max(j: [f32; 4]) -> f32 {
    // Largest singular value of [[a, b], [c, d]].
    let [a, b, c, d] = j;
    let s1 = a * a + b * b + c * c + d * d;
    let det = a * d - b * c;
    let disc = (s1 * s1 - 4.0 * det * det).max(0.0).sqrt();
    ((s1 + disc) * 0.5).sqrt()
}

#[allow(clippy::too_many_arguments)]
fn observe(
    layer: &Layer,
    list: &DrawList,
    cam: Camera,
    opts: &Options,
    seen: &mut [Seen],
    tri_seen: &mut trim::TriSeen,
    weight: f32,
    regions: &[Option<usize>],
    samples: &mut Option<simplify::Samples>,
) {
    let (w2, h2) = (opts.width as f32 * 0.5, opts.height as f32 * 0.5);
    let (minx, miny) = (-opts.margin, -opts.margin);
    let (maxx, maxy) = (opts.width as f32 + opts.margin, opts.height as f32 + opts.margin);
    for b in &list.batches {
        let page = &layer.atlas.pages[b.page as usize];
        let (pw, ph) = (page.width as f32, page.height as f32);
        let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
        let s = &mut seen[b.attachment as usize];
        // Per-triangle visibility, when the batch is the mesh's own triangles (not clipped).
        let ai = b.attachment as usize;
        let mut tri_flags = match &layer.data.attachments[ai].kind {
            AttachmentKind::Mesh(m) if !tri_seen.unknown[ai] => {
                if m.triangles.as_slice() == idx && b.vertex_count as usize == m.vertices.vertex_count() {
                    if let Some(s) = samples.as_mut() {
                        let v0 = b.first_vertex as usize * 2;
                        s.offer(ai, &list.positions[v0..v0 + b.vertex_count as usize * 2]);
                    }
                    let f = &mut tri_seen.seen[ai];
                    if f.len() != idx.len() / 3 {
                        *f = vec![false; idx.len() / 3];
                    }
                    Some(f)
                } else {
                    tri_seen.unknown[ai] = true;
                    None
                }
            }
            _ => None,
        };
        for (ti, t) in idx.chunks_exact(3).enumerate() {
            let mut sp = [[0f32; 2]; 3];
            let mut tp = [[0f32; 2]; 3];
            for k in 0..3 {
                let vi = b.first_vertex as usize + t[k] as usize;
                sp[k] = [
                    (list.positions[vi * 2] - cam.cx) * cam.scale + w2,
                    h2 - (list.positions[vi * 2 + 1] - cam.cy) * cam.scale,
                ];
                tp[k] = [list.uvs[vi * 2] * pw, list.uvs[vi * 2 + 1] * ph];
            }
            let bx0 = sp.iter().map(|p| p[0]).fold(f32::MAX, f32::min);
            let bx1 = sp.iter().map(|p| p[0]).fold(f32::MIN, f32::max);
            let by0 = sp.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
            let by1 = sp.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
            if bx1 < minx || bx0 > maxx || by1 < miny || by0 > maxy {
                continue;
            }
            s.visible = true;
            if let Some(f) = tri_flags.as_mut() {
                f[ti] = true;
            }
            // Visible part of the triangle, back in the region's logical pixels.
            if let Some(ri) = regions[b.attachment as usize] {
                let det_s = (sp[1][0] - sp[0][0]) * (sp[2][1] - sp[0][1]) - (sp[2][0] - sp[0][0]) * (sp[1][1] - sp[0][1]);
                if det_s.abs() > 1e-6 {
                    let clipped = clip_rect(&sp, [minx, miny, maxx, maxy]);
                    let region = &layer.atlas.regions[ri];
                    for q in clipped {
                        // Barycentric weights of q in the screen triangle -> texel position.
                        let l1 = ((q[0] - sp[0][0]) * (sp[2][1] - sp[0][1]) - (sp[2][0] - sp[0][0]) * (q[1] - sp[0][1])) / det_s;
                        let l2 = ((sp[1][0] - sp[0][0]) * (q[1] - sp[0][1]) - (q[0] - sp[0][0]) * (sp[1][1] - sp[0][1])) / det_s;
                        let tx = tp[0][0] + (tp[1][0] - tp[0][0]) * l1 + (tp[2][0] - tp[0][0]) * l2;
                        let ty = tp[0][1] + (tp[1][1] - tp[0][1]) * l1 + (tp[2][1] - tp[0][1]) * l2;
                        let (lx, ly) = page_to_logical(region, tx, ty);
                        s.bounds[0] = s.bounds[0].min(lx);
                        s.bounds[1] = s.bounds[1].min(ly);
                        s.bounds[2] = s.bounds[2].max(lx);
                        s.bounds[3] = s.bounds[3].max(ly);
                    }
                }
            }
            // J maps texel deltas to screen deltas: S = J T.
            let (t1, t2) = ([tp[1][0] - tp[0][0], tp[1][1] - tp[0][1]], [tp[2][0] - tp[0][0], tp[2][1] - tp[0][1]]);
            let (s1, s2) = ([sp[1][0] - sp[0][0], sp[1][1] - sp[0][1]], [sp[2][0] - sp[0][0], sp[2][1] - sp[0][1]]);
            let det = t1[0] * t2[1] - t2[0] * t1[1];
            if det.abs() < 1e-3 {
                continue;
            }
            let inv = [t2[1] / det, -t2[0] / det, -t1[1] / det, t1[0] / det];
            let j = [
                s1[0] * inv[0] + s2[0] * inv[2],
                s1[0] * inv[1] + s2[0] * inv[3],
                s1[1] * inv[0] + s2[1] * inv[2],
                s1[1] * inv[1] + s2[1] * inv[3],
            ];
            let sig = sigma_max(j) * weight;
            if sig.is_finite() {
                s.density = s.density.max(sig);
            }
        }
    }
}

/// Plays every animation the way the lobby uses it and records what is seen.
/// [`simulate_one`] through every distinct framing (`cam` and the phase cameras), merged: seen
/// under any, at the largest density, over the union of the bounds and triangles.
/// With `samples`, the mesh vertices' world positions are recorded too (one framing: they do
/// not depend on it).
fn simulate(layer: &Layer, cam: Camera, opts: &Options, samples: &mut Option<simplify::Samples>) -> (Vec<Seen>, trim::TriSeen) {
    let mut cams = vec![cam];
    for (_, c) in &opts.phase_cameras {
        if !cams.contains(c) {
            cams.push(*c);
        }
    }
    let (mut seen, mut tri) = simulate_one(layer, cams[0], opts, samples);
    for c in &cams[1..] {
        let (s2, t2) = simulate_one(layer, *c, opts, &mut None);
        for (a, b) in seen.iter_mut().zip(s2) {
            a.visible |= b.visible;
            a.density = a.density.max(b.density);
            a.bounds = [a.bounds[0].min(b.bounds[0]), a.bounds[1].min(b.bounds[1]), a.bounds[2].max(b.bounds[2]), a.bounds[3].max(b.bounds[3])];
        }
        for (a, b) in tri.seen.iter_mut().zip(t2.seen) {
            if a.is_empty() {
                *a = b;
            } else {
                for (x, y) in a.iter_mut().zip(b) {
                    *x |= y;
                }
            }
        }
        for (a, b) in tri.unknown.iter_mut().zip(t2.unknown) {
            *a |= b;
        }
    }
    (seen, tri)
}

fn simulate_one(layer: &Layer, cam: Camera, opts: &Options, samples: &mut Option<simplify::Samples>) -> (Vec<Seen>, trim::TriSeen) {
    let d = &layer.data;
    let mut seen = vec![Seen::default(); d.attachments.len()];
    let mut tri_seen = trim::TriSeen::new(d.attachments.len());
    let regions: Vec<Option<usize>> = d
        .attachments
        .iter()
        .map(|a| match &a.kind {
            AttachmentKind::Region(r) => layer.atlas.find(&r.path),
            AttachmentKind::Mesh(m) => layer.atlas.find(&m.path),
            _ => None,
        })
        .collect();
    let mut list = DrawList::default();
    let idle = d.find_animation("Idle_01").or_else(|| d.animations.iter().position(|a| a.name.starts_with("Idle")));
    for (ai, a) in d.animations.iter().enumerate() {
        let mut sk = Skeleton::new(d);
        sk.update_world_transform(d, Physics::Update);
        let mut st = AnimationState::new(d, AnimationStateData { default_mix: 0.25, mixes: vec![] });
        let base = a.name.starts_with("Idle") || a.name.starts_with("Start") || Some(ai) == idle || idle.is_none();
        // The intro plays once per load: it may be softer than what is on screen all day.
        let weight = if a.name.starts_with("Start") { opts.intro_weight } else { 1.0 };
        if base {
            st.set_animation(d, 0, Some(ai), false);
        } else {
            st.set_animation(d, 0, idle, true);
            st.set_animation(d, 1, Some(ai), false);
        }
        let dur = a.duration.max(opts.step);
        let mut t = 0.0;
        let mut first = true;
        while t <= dur + 1e-4 {
            let dt = if first { 0.0 } else { opts.step };
            first = false;
            st.update(d, dt);
            st.apply(d, &mut sk);
            sk.update(dt);
            sk.update_world_transform(d, Physics::Update);
            st.output.clear();
            bal2d_render::build(d, &sk, &mut list);
            observe(layer, &list, cam, opts, &mut seen, &mut tri_seen, weight, &regions, samples);
            t += opts.step;
        }
    }
    (seen, tri_seen)
}

/// Removes never-visible render attachments from every skin (keeps parents of kept linked
/// meshes, and every non-rendering attachment: paths, clipping, points, boxes).
fn cull(d: &mut SkeletonData, seen: &[Seen]) -> usize {
    let mut keep: Vec<bool> = d
        .attachments
        .iter()
        .enumerate()
        .map(|(i, a)| match a.kind {
            AttachmentKind::Region(_) | AttachmentKind::Mesh(_) => seen[i].visible,
            AttachmentKind::BoundingBox(_) | AttachmentKind::Point(_) => false,
            _ => true,
        })
        .collect();
    for i in 0..d.attachments.len() {
        if keep[i] {
            if let AttachmentKind::Mesh(m) = &d.attachments[i].kind {
                if let Some(p) = m.parent_mesh {
                    keep[p as usize] = true;
                }
            }
            let ta = d.attachments[i].timeline_attachment as usize;
            keep[ta] = keep[ta] || ta == i;
        }
    }
    for skin in &mut d.skins {
        skin.attachments.retain(|(_, _, id)| keep[*id as usize]);
    }
    for a in &mut d.animations {
        a.timelines.retain(|t| match t {
            Timeline::Deform { attachment, .. } | Timeline::Sequence { attachment, .. } => keep[*attachment as usize],
            _ => true,
        });
    }
    keep.iter().filter(|k| **k).count()
}

/// Compiles the layers into a pack. With several skeletons, the ones that never show (see
/// `hidden`) are found on a first compile without a baked background, and left out.
pub fn compile(inputs: &[LayerInput], opts: &Options, interactions: Option<Interactions>) -> Result<(Vec<u8>, Report), String> {
    compile_progress(inputs, opts, interactions, &mut |_, _| {})
}

/// [`compile`], reporting its stages and the fraction done as it goes.
pub fn compile_progress(
    inputs: &[LayerInput],
    opts: &Options,
    interactions: Option<Interactions>,
    report: &mut dyn FnMut(Stage, f32),
) -> Result<(Vec<u8>, Report), String> {
    if inputs.len() < 2 || opts.keep_layers || std::env::var("BAL2D_KEEP_LAYERS").is_ok() {
        return compile_once(inputs, opts, interactions, &mut Progress { report, lo: 0.0, hi: 1.0 });
    }
    // A probe compile finds the layers that never show; the final one leaves them out.
    let probe = Options { bake: false, ..opts.clone() };
    let (pack, _) = compile_once(inputs, &probe, interactions.clone(), &mut Progress { report: &mut *report, lo: 0.0, hi: 0.45 })?;
    report(Stage::Analysing, 0.47);
    let hidden = hidden::hidden_layers(&pack)?;
    let mut tail = Progress { report, lo: 0.5, hi: 1.0 };
    if hidden.is_empty() {
        return compile_once(inputs, opts, interactions, &mut tail);
    }
    let kept: Vec<LayerInput> = inputs.iter().enumerate().filter(|(i, _)| !hidden.contains(i)).map(|(_, l)| l.clone()).collect();
    // The interactive layer's index shifts down past the dropped ones below it.
    let interactions = interactions.map(|mut i| {
        let below = hidden.iter().filter(|&&h| h < i.layer as usize).count();
        i.layer -= below as u8;
        i
    });
    let (pack, mut report) = compile_once(&kept, opts, interactions, &mut tail)?;
    report.hidden_layers = hidden.iter().map(|&i| inputs[i].name.clone()).collect();
    Ok((pack, report))
}

fn compile_once(inputs: &[LayerInput], opts: &Options, interactions: Option<Interactions>, progress: &mut Progress) -> Result<(Vec<u8>, Report), String> {
    progress.at(Stage::Reading, 0.0);
    if inputs.is_empty() {
        return Err("no skeletons".into());
    }
    let mut layers: Vec<Layer> = inputs.iter().map(load).collect::<Result<_, _>>()?;
    let main = layers.iter().position(|l| l.data.find_bone("Touch_Point").is_some()).unwrap_or(layers.len() - 1);
    let cam = opts.camera.unwrap_or_else(|| auto_camera(&layers[main].data, opts.width, opts.height, opts.zoom));
    let mut report = Report { camera: Some(cam), ..Default::default() };
    let inter = interactions.unwrap_or_else(|| interactions::from_conventions(&layers[main].data, main as u8));
    let no_prune = std::env::var("BAL2D_NO_PRUNE").is_ok();
    let dropped: Vec<usize> = layers
        .iter_mut()
        .enumerate()
        .map(|(li, l)| if no_prune { 0 } else { prune::played_only(&mut l.data, li == main, &inter) })
        .collect();
    let drag_bones: Vec<&str> = inter.drags.iter().map(|d| d.bone.as_str()).collect();
    let (source_texels, sources) = work_atlases(&mut layers, inputs, cam, opts, progress)?;
    progress.at(Stage::Baking, 0.45);
    let baked = if opts.bake { bake::bake(&mut layers, main, &inter, cam, opts) } else { None };
    if let Some(b) = &baked {
        let h = b.header;
        report.baked = Some((b.slots.len(), b.triangles, [h.x, h.y, h.width, h.height], b.after_intro));
    }
    source_atlases(&mut layers, sources);

    // Observe, cull, and gather per-region scale requirements.
    progress.at(Stage::Analysing, 0.55);
    struct RegionNeed {
        layer: usize,
        region: usize,
        density: f32,
        /// Visible crop in logical pixels (x0, y0, x1, y1).
        crop: [u32; 4],
    }
    let mut needs: Vec<RegionNeed> = Vec::new();
    for (li, layer) in layers.iter_mut().enumerate() {
        // The largest scale any framing draws at: simplification tolerances are in its pixels.
        let max_scale = opts.phase_cameras.iter().map(|(_, c)| c.scale).fold(cam.scale, f32::max);
        let mut samples = (opts.simplify > 0.0).then(|| simplify::Samples::new(layer.data.attachments.len(), 0.02 / max_scale));
        let (seen, tri_seen) = simulate(layer, cam, opts, &mut samples);
        let total = layer.data.attachments.len();
        let kept = cull(&mut layer.data, &seen);
        let (tris_trimmed, verts_trimmed, maps) =
            if std::env::var("BAL2D_NO_TRIM").is_ok() { (0, 0, Vec::new()) } else { trim::trim_meshes(&mut layer.data, &tri_seen) };
        let (tris_simplified, verts_simplified) = match samples.as_mut() {
            Some(s) => {
                for (id, map) in maps.iter().enumerate() {
                    if let Some(map) = map {
                        s.remap(id, map);
                    }
                }
                let shared = trim::shared_meshes(&layer.data);
                let skip: Vec<bool> = shared.iter().zip(&tri_seen.unknown).map(|(a, b)| *a || *b).collect();
                simplify::simplify_meshes(&mut layer.data, s, opts.simplify / max_scale, &skip)
            }
            None => (0, 0),
        };
        drop(samples);
        let timelines_dropped = if no_prune { 0 } else { prune::dead_timelines(&mut layer.data, if li == main { &drag_bones } else { &[] }) };
        let region_list = region_needs(layer, &seen);
        let before = source_texels[li];
        report.layers.push(LayerReport {
            name: layer.name.clone(),
            attachments: total,
            kept,
            regions_before: layer.atlas.regions.len(),
            regions_after: region_list.len(),
            texels_before: before,
            animations_dropped: dropped[li],
            timelines_dropped,
            triangles_trimmed: tris_trimmed,
            vertices_trimmed: verts_trimmed,
            triangles_simplified: tris_simplified,
            vertices_simplified: verts_simplified,
            ..Default::default()
        });
        for (ri, d, crop) in region_list {
            needs.push(RegionNeed { layer: li, region: ri, density: d, crop });
        }
    }

    if std::env::var("BAL2D_DEBUG_REGIONS").is_ok() {
        let mut rows: Vec<(f64, String, f32, i32, i32)> = needs
            .iter()
            .map(|n| {
                let r = &layers[n.layer].atlas.regions[n.region];
                let s = (n.density * opts.quality).clamp(1.0 / 64.0, 1.0) as f64;
                let (cw, ch) = ((n.crop[2] - n.crop[0]) as i32, (n.crop[3] - n.crop[1]) as i32);
                (cw as f64 * ch as f64 * s * s, r.name.clone(), n.density, cw, ch)
            })
            .collect();
        rows.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (a, n, d, w, h) in rows.iter().take(15) {
            eprintln!("  need {:8.0} texels  density {:.3}  {}x{}  {}", a, d, w, h, n);
        }
    }
    // Scales within the budget.
    let mut scales: Vec<f32> = needs.iter().map(|n| (n.density * opts.quality).clamp(1.0 / 64.0, 1.0)).collect();
    let area = |scales: &[f32]| -> f64 {
        needs
            .iter()
            .zip(scales)
            .map(|(n, s)| {
                let (cw, ch) = ((n.crop[2] - n.crop[0]) as f64, (n.crop[3] - n.crop[1]) as f64);
                (cw * *s as f64 + 4.0) * (ch * *s as f64 + 4.0)
            })
            .sum()
    };
    let mut budget_scale = 1.0f32;
    let a0 = area(&scales);
    if a0 > opts.budget as f64 {
        budget_scale = (opts.budget as f64 / a0).sqrt() as f32;
        for s in &mut scales {
            *s *= budget_scale;
        }
    }
    report.budget_scale = budget_scale;

    // Resample and repack per layer.
    let mut sections = Vec::new();
    for (li, layer) in layers.iter().enumerate() {
        let mut items = Vec::new();
        for (n, s) in needs.iter().zip(&scales).filter(|(n, _)| n.layer == li) {
            let r = &layer.atlas.regions[n.region];
            items.push(pack_atlas::Item { region: n.region, scale: *s, rotated: r.degrees != 0, crop: n.crop });
        }
        let (lo, span) = (0.65 + 0.3 * li as f32 / layers.len() as f32, 0.3 / layers.len() as f32);
        let images = extract_items(&layer.atlas, &inputs[li], &items, &mut |page, pages| {
            progress.at(Stage::Textures { page, pages }, lo + span * (page - 1) as f32 / pages.max(1) as f32)
        })?;
        let packed = pack_atlas::build_images(&layer.atlas, &items, images, opts.max_page)?;
        let skel = writer::write_skeleton(&layer.data, 1.0);
        // Verify the re-encoded skeleton binds to the new atlas before shipping it.
        let mut check = read_skeleton(&skel, 1.0).map_err(|e| format!("{}: re-read: {e}", layer.name))?;
        let new_atlas = Atlas::parse(&packed.atlas_text);
        let missing = bind_atlas(&mut check, &new_atlas);
        if !missing.is_empty() {
            return Err(format!("{}: compiled atlas misses {} regions (first {})", layer.name, missing.len(), missing[0]));
        }
        let rep = &mut report.layers[li];
        rep.texels_after = packed.pages.iter().map(|p| p.width as u64 * p.height as u64).sum();
        rep.pages = packed.pages.iter().map(|p| (p.width, p.height)).collect();
        rep.skel_bytes = skel.len();
        sections.push(Section { kind: bal2d_pack::KIND_SKEL, layer: li as u16, index: 0, data: skel });
        sections.push(Section { kind: bal2d_pack::KIND_ATLAS, layer: li as u16, index: 0, data: packed.atlas_text.into_bytes() });
        for (pi, page) in packed.pages.iter().enumerate() {
            let pixels = image::encode_page(page, opts.format);
            let h = bal2d_pack::PageHeader { width: page.width as u16, height: page.height as u16, format: opts.format };
            sections.push(Section { kind: bal2d_pack::KIND_PAGE, layer: li as u16, index: pi as u16, data: bal2d_pack::page_payload(&h, &pixels) });
        }
    }
    if let Some(b) = &baked {
        sections.push(Section { kind: bal2d_pack::KIND_BAKE, layer: 0, index: 0, data: bal2d_pack::bake_payload(&b.header, &b.pixels) });
        let list = bal2d_pack::BakeSlots { after_intro: b.after_intro, slots: b.slots.iter().map(|&(l, s)| (l as u16, s as u16)).collect() };
        sections.push(Section { kind: bal2d_pack::KIND_BAKE_SLOTS, layer: 0, index: 0, data: list.encode() });
    }
    sections.push(Section { kind: bal2d_pack::KIND_INTERACTIONS, layer: 0, index: 0, data: inter.encode() });
    let name = if opts.name.is_empty() { layers[main].name.clone() } else { opts.name.clone() };
    sections.push(Section { kind: bal2d_pack::KIND_NAME, layer: 0, index: 0, data: name.into_bytes() });
    let framings: Vec<(u8, f32, f32, f32)> = opts
        .phase_cameras
        .iter()
        .filter(|(p, c)| *p != bal2d_pack::phase::IDLE && *c != cam)
        .map(|(p, c)| (*p, c.cx, c.cy, c.scale))
        .collect();
    if !framings.is_empty() {
        let data = bal2d_pack::PhaseCameras { cameras: framings }.encode();
        sections.push(Section { kind: bal2d_pack::KIND_CAMERAS, layer: 0, index: 0, data });
    }
    let header = Header {
        screen_width: opts.width as u16,
        screen_height: opts.height as u16,
        camera_cx: cam.cx,
        camera_cy: cam.cy,
        camera_scale: cam.scale,
        layer_count: layers.len() as u8,
        page_format: opts.format,
    };
    if !opts.idle {
        progress.at(Stage::Writing, 0.97);
        let bytes = bal2d_pack::write(&header, &sections, 9);
        report.pack_bytes = bytes.len();
        return Ok((bytes, report));
    }
    progress.at(Stage::Idle, 0.9);
    let probe = bal2d_pack::write(&header, &sections, 1);
    let mut rate = idle::RATE;
    let bytes = loop {
        let (bakes, mut rep) = idle::bake(&probe, rate, opts.idle_tolerance);
        rep.rate = rate;
        let mut with = sections.clone();
        for (li, b) in bakes {
            with.push(Section { kind: bal2d_pack::KIND_IDLE, layer: li, index: 0, data: b.encode() });
        }
        report.idle = rep;
        progress.at(Stage::Writing, 0.97);
        let bytes = bal2d_pack::write(&header, &with, 9);
        if bytes.len() <= opts.max_pack_bytes || rate <= idle::RATE / 4.0 + 0.01 {
            break bytes;
        }
        rate *= 0.5;
        progress.at(Stage::Idle, 0.93);
    };
    report.pack_bytes = bytes.len();
    Ok((bytes, report))
}

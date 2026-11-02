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
mod prune;
mod trim;
pub mod interactions;
pub mod raster;
pub mod pack_atlas;

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
    /// Atlas page file name -> PNG bytes.
    pub pages: Vec<(String, Vec<u8>)>,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub width: u32,
    pub height: u32,
    pub camera: Option<Camera>,
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
}

impl Default for Options {
    fn default() -> Options {
        Options {
            width: 336,
            height: 480,
            camera: None,
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
    let mut pages = Vec::new();
    for p in &atlas.pages {
        let png = input
            .pages
            .iter()
            .find(|(n, _)| n == &p.name)
            .ok_or_else(|| format!("{}: page {} not provided", input.name, p.name))?;
        let img = image::decode_png(&png.1).map_err(|e| format!("{}: {e}", p.name))?;
        // Straight alpha in the source atlases; work premultiplied from here on.
        pages.push(if p.pma { img } else { img.premultiplied() });
    }
    Ok(Layer { name: input.name.clone(), data, atlas, pages })
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

fn observe(layer: &Layer, list: &DrawList, cam: Camera, opts: &Options, seen: &mut [Seen], tri_seen: &mut trim::TriSeen, weight: f32, regions: &[Option<usize>]) {
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
fn simulate(layer: &Layer, cam: Camera, opts: &Options) -> (Vec<Seen>, trim::TriSeen) {
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
            observe(layer, &list, cam, opts, &mut seen, &mut tri_seen, weight, &regions);
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
    if inputs.len() < 2 || std::env::var("BAL2D_KEEP_LAYERS").is_ok() {
        return compile_once(inputs, opts, interactions);
    }
    let probe = Options { bake: false, ..opts.clone() };
    let (pack, _) = compile_once(inputs, &probe, interactions.clone())?;
    let hidden = hidden::hidden_layers(&pack)?;
    if hidden.is_empty() {
        return compile_once(inputs, opts, interactions);
    }
    let kept: Vec<LayerInput> = inputs.iter().enumerate().filter(|(i, _)| !hidden.contains(i)).map(|(_, l)| l.clone()).collect();
    // The interactive layer's index shifts down past the dropped ones below it.
    let interactions = interactions.map(|mut i| {
        let below = hidden.iter().filter(|&&h| h < i.layer as usize).count();
        i.layer -= below as u8;
        i
    });
    let (pack, mut report) = compile_once(&kept, opts, interactions)?;
    report.hidden_layers = hidden.iter().map(|&i| inputs[i].name.clone()).collect();
    Ok((pack, report))
}

fn compile_once(inputs: &[LayerInput], opts: &Options, interactions: Option<Interactions>) -> Result<(Vec<u8>, Report), String> {
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
    let baked = if opts.bake { bake::bake(&mut layers, main, &inter, cam, opts) } else { None };
    if let Some(b) = &baked {
        let h = b.header;
        report.baked = Some((b.slots.len(), b.triangles, [h.x, h.y, h.width, h.height], b.after_intro));
    }

    // Observe, cull, and gather per-region scale requirements.
    struct RegionNeed {
        layer: usize,
        region: usize,
        density: f32,
        /// Visible crop in logical pixels (x0, y0, x1, y1).
        crop: [u32; 4],
    }
    let mut needs: Vec<RegionNeed> = Vec::new();
    for (li, layer) in layers.iter_mut().enumerate() {
        let (seen, tri_seen) = simulate(layer, cam, opts);
        let total = layer.data.attachments.len();
        let kept = cull(&mut layer.data, &seen);
        let (tris_trimmed, verts_trimmed) = if std::env::var("BAL2D_NO_TRIM").is_ok() { (0, 0) } else { trim::trim_meshes(&mut layer.data, &tri_seen) };
        let timelines_dropped = if no_prune { 0 } else { prune::dead_timelines(&mut layer.data, if li == main { &drag_bones } else { &[] }) };
        let mut density: BTreeMap<usize, (f32, [f32; 4])> = BTreeMap::new();
        let live: Vec<u32> = layer.data.skins.iter().flat_map(|s| s.attachments.iter().map(|e| e.2)).collect();
        for id in live {
            let a = &layer.data.attachments[id as usize];
            let path = match &a.kind {
                AttachmentKind::Region(r) => &r.path,
                AttachmentKind::Mesh(m) => &m.path,
                _ => continue,
            };
            if let Some(ri) = layer.atlas.find(path) {
                let e = density.entry(ri).or_insert((0.0, [f32::MAX, f32::MAX, f32::MIN, f32::MIN]));
                let sn = &seen[id as usize];
                e.0 = e.0.max(sn.density);
                e.1[0] = e.1[0].min(sn.bounds[0]);
                e.1[1] = e.1[1].min(sn.bounds[1]);
                e.1[2] = e.1[2].max(sn.bounds[2]);
                e.1[3] = e.1[3].max(sn.bounds[3]);
            }
        }
        let before: u64 = layer.atlas.regions.iter().map(|r| r.width as u64 * r.height as u64).sum();
        report.layers.push(LayerReport {
            name: layer.name.clone(),
            attachments: total,
            kept,
            regions_before: layer.atlas.regions.len(),
            regions_after: density.len(),
            texels_before: before,
            animations_dropped: dropped[li],
            timelines_dropped,
            triangles_trimmed: tris_trimmed,
            vertices_trimmed: verts_trimmed,
            ..Default::default()
        });
        for (ri, (d, b)) in density {
            let r = &layer.atlas.regions[ri];
            let (w, h) = (r.width as f32, r.height as f32);
            // Two texels of margin for bilinear filtering; empty bounds keep the whole region.
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
        let packed = pack_atlas::build(&layer.atlas, &layer.pages, &items, opts.max_page)?;
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
    let header = Header {
        screen_width: opts.width as u16,
        screen_height: opts.height as u16,
        camera_cx: cam.cx,
        camera_cy: cam.cy,
        camera_scale: cam.scale,
        layer_count: layers.len() as u8,
        page_format: opts.format,
    };
    let bytes = bal2d_pack::write(&header, &sections, 9);
    report.pack_bytes = bytes.len();
    Ok((bytes, report))
}

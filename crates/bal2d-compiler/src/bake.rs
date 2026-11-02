//! Static background baking.
//!
//! The bottom of a lobby (the room, the sky, furniture) usually never changes, yet it is most of
//! the pixels the watch draws every frame: layer upon layer of large textured meshes. The slots
//! at the bottom of the draw order that look the same in every animation the lobby plays, and
//! that no drag or physics can move, are composited here once, at full source resolution, over
//! the opaque black the watch clears to, and shipped as one opaque BGR565 image (`BAKE`). The
//! watch draws that image first and then only the rest; the baked slots lose their render
//! attachments, so their textures, vertices and animation data are gone too.
//!
//! The result is exact for any blend mode in the baked slots: they are the first things drawn,
//! over the same black, in the same order.

use bal2d_pack::{BakeHeader, Interactions};
use bal2d_render::DrawList;
use bal2d_spine::state::{AnimationState, AnimationStateData};
use bal2d_spine::*;

use super::{Camera, Layer, Options};
use crate::raster::{self, Framebuffer, Texture};

/// What one slot shows at one instant, quantised so float noise does not count as change:
/// screen positions to 1/64 px, UVs to 1/65536, colours to 1/1023.
#[derive(Clone, Debug, PartialEq)]
struct Sig {
    attachment: u32,
    page: u32,
    blend: u8,
    color: [i32; 4],
    dark: Option<[i32; 3]>,
    verts: Vec<i32>,
    uvs: Vec<i32>,
}

struct Tracker {
    /// Per slot: the first signature seen (`Some(None)`: no attachment), `None` before any sample.
    /// The first sample is a steady one (the idle loop).
    reference: Vec<Option<Option<Sig>>>,
    /// Changes in the steady state: idle, talk / pat / look clips over it, drags.
    dynamic: Vec<bool>,
    /// Changes while the intro plays (it runs once per load).
    intro_moves: Vec<bool>,
    /// Ever drawn inside the screen (slots that never are cannot affect a pixel).
    visible: Vec<bool>,
    order: Option<Vec<u16>>,
    /// First draw-order position that ever differs from the reference order, steady / intro.
    order_change: usize,
    intro_order_change: usize,
    /// What made each slot dynamic (debug: `BAL2D_DEBUG_BAKE`).
    why: Vec<Option<String>>,
    label: String,
    /// The draw list of the first sample (every static slot looks like this all the time).
    first: Option<DrawList>,
}

impl Tracker {
    fn new(d: &SkeletonData) -> Tracker {
        Tracker {
            reference: vec![None; d.slots.len()],
            dynamic: vec![false; d.slots.len()],
            intro_moves: vec![false; d.slots.len()],
            visible: vec![false; d.slots.len()],
            order: None,
            order_change: usize::MAX,
            intro_order_change: usize::MAX,
            why: vec![None; d.slots.len()],
            label: String::new(),
            first: None,
        }
    }

    fn observe(&mut self, d: &SkeletonData, sk: &Skeleton, list: &mut DrawList, cam: Camera, opts: &Options, intro: bool) {
        bal2d_render::build(d, sk, list);
        let (w2, h2) = (opts.width as f32 * 0.5, opts.height as f32 * 0.5);
        let q = |v: f32, k: f32| (v * k).round() as i32;
        let mut cur: Vec<Option<Sig>> = vec![None; d.slots.len()];
        for b in &list.batches {
            let v0 = b.first_vertex as usize * 2;
            let v1 = v0 + b.vertex_count as usize * 2;
            let verts = list.positions[v0..v1]
                .chunks_exact(2)
                .flat_map(|p| [q((p[0] - cam.cx) * cam.scale + w2, 64.0), q(h2 - (p[1] - cam.cy) * cam.scale, 64.0)])
                .collect::<Vec<i32>>();
            if !self.visible[b.slot as usize] {
                let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
                for v in verts.chunks_exact(2) {
                    (x0, y0, x1, y1) = (x0.min(v[0]), y0.min(v[1]), x1.max(v[0]), y1.max(v[1]));
                }
                let (w, h) = (opts.width as i32 * 64, opts.height as i32 * 64);
                self.visible[b.slot as usize] = x1 > 0 && y1 > 0 && x0 < w && y0 < h;
            }
            let uvs = list.uvs[v0..v1].iter().map(|u| q(*u, 65536.0)).collect();
            cur[b.slot as usize] = Some(Sig {
                attachment: b.attachment,
                page: b.page,
                blend: b.blend as u8,
                color: b.color.map(|c| q(c, 1023.0)),
                dark: b.dark.map(|c| c.map(|v| q(v, 1023.0))),
                verts,
                uvs,
            });
        }
        for (s, c) in cur.into_iter().enumerate() {
            match &self.reference[s] {
                None => self.reference[s] = Some(c),
                Some(r) => {
                    if *r != c {
                        if intro {
                            self.intro_moves[s] = true;
                        } else {
                            if !self.dynamic[s] {
                                self.why[s] = Some(self.label.clone());
                            }
                            self.dynamic[s] = true;
                        }
                    }
                }
            }
        }
        match &self.order {
            None => {
                self.order = Some(sk.draw_order.clone());
                self.first = Some(list.clone());
            }
            Some(o) => {
                if let Some(p) = o.iter().zip(&sk.draw_order).position(|(a, b)| a != b) {
                    if intro {
                        self.intro_order_change = self.intro_order_change.min(p);
                    } else {
                        self.order_change = self.order_change.min(p);
                    }
                }
            }
        }
    }
}

fn step(d: &SkeletonData, sk: &mut Skeleton, st: &mut AnimationState, dt: f32, force: Option<(usize, f32, f32)>) {
    st.update(d, dt);
    st.apply(d, sk);
    if let Some((b, x, y)) = force {
        sk.bones[b].x = x;
        sk.bones[b].y = y;
    }
    sk.update(dt);
    sk.update_world_transform(d, Physics::Update);
    st.output.clear();
}

/// Plays what the lobby plays, the way `bal2d_core::Lobby` plays it, recording what changes:
/// on the main layer the idle loop (the reference), every other clip over it, every drag held at
/// the corners of its range for a second (physics included), and the real start: the intro,
/// then the idle loop once the mix into it has finished. Other layers loop their idle.
fn track(layer: &Layer, main: bool, inter: &Interactions, cam: Camera, opts: &Options) -> Tracker {
    let d = &layer.data;
    let mut t = Tracker::new(d);
    let mut list = DrawList::default();
    let fresh = || {
        let mut sk = Skeleton::new(d);
        sk.update_world_transform(d, Physics::Update);
        let st = AnimationState::new(d, AnimationStateData { default_mix: inter.default_mix, mixes: vec![] });
        (sk, st)
    };
    let run = |t: &mut Tracker, list: &mut DrawList, sk: &mut Skeleton, st: &mut AnimationState, seconds: f32, label: &str| {
        let mut time = 0.0;
        let mut first = true;
        while time <= seconds + 1e-4 {
            step(d, sk, st, if first { 0.0 } else { opts.step }, None);
            first = false;
            t.label = format!("{label} at {time:.2}s");
            t.observe(d, sk, list, cam, opts, false);
            time += opts.step;
        }
    };
    if !main {
        let idle = super::prune::layer_idle(d);
        let (mut sk, mut st) = fresh();
        st.set_animation(d, 0, idle, true);
        let dur = idle.map(|i| d.animations[i].duration).unwrap_or(0.0);
        run(&mut t, &mut list, &mut sk, &mut st, dur + 1.0, "idle");
        return t;
    }
    let idle = d.find_animation(&inter.idle);
    let intro = d.find_animation(&inter.intro);
    // The idle loop first: its first frame is the reference.
    let (mut sk, mut st) = fresh();
    st.set_animation(d, 0, idle, true);
    let idle_dur = idle.map(|i| d.animations[i].duration).unwrap_or(0.0);
    run(&mut t, &mut list, &mut sk, &mut st, idle_dur + 1.0, "idle");
    for (ai, a) in d.animations.iter().enumerate() {
        if Some(ai) == idle || Some(ai) == intro || a.duration <= 0.0 {
            continue;
        }
        let (mut sk, mut st) = fresh();
        st.set_animation(d, 0, idle, true);
        st.set_animation(d, 1, Some(ai), false);
        run(&mut t, &mut list, &mut sk, &mut st, a.duration + inter.default_mix + 0.5, &a.name);
    }
    for drag in &inter.drags {
        let Some(b) = d.find_bone(&drag.bone) else { continue };
        let (x0, x1) = (drag.min_x.min(drag.max_x), drag.min_x.max(drag.max_x));
        let (y0, y1) = (drag.min_y.min(drag.max_y), drag.min_y.max(drag.max_y));
        for (x, y) in [(x0, y0), (x1, y1), (x0, y1), (x1, y0)] {
            let (mut sk, mut st) = fresh();
            st.set_animation(d, 0, idle, true);
            for k in 0..30 {
                step(d, &mut sk, &mut st, if k == 0 { 0.0 } else { 1.0 / 30.0 }, Some((b, x, y)));
                t.label = format!("drag {} to ({x:.0}, {y:.0})", drag.bone);
                t.observe(d, &sk, &mut list, cam, opts, false);
            }
        }
    }
    // The real start: intro, then idle. Steady once the idle entry plays unmixed.
    if intro.is_some() {
        let (mut sk, mut st) = fresh();
        st.set_animation(d, 0, intro, false);
        st.add_animation(d, 0, idle, true, 0.0);
        let total = intro.map(|i| d.animations[i].duration).unwrap_or(0.0) + inter.default_mix + 2.0;
        let mut time = 0.0;
        let mut first = true;
        while time <= total {
            step(d, &mut sk, &mut st, if first { 0.0 } else { opts.step }, None);
            first = false;
            let settled = st.current(0).is_some_and(|e| st.entry(e).animation == idle && st.entry(e).mixing_from.is_none());
            t.label = format!("start at {time:.2}s");
            t.observe(d, &sk, &mut list, cam, opts, !settled);
            time += opts.step;
        }
    }
    t
}

/// Whether the slot alone, as first seen, covers every screen pixel fully opaquely.
fn opaque_full(layer: &Layer, t: &Tracker, s: usize, cam: Camera, opts: &Options) -> bool {
    let Some(first) = &t.first else { return false };
    let Some(b) = first.batches.iter().find(|b| b.slot as usize == s) else { return false };
    if b.color[3] < 0.999 {
        return false;
    }
    // Cheap test first: its vertices must span the screen.
    let (w2, h2) = (opts.width as f32 * 0.5, opts.height as f32 * 0.5);
    let v0 = b.first_vertex as usize * 2;
    let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for q in first.positions[v0..v0 + b.vertex_count as usize * 2].chunks_exact(2) {
        let (x, y) = ((q[0] - cam.cx) * cam.scale + w2, h2 - (q[1] - cam.cy) * cam.scale);
        bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x), bb[3].max(y)];
    }
    if bb[0] > 0.0 || bb[1] > 0.0 || bb[2] < opts.width as f32 || bb[3] < opts.height as f32 {
        return false;
    }
    let mut list = first.clone();
    list.batches.retain(|x| x.slot as usize == s);
    let mut fb = Framebuffer::new(opts.width as usize, opts.height as usize, [0.0, 0.0, 0.0, 0.0]);
    let textures: Vec<Texture> = layer.pages.iter().map(|p| Texture::from_premultiplied_rgba8(p.width as usize, p.height as usize, &p.data)).collect();
    raster::draw(&mut fb, &list, &textures, raster::Camera { cx: cam.cx, cy: cam.cy, scale: cam.scale });
    let ok = fb.pixels.iter().all(|p| p[3] >= 0.999);
    if std::env::var("BAL2D_DEBUG_BAKE").is_ok() {
        let n = fb.pixels.iter().filter(|p| p[3] >= 0.999).count();
        let mut hist = [0usize; 5];
        for p in &fb.pixels {
            let k = if p[3] >= 0.999 { 0 } else if p[3] >= 0.99 { 1 } else if p[3] >= 0.9 { 2 } else if p[3] > 0.0 { 3 } else { 4 };
            hist[k] += 1;
        }
        let min = fb.pixels.iter().map(|p| p[3]).fold(1.0f32, f32::min);
        eprintln!(
            "  occluder candidate slot {s} {}: {:.1}% opaque; alpha >=.999/.99/.9/>0/0: {:?}, min {min:.4}",
            layer.data.slots[s].name,
            n as f64 * 100.0 / fb.pixels.len() as f64,
            hist
        );
    }
    ok
}

/// The baked image and what it replaced.
pub struct Bake {
    pub header: BakeHeader,
    /// BGR565 pixels, `width * height`, rows unpadded.
    pub pixels: Vec<u8>,
    /// (layer, slot) of every slot the image replaces (or hides).
    pub slots: Vec<(usize, usize)>,
    pub triangles: usize,
    /// The intro moves some of them: they stay in the skeletons for it and are left out once
    /// the idle loop has settled; otherwise they are removed.
    pub after_intro: bool,
}

/// Finds the static bottom of the scene, composites it and removes it from the skeletons.
/// `None` when nothing at the bottom is static.
pub fn bake(layers: &mut [Layer], main: usize, inter: &Interactions, cam: Camera, opts: &Options) -> Option<Bake> {
    let trackers: Vec<Tracker> = layers.iter().enumerate().map(|(li, l)| track(l, li == main, inter, cam, opts)).collect();
    // The global draw order (layer, slot, position in the layer's order).
    let mut global: Vec<(usize, usize, usize)> = Vec::new();
    for (li, t) in trackers.iter().enumerate() {
        let Some(order) = &t.order else { break };
        global.extend(order.iter().enumerate().map(|(p, &s)| (li, s as usize, p)));
    }
    let clipping = |li: usize, s: usize| {
        layers[li].data.skins.iter().any(|sk| {
            sk.attachments
                .iter()
                .any(|(slot, _, id)| *slot as usize == s && matches!(layers[li].data.attachments[*id as usize].kind, AttachmentKind::Clipping(_)))
        })
    };
    // The topmost slot that is static and covers the whole screen opaquely: nothing below it is
    // ever seen, dynamic or not, so the bake starts there and everything below is left out.
    let occluder = (0..global.len()).rev().find(|&g| {
        let (li, s, p) = global[g];
        let t = &trackers[li];
        if p >= t.order_change || !t.visible[s] || t.dynamic[s] || layers[li].data.slots[s].blend != BlendMode::Normal {
            return false;
        }
        // A clipping attachment below it could reach above it.
        if global[..g].iter().any(|&(l, x, _)| l == li && clipping(l, x)) {
            return false;
        }
        opaque_full(&layers[li], t, s, cam, opts)
    });
    let start = occluder.unwrap_or(0);
    // Below the occluder: hidden. From it: the longest static run.
    let mut slots: Vec<(usize, usize)> = global[..start].iter().map(|&(li, s, _)| (li, s)).collect();
    let hidden = slots.len();
    for &(li, s, p) in &global[start..] {
        let t = &trackers[li];
        // Clipping masks what follows it in the draw order: the run stops there.
        if p >= t.order_change || clipping(li, s) {
            break;
        }
        if !t.visible[s] {
            // Never on screen: it cannot change a pixel, static or not.
            continue;
        }
        if t.dynamic[s] {
            break;
        }
        slots.push((li, s));
    }
    let run = &slots[hidden..];
    if std::env::var("BAL2D_DEBUG_BAKE").is_ok() {
        for (li, t) in trackers.iter().enumerate() {
            let Some(order) = &t.order else { continue };
            eprintln!("layer {li}: draw order changes at {}", if t.order_change == usize::MAX { "never".into() } else { t.order_change.to_string() });
            // Every slot that covers a noticeable part of the screen, with its share of it.
            let screen = (opts.width * opts.height) as f32;
            let (mut total, mut stat) = (0.0f32, 0.0f32);
            for &s in order.iter() {
                let s = s as usize;
                let Some(Some(sig)) = &t.reference[s] else { continue };
                let v: Vec<[f32; 2]> = sig.verts.chunks_exact(2).map(|q| [q[0] as f32 / 64.0, q[1] as f32 / 64.0]).collect();
                let Some(list) = &t.first else { continue };
                let Some(b) = list.batches.iter().find(|b| b.slot as usize == s) else { continue };
                let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
                let mut area = 0.0f32;
                for tri in idx.chunks_exact(3) {
                    let poly = [v[tri[0] as usize], v[tri[1] as usize], v[tri[2] as usize]];
                    let c = super::clip_rect(&poly, [0.0, 0.0, opts.width as f32, opts.height as f32]);
                    for k in 0..c.len() {
                        let (a, bb) = (c[k], c[(k + 1) % c.len()]);
                        area += a[0] * bb[1] - bb[0] * a[1];
                    }
                }
                let share = area.abs() * 0.5 / screen;
                total += share;
                if !t.dynamic[s] {
                    stat += share;
                }
                if share >= 0.05 {
                    eprintln!(
                        "  slot {s:4} {:28} {:5.2} screens {:?} {}{}",
                        layers[li].data.slots[s].name,
                        share,
                        layers[li].data.slots[s].blend,
                        t.why[s].clone().unwrap_or_else(|| "static".into()),
                        if t.intro_moves[s] { " (intro moves it)" } else { "" }
                    );
                }
            }
            eprintln!("  total {total:.2} screens of triangles, {stat:.2} of them static");
        }
    }
    // Composite the run over opaque black at full source resolution.
    let mut fb = Framebuffer::new(opts.width as usize, opts.height as usize, [0.0, 0.0, 0.0, 1.0]);
    let rc = raster::Camera { cx: cam.cx, cy: cam.cy, scale: cam.scale };
    let (w2, h2) = (opts.width as f32 * 0.5, opts.height as f32 * 0.5);
    let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    let mut triangles = 0;
    for (li, layer) in layers.iter().enumerate() {
        let Some(first) = &trackers[li].first else { continue };
        let mut list = first.clone();
        list.batches.retain(|b| run.contains(&(li, b.slot as usize)));
        if list.batches.is_empty() {
            continue;
        }
        for b in &list.batches {
            triangles += b.index_count as usize / 3;
            let v0 = b.first_vertex as usize * 2;
            for p in list.positions[v0..v0 + b.vertex_count as usize * 2].chunks_exact(2) {
                let (x, y) = ((p[0] - cam.cx) * cam.scale + w2, h2 - (p[1] - cam.cy) * cam.scale);
                bbox = [bbox[0].min(x), bbox[1].min(y), bbox[2].max(x), bbox[3].max(y)];
            }
        }
        let textures: Vec<Texture> =
            layer.pages.iter().map(|p| Texture::from_premultiplied_rgba8(p.width as usize, p.height as usize, &p.data)).collect();
        raster::draw(&mut fb, &list, &textures, rc);
    }
    if triangles == 0 {
        return None;
    }
    let mut x0 = (bbox[0].floor().max(0.0) as u32).min(opts.width);
    let y0 = (bbox[1].floor().max(0.0) as u32).min(opts.height);
    let mut x1 = (bbox[2].ceil().max(0.0) as u32).min(opts.width);
    let y1 = (bbox[3].ceil().max(0.0) as u32).min(opts.height);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    // Rows of a multiple of 16 pixels: the GPU samples the image in place.
    x0 &= !15;
    x1 = (x0 + (x1 - x0).div_ceil(16) * 16).min(opts.width & !15);
    if x1 <= x0 {
        return None;
    }
    // BGR565 with 4x4 ordered dithering: the watch's target is 16-bit, and a smooth sky or a
    // blurred room bands badly when every pixel rounds the same way.
    const BAYER: [[f32; 4]; 4] = [[0.0, 8.0, 2.0, 10.0], [12.0, 4.0, 14.0, 6.0], [3.0, 11.0, 1.0, 9.0], [15.0, 7.0, 13.0, 5.0]];
    let mut pixels = Vec::with_capacity(((x1 - x0) * (y1 - y0) * 2) as usize);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = fb.pixels[y as usize * fb.width + x as usize];
            let d = (BAYER[(y & 3) as usize][(x & 3) as usize] + 0.5) / 16.0 - 0.5;
            let ch = |v: f32, max: f32| ((v.clamp(0.0, 1.0) * max + d).round()).clamp(0.0, max) as u16;
            let v = (ch(p[0], 31.0) << 11) | (ch(p[1], 63.0) << 5) | ch(p[2], 31.0);
            pixels.extend_from_slice(&v.to_le_bytes());
        }
    }
    // Does the intro move what the image shows (or reorder it)? The slots under an occluder the
    // intro leaves alone stay hidden through the intro too.
    let after_intro = run.iter().any(|&(li, s)| trackers[li].intro_moves[s])
        || trackers.iter().enumerate().any(|(li, t)| {
            let last = slots.iter().filter(|x| x.0 == li).count();
            t.intro_order_change < last
        });
    let slots = slots.clone();
    // Without that, the baked slots leave the skeletons: no render attachment, no timelines.
    for (li, layer) in layers.iter_mut().enumerate() {
        if after_intro {
            break;
        }
        let gone: Vec<bool> = (0..layer.data.slots.len()).map(|s| slots.contains(&(li, s))).collect();
        if !gone.iter().any(|g| *g) {
            continue;
        }
        let d = &mut layer.data;
        let render: Vec<bool> = d.attachments.iter().map(|a| matches!(a.kind, AttachmentKind::Region(_) | AttachmentKind::Mesh(_))).collect();
        for skin in &mut d.skins {
            skin.attachments.retain(|(slot, _, id)| !(gone[*slot as usize] && render[*id as usize]));
        }
        for a in &mut d.animations {
            a.timelines.retain(|t| match t {
                Timeline::Attachment { slot, .. } | Timeline::SlotColor { slot, .. } => !gone[*slot as usize],
                _ => true,
            });
        }
    }
    let header = BakeHeader { x: x0 as u16, y: y0 as u16, width: (x1 - x0) as u16, height: (y1 - y0) as u16 };
    Some(Bake { header, pixels, slots, triangles, after_intro })
}

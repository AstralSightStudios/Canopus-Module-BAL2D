//! Baked idle: each layer's idle loop played here and stored as the watch's GPU calls
//! (`bal2d_pack::IdleBake`, drawn by `bal2d_core::idle`).
//!
//! The finished pack is loaded into the same lobby the watch runs and played into its idle
//! loop. Each loop is sampled `rate` times a second; between two samples the watch interpolates
//! every call's matrix, so the triangles of a call must fit one affine at both ends and at the
//! midpoint (seed-triangle affines, as the watch's own grouping; the midpoint takes their
//! average). An attachment's groups carry over from one interval to the next while they still
//! fit (a group that does not is regrouped on its own), so most calls keep their shape and their
//! matrices change little: the stream stores them as deltas.

use std::collections::HashMap;

use bal2d_core::Lobby;
use bal2d_pack::{IdleBake, IdleCall, IdleShape};
use bal2d_render::{DrawList, Tri};

/// Tolerance of the baked grouping (target pixels), less a margin for the fixed-point matrices.
pub const TOLERANCE: f32 = 1.0;
const QUANT_MARGIN: f32 = 0.06;
/// Samples per second, and the most frames one layer's loop gets (longer loops are sampled
/// less often). At `HOLD_RATE` and above, groups need only fit their own frame: the watch
/// interpolates towards the next frame where the same group carries on and holds the frame
/// where it does not (a thirtieth of a second); below it, they must fit the whole interval.
pub const RATE: f32 = 30.0;
pub const MAX_FRAMES: f32 = 600.0;
pub const HOLD_RATE: f32 = 12.0;
/// Most bytes the bakes may take unpacked: the watch holds them in its heap next to the pages
/// (Neru's 2.6 MB at 30 frames a second never loaded and its idle was drawn live; Hoshino's
/// 1.45 MB does). Above it the rate is halved, as for a pack over its size.
pub const MAX_RAW_BYTES: usize = 1_600_000;
/// Groups a carried-over grouping may have beyond a fresh one before it is redone.
const SLACK: usize = 0;
/// Longest the lobby is played looking for the idle loop (seconds).
const SETTLE_S: f32 = 120.0;

/// What a bake came to, per layer: (frames, calls per frame, shapes, encoded bytes).
#[derive(Clone, Debug, Default)]
pub struct IdleReport {
    pub layers: Vec<(u16, f32, usize, usize)>,
    /// Why there is no bake (empty when there is one).
    pub skipped: String,
    /// The most frames a second it was baked at (lowered to keep the pack within its size).
    pub rate: f32,
    /// What the fill pass left out (`crate::cull`).
    pub cull: crate::cull::CullReport,
    /// Slots of the main layer the talks leave alone (`crate::steady`).
    pub steady: usize,
}

/// One attachment's batch in a pose.
struct Batch {
    key: (u16, u32),
    page: u32,
    color: u32,
    screen: Vec<[f32; 2]>,
    texel: Vec<[f32; 2]>,
    tris: Vec<[u16; 3]>,
    /// Clipped: its geometry is not the attachment's own, so it is drawn live.
    live: bool,
}

type Pose = Vec<Batch>;

fn color(c: [f32; 4]) -> u32 {
    let q = |v: f32| ((v.clamp(0.0, 1.0) * 255.0) + 0.5) as u32;
    q(c[0]) | q(c[1]) << 8 | q(c[2]) << 16 | q(c[3]) << 24
}

/// Layer `li`'s batches now, through the idle framing (clipped ones marked live).
fn capture(lobby: &Lobby, li: usize, sizes: &[(f32, f32)], list: &mut DrawList) -> Option<Pose> {
    let c = lobby.cameras[bal2d_pack::phase::IDLE as usize];
    let (w2, h2) = (lobby.width * 0.5, lobby.height * 0.5);
    lobby.draw_list(li, list);
    let data = &lobby.layers[li].data;
    let mut out = Vec::with_capacity(list.batches.len());
    for b in &list.batches {
        let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
        let own = bal2d_core::idle::geometry(data, b.attachment).is_some_and(|(_, tris, verts)| idx == tris && b.vertex_count as usize == verts);
        if !own {
            out.push(Batch { key: (b.slot, b.attachment), page: b.page, color: 0, screen: Vec::new(), texel: Vec::new(), tris: Vec::new(), live: true });
            continue;
        }
        let &(pw, ph) = sizes.get(b.page as usize)?;
        let v0 = b.first_vertex as usize;
        let n = b.vertex_count as usize;
        out.push(Batch {
            key: (b.slot, b.attachment),
            page: b.page,
            color: color(b.color),
            screen: (0..n).map(|i| [(list.positions[(v0 + i) * 2] - c.cx) * c.scale + w2, h2 - (list.positions[(v0 + i) * 2 + 1] - c.cy) * c.scale]).collect(),
            texel: (0..n).map(|i| [list.uvs[(v0 + i) * 2] * pw, list.uvs[(v0 + i) * 2 + 1] * ph]).collect(),
            tris: idx.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect(),
            live: false,
        });
    }
    Some(out)
}

/// An interval's three poses of one batch: start, midpoint, end.
struct Span<'a> {
    b: &'a Batch,
    mid: &'a [[f32; 2]],
    end: &'a [[f32; 2]],
}

impl Span<'_> {
    fn tri(&self, t: u16, s: &[[f32; 2]]) -> Tri {
        let v = self.b.tris[t as usize];
        Tri { s: v.map(|k| s[k as usize]), t: v.map(|k| self.b.texel[k as usize]), v: [0; 3] }
    }

    /// The seed's affines at the start and the end.
    fn affines(&self, seed: u16) -> Option<([f32; 6], [f32; 6])> {
        Some((self.tri(seed, &self.b.screen).affine()?, self.tri(seed, self.end).affine()?))
    }

    fn fits(&self, t: u16, a: &([f32; 6], [f32; 6]), tol: f32) -> bool {
        let mid: [f32; 6] = core::array::from_fn(|k| (a.0[k] + a.1[k]) * 0.5);
        self.tri(t, &self.b.screen).fits(&a.0, tol) && self.tri(t, self.end).fits(&a.1, tol) && self.tri(t, self.mid).fits(&mid, tol)
    }

    /// Whether triangle `t` reaches the target at any of the three poses.
    fn visible(&self, t: u16, w: f32, h: f32) -> bool {
        [&self.b.screen[..], self.mid, self.end].iter().any(|s| {
            let v = self.b.tris[t as usize].map(|k| s[k as usize]);
            let (x0, x1) = (v[0][0].min(v[1][0]).min(v[2][0]), v[0][0].max(v[1][0]).max(v[2][0]));
            let (y0, y1) = (v[0][1].min(v[1][1]).min(v[2][1]), v[0][1].max(v[1][1]).max(v[2][1]));
            x1 >= -2.0 && y1 >= -2.0 && x0 <= w + 2.0 && y0 <= h + 2.0
        })
    }

    /// The least-squares texel -> screen affine of the members' corners in pose `s`.
    fn ls(&self, members: &[u16], s: &[[f32; 2]]) -> Option<[f32; 6]> {
        let n = members.len() as f64 * 3.0;
        let (mut mu, mut mv) = (0f64, 0f64);
        for &t in members {
            for k in self.b.tris[t as usize] {
                mu += self.b.texel[k as usize][0] as f64;
                mv += self.b.texel[k as usize][1] as f64;
            }
        }
        mu /= n;
        mv /= n;
        let (mut suu, mut suv, mut svv, mut sux, mut svx, mut suy, mut svy, mut sx, mut sy) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
        for &t in members {
            for k in self.b.tris[t as usize] {
                let (u, v) = (self.b.texel[k as usize][0] as f64 - mu, self.b.texel[k as usize][1] as f64 - mv);
                let (x, y) = (s[k as usize][0] as f64, s[k as usize][1] as f64);
                suu += u * u;
                suv += u * v;
                svv += v * v;
                sux += u * x;
                svx += v * x;
                suy += u * y;
                svy += v * y;
                sx += x;
                sy += y;
            }
        }
        let det = suu * svv - suv * suv;
        if !(det > 1e-9 * suu * svv) || det == 0.0 {
            return None;
        }
        let a = (sux * svv - svx * suv) / det;
        let b = (svx * suu - sux * suv) / det;
        let d = (suy * svv - svy * suv) / det;
        let e = (svy * suu - suy * suv) / det;
        Some([a as f32, b as f32, (sx / n - a * mu - b * mv) as f32, d as f32, e as f32, (sy / n - d * mu - e * mv) as f32])
    }

    /// The members' least-squares affines at the start and the end.
    fn fitted(&self, members: &[u16]) -> Option<([f32; 6], [f32; 6])> {
        Some((self.ls(members, &self.b.screen)?, self.ls(members, self.end)?))
    }

    /// The affines `members` keep as a group, if one pair draws them all within `tol`: their
    /// least-squares fit, else their first triangle's.
    fn keeps(&self, members: &[u16], tol: f32) -> Option<([f32; 6], [f32; 6])> {
        let ok = |a: &([f32; 6], [f32; 6])| members.iter().all(|&t| self.fits(t, a, tol));
        self.fitted(members).filter(ok).or_else(|| self.affines(members[0]).filter(ok))
    }

    /// Groups `members`: the first left seeds a group of everything its affines draw within
    /// `tol` over the interval; then the group's least-squares affines are tried, and taken
    /// while they gather more of what is left.
    fn group(&self, members: &[u16], tol: f32) -> Vec<(Vec<u16>, ([f32; 6], [f32; 6]))> {
        let mut pending: Vec<u16> = members.to_vec();
        let mut out = Vec::new();
        while let Some(&seed) = pending.first() {
            let Some(mut a) = self.affines(seed) else {
                pending.remove(0);
                continue;
            };
            let mut g: Vec<u16> = pending.iter().copied().filter(|&t| t == seed || self.fits(t, &a, tol)).collect();
            for _ in 0..3 {
                let Some(a2) = self.fitted(&g) else { break };
                let g2: Vec<u16> = pending.iter().copied().filter(|&t| self.fits(t, &a2, tol)).collect();
                if g2.len() <= g.len() {
                    break;
                }
                g = g2;
                a = a2;
            }
            let mut taken = vec![false; self.b.tris.len()];
            for &t in &g {
                taken[t as usize] = true;
            }
            pending.retain(|&t| !taken[t as usize]);
            out.push((g, a));
        }
        out
    }
}

/// Bakes every layer's idle loop of the pack `pack` at up to `max_rate` frames a second, groups
/// within `tolerance` target pixels; empty
/// (with the reason) when the lobby does not settle into idle loops that can be baked.
pub fn bake(pack: &[u8], max_rate: f32, tolerance: f32) -> (Vec<(u16, IdleBake)>, IdleReport) {
    let mut report = IdleReport::default();
    let Ok((mut lobby, pages)) = Lobby::load(pack) else {
        report.skipped = "pack does not load".into();
        return (Vec::new(), report);
    };
    let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
    for p in &pages {
        if let Some(s) = sizes.get_mut(p.layer as usize) {
            s.push((p.header.width as f32, p.header.height as f32));
        }
    }
    // Into the idle loops (the bake must not be in the way of the readiness test).
    let idle = std::mem::take(&mut lobby.idle);
    lobby.idle = lobby.layers.iter().map(|_| None).collect();
    let settled = |l: &Lobby| -> Option<Vec<(usize, f32)>> {
        let mut v = Vec::new();
        for layer in &l.layers {
            let e = layer.state.entry(layer.state.current(0)?);
            let a = e.animation?;
            if !e.looped || e.mixing_from.is_some() || e.next.is_some() {
                return None;
            }
            let period = e.animation_end - e.animation_start;
            if !(period > 0.05) {
                return None;
            }
            v.push((a, period));
        }
        Some(v)
    };
    let mut t = 0.0;
    let loops = loop {
        if lobby.bake.is_none() || lobby.bake_active() {
            if let Some(v) = settled(&lobby) {
                if (1..4).all(|tr| lobby.layers.iter().all(|l| l.state.current(tr).is_none_or(|e| l.state.entry(e).animation.is_none() && l.state.entry(e).mixing_from.is_none())))
                {
                    break v;
                }
            }
        }
        if t > SETTLE_S {
            report.skipped = "no settled idle loop".into();
            return (Vec::new(), report);
        }
        lobby.update(1.0 / 30.0);
        t += 1.0 / 30.0;
    };
    drop(idle);
    let (w, h) = (lobby.width, lobby.height);
    let env = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let tol = env("BAL2D_IDLE_TOL", tolerance) - QUANT_MARGIN;
    let rate_max = env("BAL2D_IDLE_RATE", max_rate);
    let mut list = DrawList::default();
    let mut out = Vec::new();
    for (li, &(animation, period)) in loops.iter().enumerate() {
        let frames = (period * rate_max).min(MAX_FRAMES).round().max(1.0) as usize;
        let rate = frames as f32 / period;
        let hold = rate >= HOLD_RATE;
        // One loop to settle (physics), then sample the next.
        let mut s = 0.0;
        while s < period {
            lobby.update(1.0 / 30.0);
            s += 1.0 / 30.0;
        }
        let entry = {
            let l = &lobby.layers[li];
            l.state.entry(l.state.current(0).unwrap()).track_time
        };
        let start = bal2d_core::idle::wrap(entry, period);
        let half = 0.5 / rate;
        let Some(mut a) = capture(&lobby, li, &sizes[li], &mut list) else {
            report.skipped = format!("layer {li}: clipping in the idle loop");
            return (Vec::new(), report);
        };
        let mut shapes: Vec<IdleShape> = Vec::new();
        let mut shape_ids: HashMap<((u16, u32), Vec<u16>), u16> = HashMap::new();
        let mut colors: Vec<u32> = Vec::new();
        let mut color_ids: HashMap<u32, u16> = HashMap::new();
        let mut prev: HashMap<(u16, u32), Vec<Vec<u16>>> = HashMap::new();
        let mut calls: Vec<Vec<IdleCall>> = Vec::with_capacity(frames);
        let mut total_calls = 0usize;
        let mut live: Vec<Vec<u16>> = Vec::new();
        let mut live_ids: HashMap<u16, u16> = HashMap::new();
        let mut current_run = 0usize;
        let mut mergeable = 0usize;
        for _ in 0..frames {
            lobby.update(half);
            let mid = capture(&lobby, li, &sizes[li], &mut list);
            lobby.update(half);
            let end = capture(&lobby, li, &sizes[li], &mut list);
            let (Some(mid), Some(end)) = (mid, end) else {
                report.skipped = format!("layer {li}: clipping in the idle loop");
                return (Vec::new(), report);
            };
            let mid_of: HashMap<(u16, u32), usize> = mid.iter().enumerate().map(|(i, b)| (b.key, i)).collect();
            let end_of: HashMap<(u16, u32), usize> = end.iter().enumerate().map(|(i, b)| (b.key, i)).collect();
            let mut frame = Vec::new();
            let mut last_call: Option<(u32, u8, u32, ([f32; 6], [f32; 6]))> = None;
            let mut next_prev = HashMap::new();
            let mut in_run = false;
            for b in &a {
                // Clipped slots: one marker per run, the run drawn live from the clipping slot on.
                if b.live {
                    if !in_run {
                        let sk = &lobby.layers[li].skeleton;
                        let data = &lobby.layers[li].data;
                        let pos = sk.draw_order.iter().position(|&s| s == b.key.0).unwrap_or(0);
                        let clip = sk.draw_order[..pos].iter().rev().copied().find(|&s| {
                            sk.slots[s as usize].attachment.is_some_and(|a| matches!(data.attachments[a as usize].kind, bal2d_spine::AttachmentKind::Clipping(_)))
                        });
                        let first = clip.unwrap_or(b.key.0);
                        let id = *live_ids.entry(first).or_insert_with(|| {
                            live.push(vec![first]);
                            (live.len() - 1) as u16
                        });
                        frame.push(IdleCall { shape: bal2d_pack::IDLE_LIVE, color: id, m0: [0; 6], m1: None });
                        current_run = id as usize;
                        in_run = true;
                    }
                    if !live[current_run].contains(&b.key.0) {
                        live[current_run].push(b.key.0);
                    }
                    last_call = None;
                    continue;
                }
                in_run = false;
                // An attachment that is gone by the end of the interval holds still until then.
                fn same<'p>(p: &'p Pose, i: Option<&usize>, n: usize) -> Option<&'p [[f32; 2]]> {
                    i.map(|&i| &p[i].screen[..]).filter(|s| s.len() == n)
                }
                let n = b.screen.len();
                let span = if hold {
                    Span { b, mid: &b.screen, end: &b.screen }
                } else {
                    Span { b, mid: same(&mid, mid_of.get(&b.key), n).unwrap_or(&b.screen), end: same(&end, end_of.get(&b.key), n).unwrap_or(&b.screen) }
                };
                let visible: Vec<u16> = (0..b.tris.len() as u16).filter(|&t| span.visible(t, w, h)).collect();
                if visible.is_empty() {
                    continue;
                }
                let fresh = span.group(&visible, tol);
                let groups = match prev.get(&b.key) {
                    Some(old) => {
                        let mut on = vec![false; b.tris.len()];
                        for &t in &visible {
                            on[t as usize] = true;
                        }
                        let mut covered = vec![false; b.tris.len()];
                        let mut kept: Vec<(Vec<u16>, ([f32; 6], [f32; 6]))> = Vec::new();
                        for g in old {
                            let g: Vec<u16> = g.iter().copied().filter(|&t| on[t as usize]).collect();
                            if g.is_empty() {
                                continue;
                            }
                            for &t in &g {
                                covered[t as usize] = true;
                            }
                            match span.keeps(&g, tol) {
                                Some(af) => kept.push((g, af)),
                                None => kept.extend(span.group(&g, tol)),
                            }
                        }
                        let new: Vec<u16> = visible.iter().copied().filter(|&t| !covered[t as usize]).collect();
                        kept.extend(span.group(&new, tol));
                        if kept.len() > fresh.len() + SLACK { fresh } else { kept }
                    }
                    None => fresh,
                };
                let ci = *color_ids.entry(b.color).or_insert_with(|| {
                    colors.push(b.color);
                    (colors.len() - 1) as u16
                });
                let blend = lobby.layers[li].data.slots[b.key.0 as usize].blend as u8;
                if let Some((p, bl, c, af)) = last_call {
                    if p == b.page && bl == blend && c == b.color && groups.iter().any(|(g, _)| g.iter().all(|&t| span.fits(t, &af, tol))) {
                        mergeable += 1;
                    }
                }
                last_call = groups.last().map(|g| (b.page, blend, b.color, g.1));
                for (g, af) in &groups {
                    let id = match shape_ids.get(&(b.key, g.clone())) {
                        Some(&id) => id,
                        None => {
                            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
                            for &t in g {
                                for k in b.tris[t as usize] {
                                    let p = b.texel[k as usize];
                                    lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                                    hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                                }
                            }
                            let anchor = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
                            let span = [((hi[0] - lo[0]) * 0.5).max(1.0), ((hi[1] - lo[1]) * 0.5).max(1.0)];
                            shapes.push(IdleShape { slot: b.key.0, attachment: b.key.1, anchor, span, tris: g.clone(), clip: 0 });
                            let id = (shapes.len() - 1) as u16;
                            shape_ids.insert((b.key, g.clone()), id);
                            id
                        }
                    };
                    let s = &shapes[id as usize];
                    let m1 = (!hold).then(|| IdleBake::params(s, &af.1));
                    frame.push(IdleCall { shape: id, color: ci, m0: IdleBake::params(s, &af.0), m1 });
                }
                total_calls += groups.len();
                next_prev.insert(b.key, groups.into_iter().map(|g| g.0).collect::<Vec<_>>());
            }
            prev = next_prev;
            calls.push(frame);
            a = end;
            if shapes.len() > u16::MAX as usize - 1024 {
                report.skipped = format!("layer {li}: too many shapes");
                return (Vec::new(), report);
            }
        }
        // An end matrix the next frame's call of the same shape starts with is not stored.
        for f in 0..frames.saturating_sub(1) {
            let starts: HashMap<u16, [i16; 6]> = calls[f + 1].iter().map(|c| (c.shape, c.m0)).collect();
            for c in &mut calls[f] {
                if c.m1.is_some() && starts.get(&c.shape) == c.m1.as_ref() {
                    c.m1 = None;
                }
            }
        }
        if std::env::var("BAL2D_IDLE_STATS").is_ok() {
            eprintln!("layer {li}: {:.0} calls/frame, {:.1} could join the call before (same page, blend, tint, affine)", total_calls as f32 / frames as f32, mergeable as f32 / frames as f32);
        }
        let stream = IdleBake::encode_frames(&calls, shapes.len());
        let bake = IdleBake { animation: animation as u16, rate, period, start, shapes, meshes: Vec::new(), regions: Default::default(), steady: Vec::new(), colors, live, frames: frames as u16, stream: vec![stream] };
        let bytes = bake.encode().len();
        report.layers.push((frames as u16, total_calls as f32 / frames as f32, bake.shapes.len(), bytes));
        out.push((li as u16, bake));
    }
    // Slots the talks leave alone stay on the main layer's bake during a talk.
    if std::env::var("BAL2D_NO_STEADY").is_err() {
        if let Some((main, steady)) = crate::steady::steady_slots(pack) {
            if let Some((_, b)) = out.iter_mut().find(|(li, _)| *li as usize == main) {
                let mut bits = vec![0u8; steady.len().div_ceil(8)];
                for (i, &k) in steady.iter().enumerate() {
                    if k {
                        bits[i / 8] |= 1 << (i % 8);
                    }
                }
                report.steady = steady.iter().filter(|&&k| k).count();
                b.steady = bits;
            }
        }
    }
    if std::env::var("BAL2D_NO_CULL").is_err() {
        let mut textures: Vec<Vec<crate::raster::Texture>> = (0..lobby.layers.len()).map(|_| Vec::new()).collect();
        for p in &pages {
            let mut section = vec![0u8; p.section_len()];
            if bal2d_core::read_page(&mut &pack[..], p, &mut section).is_err() {
                return (out, report);
            }
            let img = crate::image::decode_page(p.header.width as u32, p.header.height as u32, p.header.format, &section[8..]);
            if let Some(l) = textures.get_mut(p.layer as usize) {
                l.push(crate::raster::Texture::from_premultiplied_vec(img.width as usize, img.height as usize, img.data));
            }
        }
        report.cull = crate::cull::cull(&lobby, &textures, &mut out);
        for (i, (_, b)) in out.iter().enumerate() {
            if let Some(l) = report.layers.get_mut(i) {
                l.3 = b.encode().len();
            }
        }
    }
    (out, report)
}

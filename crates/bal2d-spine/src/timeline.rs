//! Applying timelines to a skeleton pose, with Spine's MixBlend / MixDirection semantics.

use alloc::vec::Vec;

use crate::data::*;
use crate::math::*;
use crate::skeleton::{Skeleton, set_slot_attachment};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixBlend {
    Setup,
    First,
    Replace,
    Add,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixDirection {
    In,
    Out,
}

/// An event fired while applying an animation: index into the animation's event timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FiredEvent {
    pub animation: u16,
    pub key: u16,
}

// ---- property ids (for AnimationState's timeline modes) ----

pub const PROP_ROTATE: u64 = 0;
pub const PROP_X: u64 = 1;
pub const PROP_Y: u64 = 2;
pub const PROP_SCALE_X: u64 = 3;
pub const PROP_SCALE_Y: u64 = 4;
pub const PROP_SHEAR_X: u64 = 5;
pub const PROP_SHEAR_Y: u64 = 6;
pub const PROP_INHERIT: u64 = 7;
pub const PROP_RGB: u64 = 8;
pub const PROP_ALPHA: u64 = 9;
pub const PROP_RGB2: u64 = 10;
pub const PROP_ATTACHMENT: u64 = 11;
pub const PROP_DEFORM: u64 = 12;
pub const PROP_EVENT: u64 = 13;
pub const PROP_DRAW_ORDER: u64 = 14;
pub const PROP_IK: u64 = 15;
pub const PROP_TRANSFORM: u64 = 16;
pub const PROP_PATH_POSITION: u64 = 17;
pub const PROP_PATH_SPACING: u64 = 18;
pub const PROP_PATH_MIX: u64 = 19;
pub const PROP_PHYSICS_INERTIA: u64 = 20;
pub const PROP_PHYSICS_STRENGTH: u64 = 21;
pub const PROP_PHYSICS_DAMPING: u64 = 22;
pub const PROP_PHYSICS_MASS: u64 = 23;
pub const PROP_PHYSICS_WIND: u64 = 24;
pub const PROP_PHYSICS_GRAVITY: u64 = 25;
pub const PROP_PHYSICS_MIX: u64 = 26;
pub const PROP_PHYSICS_RESET: u64 = 27;
pub const PROP_SEQUENCE: u64 = 28;

#[inline]
fn pid(prop: u64, a: u64, b: u64) -> u64 {
    (prop << 56) | ((a & 0xff_ffff) << 32) | (b & 0xffff_ffff)
}

/// Property ids a timeline touches (Spine's "property|index" strings, packed).
pub fn property_ids(t: &Timeline, out: &mut Vec<u64>) {
    match t {
        Timeline::Attachment { slot, .. } => out.push(pid(PROP_ATTACHMENT, *slot as u64, 0)),
        Timeline::SlotColor { slot, kind, .. } => {
            let s = *slot as u64;
            match kind {
                SlotColorKind::Rgba => {
                    out.push(pid(PROP_RGB, s, 0));
                    out.push(pid(PROP_ALPHA, s, 0));
                }
                SlotColorKind::Rgb => out.push(pid(PROP_RGB, s, 0)),
                SlotColorKind::Alpha => out.push(pid(PROP_ALPHA, s, 0)),
                SlotColorKind::Rgba2 => {
                    out.push(pid(PROP_RGB, s, 0));
                    out.push(pid(PROP_ALPHA, s, 0));
                    out.push(pid(PROP_RGB2, s, 0));
                }
                SlotColorKind::Rgb2 => {
                    out.push(pid(PROP_RGB, s, 0));
                    out.push(pid(PROP_RGB2, s, 0));
                }
            }
        }
        Timeline::Bone { bone, property, .. } => {
            let b = *bone as u64;
            let (p1, p2) = match property {
                BoneProperty::Rotate => (PROP_ROTATE, None),
                BoneProperty::Translate => (PROP_X, Some(PROP_Y)),
                BoneProperty::TranslateX => (PROP_X, None),
                BoneProperty::TranslateY => (PROP_Y, None),
                BoneProperty::Scale => (PROP_SCALE_X, Some(PROP_SCALE_Y)),
                BoneProperty::ScaleX => (PROP_SCALE_X, None),
                BoneProperty::ScaleY => (PROP_SCALE_Y, None),
                BoneProperty::Shear => (PROP_SHEAR_X, Some(PROP_SHEAR_Y)),
                BoneProperty::ShearX => (PROP_SHEAR_X, None),
                BoneProperty::ShearY => (PROP_SHEAR_Y, None),
            };
            out.push(pid(p1, b, 0));
            if let Some(p2) = p2 {
                out.push(pid(p2, b, 0));
            }
        }
        Timeline::Inherit { bone, .. } => out.push(pid(PROP_INHERIT, *bone as u64, 0)),
        Timeline::Ik { constraint, .. } => out.push(pid(PROP_IK, *constraint as u64, 0)),
        Timeline::Transform { constraint, .. } => out.push(pid(PROP_TRANSFORM, *constraint as u64, 0)),
        Timeline::Path { constraint, property, .. } => {
            let p = match property {
                PathProperty::Position => PROP_PATH_POSITION,
                PathProperty::Spacing => PROP_PATH_SPACING,
                PathProperty::Mix => PROP_PATH_MIX,
            };
            out.push(pid(p, *constraint as u64, 0));
        }
        Timeline::Physics { constraint, property, .. } => {
            let p = match property {
                PhysicsProperty::Inertia => PROP_PHYSICS_INERTIA,
                PhysicsProperty::Strength => PROP_PHYSICS_STRENGTH,
                PhysicsProperty::Damping => PROP_PHYSICS_DAMPING,
                PhysicsProperty::Mass => PROP_PHYSICS_MASS,
                PhysicsProperty::Wind => PROP_PHYSICS_WIND,
                PhysicsProperty::Gravity => PROP_PHYSICS_GRAVITY,
                PhysicsProperty::Mix => PROP_PHYSICS_MIX,
            };
            out.push(pid(p, constraint.map_or(0xff_ffff, |c| c as u64), 0));
        }
        Timeline::PhysicsReset { constraint, .. } => {
            out.push(pid(PROP_PHYSICS_RESET, constraint.map_or(0xff_ffff, |c| c as u64), 0))
        }
        Timeline::Deform { slot, attachment, .. } => out.push(pid(PROP_DEFORM, *slot as u64, *attachment as u64)),
        Timeline::Sequence { slot, attachment, .. } => out.push(pid(PROP_SEQUENCE, *slot as u64, *attachment as u64)),
        Timeline::DrawOrder { .. } => out.push(pid(PROP_DRAW_ORDER, 0, 0)),
        Timeline::Event { .. } => out.push(pid(PROP_EVENT, 0, 0)),
    }
}

// ---- frame search and curve evaluation ----

/// Index of the frame (in units of floats) at or before `time`, entries floats per frame: the
/// frame before the first one after `time` (frame times are sorted), else the last frame.
/// Long timelines are searched by bisection (the idle loops have hundreds of keys and every
/// timeline is searched every frame); the result is the linear scan's.
#[inline]
pub fn search(frames: &[f32], time: f32, step: usize) -> usize {
    let n = frames.len();
    let count = n / step;
    if count > 8 {
        // First frame k >= 1 with time(k) > time.
        let (mut lo, mut hi) = (1usize, count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if frames[mid * step] > time {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        return if lo < count { (lo - 1) * step } else { n - step };
    }
    let mut i = step;
    while i < n {
        if frames[i] > time {
            return i - step;
        }
        i += step;
    }
    n - step
}

#[inline]
pub fn search1(frames: &[f32], time: f32) -> usize {
    search(frames, time, 1)
}

impl Curve {
    pub fn frame_count(&self) -> usize {
        self.frames.len() / self.entries as usize
    }

    /// Control values (cx1, cy1, cx2, cy2) of value `v` (1-based offset within a frame; 0 for
    /// deform curves, which run from 0 to 1) of the Bezier frame at float index `i`, whose curve
    /// type is `t`: as stored, or rebuilt from the shared normalised easing.
    #[inline]
    pub fn controls(&self, t: f32, i: usize, v: usize) -> [f32; BEZIER_SIZE] {
        if t >= CURVE_BEZIER {
            let b = t as usize - 2 + v.saturating_sub(1) * BEZIER_SIZE;
            return [self.curves[b], self.curves[b + 1], self.curves[b + 2], self.curves[b + 3]];
        }
        let b = (-t) as usize - 2;
        let q = &self.curves[b..b + BEZIER_SIZE];
        let e = self.entries as usize;
        let (t1, t2) = (self.frames[i], self.frames[i + e]);
        let (v1, v2) = if v == 0 { (0.0, 1.0) } else { (self.frames[i + v], self.frames[i + e + v]) };
        let (dt, dv) = (t2 - t1, v2 - v1);
        [t1 + q[0] * dt, v1 + q[1] * dv, t1 + q[2] * dt, v1 + q[3] * dv]
    }

    /// The control values of channel `k` (0-based) of the Bezier between frame `frame` and the
    /// next (for writers).
    pub fn bezier_controls(&self, frame: usize, k: usize) -> [f32; BEZIER_SIZE] {
        let e = self.entries as usize;
        self.controls(self.curves[frame], frame * e, if e == 1 { 0 } else { k + 1 })
    }

    /// Bezier segments stored per channel, as a skeleton file counts them.
    pub fn bezier_count(&self) -> usize {
        let n = self.frame_count();
        (0..n.saturating_sub(1)).filter(|&k| self.curves[k] >= CURVE_BEZIER || self.curves[k] <= -CURVE_BEZIER).count() * self.channels as usize
    }

    /// Stores once, normalised, each Bezier frame whose channels follow one easing: Spine's
    /// linked curves (x and y of a translate, the channels of a colour) give every channel the
    /// same handles once its keys are mapped to 0..1, and most multi-channel keys are such (half
    /// the floats of a big lobby's curves). A channel that does not move (keys and handles at one
    /// value) fits any easing. Rebuilt control values are within float rounding of the stored
    /// ones ([`SHARE_TOLERANCE`] of their magnitude).
    pub fn share_easings(&mut self) {
        let (ch, e, n) = (self.channels as usize, self.entries as usize, self.frame_count());
        if ch < 2 || n < 2 || e < ch + 1 {
            self.curves.shrink_to_fit();
            return;
        }
        let mut out: Vec<f32> = Vec::with_capacity(self.curves.len());
        out.extend_from_slice(&self.curves[..n]);
        for k in 0..n - 1 {
            let t = self.curves[k];
            if t < CURVE_BEZIER {
                continue;
            }
            let base = t as usize - 2;
            match self.shared_easing(k * e, base, ch) {
                Some(q) => {
                    out[k] = -(CURVE_BEZIER + out.len() as f32);
                    out.extend_from_slice(&q);
                }
                None => {
                    out[k] = CURVE_BEZIER + out.len() as f32;
                    out.extend_from_slice(&self.curves[base..base + ch * BEZIER_SIZE]);
                }
            }
        }
        out.shrink_to_fit();
        self.curves = out;
    }

    /// The normalised easing every channel of the Bezier frame at float index `i` (channels from
    /// `curves[base]`) follows, if there is one.
    fn shared_easing(&self, i: usize, base: usize, ch: usize) -> Option<[f32; BEZIER_SIZE]> {
        let e = self.entries as usize;
        let f = &self.frames;
        let (t1, t2) = (f[i], f[i + e]);
        let dt = t2 - t1;
        if !(dt > 0.0) {
            return None;
        }
        // The channel that moves most gives the easing (the most precise once normalised).
        let mut reference = None;
        let mut most = 0.0f32;
        for v in 0..ch {
            let dv = (f[i + e + v + 1] - f[i + v + 1]).abs();
            if dv > most {
                most = dv;
                reference = Some(v);
            }
        }
        let c0 = &self.curves[base..base + BEZIER_SIZE];
        let q = match reference {
            Some(v) => {
                let c = &self.curves[base + v * BEZIER_SIZE..base + (v + 1) * BEZIER_SIZE];
                let (v1, v2) = (f[i + v + 1], f[i + e + v + 1]);
                [(c[0] - t1) / dt, (c[1] - v1) / (v2 - v1), (c[2] - t1) / dt, (c[3] - v1) / (v2 - v1)]
            }
            None => [(c0[0] - t1) / dt, 0.0, (c0[2] - t1) / dt, 0.0],
        };
        if !q.iter().all(|x| x.is_finite()) {
            return None;
        }
        let mt = 1.0f32.max(t1.abs()).max(t2.abs());
        for v in 0..ch {
            let c = &self.curves[base + v * BEZIER_SIZE..base + (v + 1) * BEZIER_SIZE];
            let (v1, v2) = (f[i + v + 1], f[i + e + v + 1]);
            if v1 == v2 && c[1] == v1 && c[3] == v1 {
                continue;
            }
            let dv = v2 - v1;
            let mv = 1.0f32.max(v1.abs()).max(v2.abs()).max(c[1].abs()).max(c[3].abs());
            let near = |a: f32, b: f32, m: f32| (a - b).abs() <= SHARE_TOLERANCE * m;
            if !(near(t1 + q[0] * dt, c[0], mt) && near(t1 + q[2] * dt, c[2], mt) && near(v1 + q[1] * dv, c[1], mv) && near(v1 + q[3] * dv, c[3], mv)) {
                return None;
            }
        }
        Some(q)
    }

    /// The sampled Bezier between the frame at float index `frame_index` and the next one, for
    /// value `value_offset` of the frame. Deform curves (`value_offset == 0`) run from 0 to 1.
    #[inline]
    pub fn samples(&self, frame_index: usize, value_offset: usize) -> [f32; BEZIER_SAMPLES] {
        let e = self.entries as usize;
        let c = self.controls(self.curves[frame_index / e], frame_index, value_offset);
        let f = &self.frames;
        let (v1, v2) = if value_offset == 0 { (0.0, 1.0) } else { (f[frame_index + value_offset], f[frame_index + e + value_offset]) };
        let mut s = [0f32; BEZIER_SAMPLES];
        crate::binary::sample_bezier(&mut s, f[frame_index], v1, c[0], c[1], c[2], c[3], f[frame_index + e], v2);
        s
    }

    pub fn bezier_value(&self, time: f32, frame_index: usize, value_offset: usize, c: [f32; BEZIER_SIZE]) -> f32 {
        let e = self.entries as usize;
        let frames = &self.frames;
        let (t1, v1) = (frames[frame_index], frames[frame_index + value_offset]);
        let (t2, v2) = (frames[frame_index + e], frames[frame_index + e + value_offset]);
        // The samples of `sample_bezier`, generated only up to the one past `time` (the same
        // values, in the same order: the result is unchanged).
        let mut b = Bezier::new(t1, v1, c[0], c[1], c[2], c[3], t2, v2);
        let (mut x, mut y) = (t1, v1);
        for k in 0..BEZIER_SAMPLES / 2 {
            let (sx, sy) = b.next();
            if if k == 0 { sx > time } else { sx >= time } {
                return y + (time - x) / (sx - x) * (sy - y);
            }
            x = sx;
            y = sy;
        }
        y + (time - x) / (t2 - x) * (v2 - y)
    }

    /// Evaluates value `v` (1-based offset within a frame) at `time`. `i` is the frame's float
    /// index from `search`.
    #[inline]
    pub fn value_at(&self, time: f32, i: usize, v: usize) -> f32 {
        if self.constant {
            return self.frames[v];
        }
        let e = self.entries as usize;
        let curve_type = self.curves[i / e];
        if curve_type == CURVE_LINEAR {
            let before = self.frames[i];
            let value = self.frames[i + v];
            let t = (time - before) / (self.frames[i + e] - before);
            value + (self.frames[i + e + v] - value) * t
        } else if curve_type == CURVE_STEPPED {
            self.frames[i + v]
        } else {
            self.bezier_value(time, i, v, self.controls(curve_type, i, v))
        }
    }

    /// Values 1..=`out.len()` of the frame at float index `i` at `time` (as [`Curve::value_at`]
    /// for each, the same floats). The channels of a key usually share one Bezier timing (only
    /// the values differ): then the sample that brackets `time` is found once for all of them.
    pub fn values_at(&self, time: f32, i: usize, out: &mut [f32]) {
        if self.constant {
            out.copy_from_slice(&self.frames[1..1 + out.len()]);
            return;
        }
        let e = self.entries as usize;
        let curve_type = self.curves[i / e];
        let shared = out.len() > 1
            && (curve_type <= -CURVE_BEZIER
                || curve_type >= CURVE_BEZIER && {
                    let base = curve_type as usize - 2;
                    let (cx1, cx2) = (self.curves[base].to_bits(), self.curves[base + 2].to_bits());
                    (1..out.len()).all(|v| self.curves[base + v * BEZIER_SIZE].to_bits() == cx1 && self.curves[base + v * BEZIER_SIZE + 2].to_bits() == cx2)
                });
        if !shared {
            for (v, o) in out.iter_mut().enumerate() {
                *o = self.value_at(time, i, v + 1);
            }
            return;
        }
        let frames = &self.frames;
        let (t1, t2) = (frames[i], frames[i + e]);
        let c = self.controls(curve_type, i, 1);
        // The time axis of `Bezier` (the same operations), up to the sample past `time`.
        let tmpx = (t1 - c[0] * 2.0 + c[2]) * 0.03;
        let dddx = ((c[0] - c[2]) * 3.0 - t1 + t2) * 0.006;
        let mut ddx = tmpx * 2.0 + dddx;
        let mut dx = (c[0] - t1) * 0.3 + tmpx + dddx * 0.166_666_67;
        let mut sx = t1 + dx;
        let mut x = t1;
        let mut steps = BEZIER_SAMPLES / 2;
        for k in 0..BEZIER_SAMPLES / 2 {
            if if k == 0 { sx > time } else { sx >= time } {
                steps = k;
                break;
            }
            x = sx;
            dx += ddx;
            ddx += dddx;
            sx += dx;
        }
        let end = steps == BEZIER_SAMPLES / 2;
        let q = if end { (time - x) / (t2 - x) } else { (time - x) / (sx - x) };
        for (v, o) in out.iter_mut().enumerate() {
            let c = self.controls(curve_type, i, v + 1);
            let (v1, v2) = (frames[i + v + 1], frames[i + e + v + 1]);
            // The value axis: sample `steps` (the one past `time`) and the one before it.
            let tmpy = (v1 - c[1] * 2.0 + c[3]) * 0.03;
            let dddy = ((c[1] - c[3]) * 3.0 - v1 + v2) * 0.006;
            let mut ddy = tmpy * 2.0 + dddy;
            let mut dy = (c[1] - v1) * 0.3 + tmpy + dddy * 0.166_666_67;
            let mut sy = v1 + dy;
            let mut y = v1;
            for _ in 0..steps {
                y = sy;
                dy += ddy;
                ddy += dddy;
                sy += dy;
            }
            *o = if end { y + q * (v2 - y) } else { y + q * (sy - y) };
        }
    }

    /// Single-value curve (entries == 2) at `time` (time >= first frame).
    pub fn curve_value(&self, time: f32) -> f32 {
        if self.constant {
            return self.frames[1];
        }
        let i = search(&self.frames, time, 2);
        self.value_at(time, i, 1)
    }

    /// The float index of the frame at or before `time` (`search`); 0 for a constant curve,
    /// whose frames all hold the same values.
    #[inline]
    pub fn index(&self, time: f32) -> usize {
        if self.constant { 0 } else { search(&self.frames, time, self.entries as usize) }
    }

    /// Interpolation percent between `frame` and the next (entries == 1 curves: deform).
    pub fn curve_percent(&self, time: f32, frame: usize) -> f32 {
        let curves = &self.curves;
        let i = curves[frame];
        if i == CURVE_LINEAR {
            let x = self.frames[frame];
            return (time - x) / (self.frames[frame + 1] - x);
        }
        if i == CURVE_STEPPED {
            return 0.0;
        }
        let c = self.controls(i, frame, 0);
        let (t1, t2) = (self.frames[frame], self.frames[frame + 1]);
        let mut b = Bezier::new(t1, 0.0, c[0], c[1], c[2], c[3], t2, 1.0);
        let (s0, s1) = b.next();
        if s0 > time {
            return s1 * (time - t1) / (s0 - t1);
        }
        let (mut x, mut y) = (s0, s1);
        for _ in 1..BEZIER_SAMPLES / 2 {
            let (sx, sy) = b.next();
            if sx >= time {
                return y + (time - x) / (sx - x) * (sy - y);
            }
            x = sx;
            y = sy;
        }
        y + (1.0 - y) * (time - x) / (t2 - x)
    }

    fn relative(&self, time: f32, alpha: f32, blend: MixBlend, current: f32, setup: f32) -> f32 {
        if time < self.frames[0] {
            return match blend {
                MixBlend::Setup => setup,
                MixBlend::First => current + (setup - current) * alpha,
                _ => current,
            };
        }
        let mut value = self.curve_value(time);
        match blend {
            MixBlend::Setup => return setup + value * alpha,
            MixBlend::First | MixBlend::Replace => value += setup - current,
            MixBlend::Add => {}
        }
        current + value * alpha
    }

    fn absolute(&self, time: f32, alpha: f32, blend: MixBlend, current: f32, setup: f32) -> f32 {
        if time < self.frames[0] {
            return match blend {
                MixBlend::Setup => setup,
                MixBlend::First => current + (setup - current) * alpha,
                _ => current,
            };
        }
        let value = self.curve_value(time);
        if blend == MixBlend::Setup {
            return setup + (value - setup) * alpha;
        }
        current + (value - current) * alpha
    }

    fn absolute2(&self, time: f32, alpha: f32, blend: MixBlend, current: f32, setup: f32, value: f32) -> f32 {
        if time < self.frames[0] {
            return match blend {
                MixBlend::Setup => setup,
                MixBlend::First => current + (setup - current) * alpha,
                _ => current,
            };
        }
        if blend == MixBlend::Setup {
            return setup + (value - setup) * alpha;
        }
        current + (value - current) * alpha
    }

    fn scale(&self, time: f32, alpha: f32, blend: MixBlend, direction: MixDirection, current: f32, setup: f32) -> f32 {
        if time < self.frames[0] {
            return match blend {
                MixBlend::Setup => setup,
                MixBlend::First => current + (setup - current) * alpha,
                _ => current,
            };
        }
        let value = self.curve_value(time) * setup;
        if alpha == 1.0 {
            if blend == MixBlend::Add {
                return current + value - setup;
            }
            return value;
        }
        if direction == MixDirection::Out {
            match blend {
                MixBlend::Setup => return setup + (abs(value) * signum(setup) - setup) * alpha,
                MixBlend::First | MixBlend::Replace => return current + (abs(value) * signum(current) - current) * alpha,
                MixBlend::Add => {}
            }
        } else {
            match blend {
                MixBlend::Setup => {
                    let s = abs(setup) * signum(value);
                    return s + (value - s) * alpha;
                }
                MixBlend::First | MixBlend::Replace => {
                    let s = abs(current) * signum(value);
                    return s + (value - s) * alpha;
                }
                MixBlend::Add => {}
            }
        }
        current + (value - setup) * alpha
    }
}

fn add_color(c: &mut Color, r: f32, g: f32, b: f32, a: f32) {
    c.r += r;
    c.g += g;
    c.b += b;
    c.a += a;
    c.clamp();
}

fn mix_color_to(c: &mut Color, target: Color, alpha: f32) {
    let d = *c;
    add_color(c, (target.r - d.r) * alpha, (target.g - d.g) * alpha, (target.b - d.b) * alpha, (target.a - d.a) * alpha);
}

fn set_color(c: &mut Color, r: f32, g: f32, b: f32, a: f32) {
    *c = Color { r, g, b, a };
    c.clamp();
}

/// Applies one timeline. `events` receives fired event keys (indices into the timeline's
/// `events`) when given.
#[allow(clippy::too_many_arguments)]
pub fn apply_timeline(
    t: &Timeline,
    data: &SkeletonData,
    sk: &mut Skeleton,
    last_time: f32,
    time: f32,
    events: Option<&mut Vec<u16>>,
    alpha: f32,
    blend: MixBlend,
    direction: MixDirection,
) {
    match t {
        Timeline::Bone { bone, property, curve } => {
            let bi = *bone as usize;
            if !sk.bones[bi].active {
                return;
            }
            let setup = &data.bones[bi];
            let b = &mut sk.bones[bi];
            match property {
                BoneProperty::Rotate => b.rotation = curve.relative(time, alpha, blend, b.rotation, setup.rotation),
                BoneProperty::TranslateX => b.x = curve.relative(time, alpha, blend, b.x, setup.x),
                BoneProperty::TranslateY => b.y = curve.relative(time, alpha, blend, b.y, setup.y),
                BoneProperty::ShearX => b.shear_x = curve.relative(time, alpha, blend, b.shear_x, setup.shear_x),
                BoneProperty::ShearY => b.shear_y = curve.relative(time, alpha, blend, b.shear_y, setup.shear_y),
                BoneProperty::ScaleX => b.scale_x = curve.scale(time, alpha, blend, direction, b.scale_x, setup.scale_x),
                BoneProperty::ScaleY => b.scale_y = curve.scale(time, alpha, blend, direction, b.scale_y, setup.scale_y),
                BoneProperty::Translate | BoneProperty::Shear => {
                    let (sx, sy) = if *property == BoneProperty::Translate { (setup.x, setup.y) } else { (setup.shear_x, setup.shear_y) };
                    let (cx, cy) = if *property == BoneProperty::Translate { (&mut b.x, &mut b.y) } else { (&mut b.shear_x, &mut b.shear_y) };
                    if time < curve.frames[0] {
                        match blend {
                            MixBlend::Setup => {
                                *cx = sx;
                                *cy = sy;
                            }
                            MixBlend::First => {
                                *cx += (sx - *cx) * alpha;
                                *cy += (sy - *cy) * alpha;
                            }
                            _ => {}
                        }
                        return;
                    }
                    let i = curve.index(time);
                    let mut xy = [0f32; 2];
                    curve.values_at(time, i, &mut xy);
                    let [x, y] = xy;
                    match blend {
                        MixBlend::Setup => {
                            *cx = sx + x * alpha;
                            *cy = sy + y * alpha;
                        }
                        MixBlend::First | MixBlend::Replace => {
                            *cx += (sx + x - *cx) * alpha;
                            *cy += (sy + y - *cy) * alpha;
                        }
                        MixBlend::Add => {
                            *cx += x * alpha;
                            *cy += y * alpha;
                        }
                    }
                }
                BoneProperty::Scale => {
                    if time < curve.frames[0] {
                        match blend {
                            MixBlend::Setup => {
                                b.scale_x = setup.scale_x;
                                b.scale_y = setup.scale_y;
                            }
                            MixBlend::First => {
                                b.scale_x += (setup.scale_x - b.scale_x) * alpha;
                                b.scale_y += (setup.scale_y - b.scale_y) * alpha;
                            }
                            _ => {}
                        }
                        return;
                    }
                    let i = curve.index(time);
                    let mut xy = [0f32; 2];
                    curve.values_at(time, i, &mut xy);
                    let (x, y) = (xy[0] * setup.scale_x, xy[1] * setup.scale_y);
                    if alpha == 1.0 {
                        if blend == MixBlend::Add {
                            b.scale_x += x - setup.scale_x;
                            b.scale_y += y - setup.scale_y;
                        } else {
                            b.scale_x = x;
                            b.scale_y = y;
                        }
                    } else if direction == MixDirection::Out {
                        match blend {
                            MixBlend::Setup => {
                                let (bx, by) = (setup.scale_x, setup.scale_y);
                                b.scale_x = bx + (abs(x) * signum(bx) - bx) * alpha;
                                b.scale_y = by + (abs(y) * signum(by) - by) * alpha;
                            }
                            MixBlend::First | MixBlend::Replace => {
                                let (bx, by) = (b.scale_x, b.scale_y);
                                b.scale_x = bx + (abs(x) * signum(bx) - bx) * alpha;
                                b.scale_y = by + (abs(y) * signum(by) - by) * alpha;
                            }
                            MixBlend::Add => {
                                b.scale_x += (x - setup.scale_x) * alpha;
                                b.scale_y += (y - setup.scale_y) * alpha;
                            }
                        }
                    } else {
                        match blend {
                            MixBlend::Setup => {
                                let bx = abs(setup.scale_x) * signum(x);
                                let by = abs(setup.scale_y) * signum(y);
                                b.scale_x = bx + (x - bx) * alpha;
                                b.scale_y = by + (y - by) * alpha;
                            }
                            MixBlend::First | MixBlend::Replace => {
                                let bx = abs(b.scale_x) * signum(x);
                                let by = abs(b.scale_y) * signum(y);
                                b.scale_x = bx + (x - bx) * alpha;
                                b.scale_y = by + (y - by) * alpha;
                            }
                            MixBlend::Add => {
                                b.scale_x += (x - setup.scale_x) * alpha;
                                b.scale_y += (y - setup.scale_y) * alpha;
                            }
                        }
                    }
                }
            }
        }
        Timeline::Inherit { bone, frames, values } => {
            let bi = *bone as usize;
            if !sk.bones[bi].active {
                return;
            }
            if direction == MixDirection::Out {
                if blend == MixBlend::Setup {
                    sk.bones[bi].inherit = data.bones[bi].inherit;
                }
                return;
            }
            if time < frames[0] {
                if matches!(blend, MixBlend::Setup | MixBlend::First) {
                    sk.bones[bi].inherit = data.bones[bi].inherit;
                }
                return;
            }
            sk.bones[bi].inherit = values[search1(frames, time)];
        }
        Timeline::SlotColor { slot, kind, curve } => {
            let si = *slot as usize;
            let bone = sk.slots[si].bone as usize;
            if !sk.bones[bone].active {
                return;
            }
            let setup = data.slots[si].color;
            let setup_dark = data.slots[si].dark.unwrap_or(Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 });
            let s = &mut sk.slots[si];
            if time < curve.frames[0] {
                match (blend, kind) {
                    (MixBlend::Setup, SlotColorKind::Rgba) => s.color = setup,
                    (MixBlend::First, SlotColorKind::Rgba) => mix_color_to(&mut s.color, setup, alpha),
                    (MixBlend::Setup, SlotColorKind::Rgb) => {
                        s.color.r = setup.r;
                        s.color.g = setup.g;
                        s.color.b = setup.b;
                    }
                    (MixBlend::First, SlotColorKind::Rgb) => {
                        s.color.r += (setup.r - s.color.r) * alpha;
                        s.color.g += (setup.g - s.color.g) * alpha;
                        s.color.b += (setup.b - s.color.b) * alpha;
                    }
                    (MixBlend::Setup, SlotColorKind::Alpha) => s.color.a = setup.a,
                    (MixBlend::First, SlotColorKind::Alpha) => s.color.a += (setup.a - s.color.a) * alpha,
                    (MixBlend::Setup, SlotColorKind::Rgba2) => {
                        s.color = setup;
                        if let Some(d) = &mut s.dark {
                            d.r = setup_dark.r;
                            d.g = setup_dark.g;
                            d.b = setup_dark.b;
                        }
                    }
                    (MixBlend::First, SlotColorKind::Rgba2) => {
                        mix_color_to(&mut s.color, setup, alpha);
                        if let Some(d) = &mut s.dark {
                            d.r += (setup_dark.r - d.r) * alpha;
                            d.g += (setup_dark.g - d.g) * alpha;
                            d.b += (setup_dark.b - d.b) * alpha;
                        }
                    }
                    (MixBlend::Setup, SlotColorKind::Rgb2) => {
                        s.color.r = setup.r;
                        s.color.g = setup.g;
                        s.color.b = setup.b;
                        if let Some(d) = &mut s.dark {
                            d.r = setup_dark.r;
                            d.g = setup_dark.g;
                            d.b = setup_dark.b;
                        }
                    }
                    (MixBlend::First, SlotColorKind::Rgb2) => {
                        s.color.r += (setup.r - s.color.r) * alpha;
                        s.color.g += (setup.g - s.color.g) * alpha;
                        s.color.b += (setup.b - s.color.b) * alpha;
                        if let Some(d) = &mut s.dark {
                            d.r += (setup_dark.r - d.r) * alpha;
                            d.g += (setup_dark.g - d.g) * alpha;
                            d.b += (setup_dark.b - d.b) * alpha;
                        }
                    }
                    _ => {}
                }
                return;
            }
            let e = curve.entries as usize;
            let i = curve.index(time);
            let mut v = [0f32; 7];
            curve.values_at(time, i, &mut v[..e - 1]);
            match kind {
                SlotColorKind::Rgba => {
                    if alpha == 1.0 {
                        set_color(&mut s.color, v[0], v[1], v[2], v[3]);
                    } else {
                        if blend == MixBlend::Setup {
                            s.color = setup;
                        }
                        let c = s.color;
                        add_color(&mut s.color, (v[0] - c.r) * alpha, (v[1] - c.g) * alpha, (v[2] - c.b) * alpha, (v[3] - c.a) * alpha);
                    }
                }
                SlotColorKind::Rgb => {
                    if alpha == 1.0 {
                        s.color.r = v[0];
                        s.color.g = v[1];
                        s.color.b = v[2];
                    } else {
                        if blend == MixBlend::Setup {
                            s.color.r = setup.r;
                            s.color.g = setup.g;
                            s.color.b = setup.b;
                        }
                        s.color.r += (v[0] - s.color.r) * alpha;
                        s.color.g += (v[1] - s.color.g) * alpha;
                        s.color.b += (v[2] - s.color.b) * alpha;
                    }
                }
                SlotColorKind::Alpha => {
                    if alpha == 1.0 {
                        s.color.a = v[0];
                    } else {
                        if blend == MixBlend::Setup {
                            s.color.a = setup.a;
                        }
                        s.color.a += (v[0] - s.color.a) * alpha;
                    }
                }
                SlotColorKind::Rgba2 => {
                    if alpha == 1.0 {
                        set_color(&mut s.color, v[0], v[1], v[2], v[3]);
                        if let Some(d) = &mut s.dark {
                            d.r = v[4];
                            d.g = v[5];
                            d.b = v[6];
                        }
                    } else {
                        if blend == MixBlend::Setup {
                            s.color = setup;
                            if let Some(d) = &mut s.dark {
                                d.r = setup_dark.r;
                                d.g = setup_dark.g;
                                d.b = setup_dark.b;
                            }
                        }
                        let c = s.color;
                        add_color(&mut s.color, (v[0] - c.r) * alpha, (v[1] - c.g) * alpha, (v[2] - c.b) * alpha, (v[3] - c.a) * alpha);
                        if let Some(d) = &mut s.dark {
                            d.r += (v[4] - d.r) * alpha;
                            d.g += (v[5] - d.g) * alpha;
                            d.b += (v[6] - d.b) * alpha;
                        }
                    }
                }
                SlotColorKind::Rgb2 => {
                    if alpha == 1.0 {
                        s.color.r = v[0];
                        s.color.g = v[1];
                        s.color.b = v[2];
                        if let Some(d) = &mut s.dark {
                            d.r = v[3];
                            d.g = v[4];
                            d.b = v[5];
                        }
                    } else {
                        if blend == MixBlend::Setup {
                            s.color.r = setup.r;
                            s.color.g = setup.g;
                            s.color.b = setup.b;
                            if let Some(d) = &mut s.dark {
                                d.r = setup_dark.r;
                                d.g = setup_dark.g;
                                d.b = setup_dark.b;
                            }
                        }
                        s.color.r += (v[0] - s.color.r) * alpha;
                        s.color.g += (v[1] - s.color.g) * alpha;
                        s.color.b += (v[2] - s.color.b) * alpha;
                        if let Some(d) = &mut s.dark {
                            d.r += (v[3] - d.r) * alpha;
                            d.g += (v[4] - d.g) * alpha;
                            d.b += (v[5] - d.b) * alpha;
                        }
                    }
                }
            }
        }
        Timeline::Attachment { slot, frames, names } => {
            let si = *slot as usize;
            if !sk.bones[sk.slots[si].bone as usize].active {
                return;
            }
            let name: Option<&str> = if direction == MixDirection::Out {
                if blend != MixBlend::Setup {
                    return;
                }
                data.slots[si].attachment.as_deref()
            } else if time < frames[0] {
                if !matches!(blend, MixBlend::Setup | MixBlend::First) {
                    return;
                }
                data.slots[si].attachment.as_deref()
            } else {
                names[search1(frames, time)].as_deref()
            };
            set_attachment_by_name(data, sk, si, name);
        }
        Timeline::Deform { slot, attachment, curve, vertices, constant } => {
            apply_deform(data, sk, *slot as usize, *attachment, curve, vertices, *constant, time, alpha, blend);
        }
        Timeline::Event { frames, .. } => {
            let Some(events) = events else { return };
            apply_events(frames, last_time, time, events);
        }
        Timeline::DrawOrder { frames, orders } => {
            let reset = |sk: &mut Skeleton| {
                for (i, o) in sk.draw_order.iter_mut().enumerate() {
                    *o = i as u16;
                }
            };
            if direction == MixDirection::Out {
                if blend == MixBlend::Setup {
                    reset(sk);
                }
                return;
            }
            if time < frames[0] {
                if matches!(blend, MixBlend::Setup | MixBlend::First) {
                    reset(sk);
                }
                return;
            }
            match &orders[search1(frames, time)] {
                None => reset(sk),
                Some(order) => sk.draw_order.copy_from_slice(order),
            }
        }
        Timeline::Ik { constraint, curve } => {
            let ci = *constraint as usize;
            if !sk.ik[ci].active {
                return;
            }
            let d = &data.ik_constraints[ci];
            let c = &mut sk.ik[ci];
            if time < curve.frames[0] {
                match blend {
                    MixBlend::Setup => {
                        c.mix = d.mix;
                        c.softness = d.softness;
                        c.bend_direction = d.bend_direction;
                        c.compress = d.compress;
                        c.stretch = d.stretch;
                    }
                    MixBlend::First => {
                        c.mix += (d.mix - c.mix) * alpha;
                        c.softness += (d.softness - c.softness) * alpha;
                        c.bend_direction = d.bend_direction;
                        c.compress = d.compress;
                        c.stretch = d.stretch;
                    }
                    _ => {}
                }
                return;
            }
            let i = curve.index(time);
            let mix = curve.value_at(time, i, 1);
            let softness = curve.value_at(time, i, 2);
            let f = &curve.frames;
            if blend == MixBlend::Setup {
                c.mix = d.mix + (mix - d.mix) * alpha;
                c.softness = d.softness + (softness - d.softness) * alpha;
                if direction == MixDirection::Out {
                    c.bend_direction = d.bend_direction;
                    c.compress = d.compress;
                    c.stretch = d.stretch;
                } else {
                    c.bend_direction = f[i + 3] as i8;
                    c.compress = f[i + 4] != 0.0;
                    c.stretch = f[i + 5] != 0.0;
                }
            } else {
                c.mix += (mix - c.mix) * alpha;
                c.softness += (softness - c.softness) * alpha;
                if direction == MixDirection::In {
                    c.bend_direction = f[i + 3] as i8;
                    c.compress = f[i + 4] != 0.0;
                    c.stretch = f[i + 5] != 0.0;
                }
            }
        }
        Timeline::Transform { constraint, curve } => {
            let ci = *constraint as usize;
            if !sk.transform[ci].active {
                return;
            }
            let d = &data.transform_constraints[ci];
            let c = &mut sk.transform[ci];
            let setup = [d.mix_rotate, d.mix_x, d.mix_y, d.mix_scale_x, d.mix_scale_y, d.mix_shear_y];
            let mut cur = [c.mix_rotate, c.mix_x, c.mix_y, c.mix_scale_x, c.mix_scale_y, c.mix_shear_y];
            if time < curve.frames[0] {
                match blend {
                    MixBlend::Setup => cur = setup,
                    MixBlend::First => {
                        for k in 0..6 {
                            cur[k] += (setup[k] - cur[k]) * alpha;
                        }
                    }
                    _ => return,
                }
            } else {
                let i = curve.index(time);
                for k in 0..6 {
                    let v = curve.value_at(time, i, k + 1);
                    if blend == MixBlend::Setup {
                        cur[k] = setup[k] + (v - setup[k]) * alpha;
                    } else {
                        cur[k] += (v - cur[k]) * alpha;
                    }
                }
            }
            c.mix_rotate = cur[0];
            c.mix_x = cur[1];
            c.mix_y = cur[2];
            c.mix_scale_x = cur[3];
            c.mix_scale_y = cur[4];
            c.mix_shear_y = cur[5];
        }
        Timeline::Path { constraint, property, curve } => {
            let ci = *constraint as usize;
            if !sk.path[ci].active {
                return;
            }
            let d = &data.path_constraints[ci];
            let c = &mut sk.path[ci];
            match property {
                PathProperty::Position => c.position = curve.absolute(time, alpha, blend, c.position, d.position),
                PathProperty::Spacing => c.spacing = curve.absolute(time, alpha, blend, c.spacing, d.spacing),
                PathProperty::Mix => {
                    let setup = [d.mix_rotate, d.mix_x, d.mix_y];
                    let mut cur = [c.mix_rotate, c.mix_x, c.mix_y];
                    if time < curve.frames[0] {
                        match blend {
                            MixBlend::Setup => cur = setup,
                            MixBlend::First => {
                                for k in 0..3 {
                                    cur[k] += (setup[k] - cur[k]) * alpha;
                                }
                            }
                            _ => return,
                        }
                    } else {
                        let i = curve.index(time);
                        for k in 0..3 {
                            let v = curve.value_at(time, i, k + 1);
                            if blend == MixBlend::Setup {
                                cur[k] = setup[k] + (v - setup[k]) * alpha;
                            } else {
                                cur[k] += (v - cur[k]) * alpha;
                            }
                        }
                    }
                    c.mix_rotate = cur[0];
                    c.mix_x = cur[1];
                    c.mix_y = cur[2];
                }
            }
        }
        Timeline::Physics { constraint, property, curve } => {
            fn field(c: &mut crate::skeleton::PhysicsConstraint, p: PhysicsProperty) -> &mut f32 {
                match p {
                    PhysicsProperty::Inertia => &mut c.inertia,
                    PhysicsProperty::Strength => &mut c.strength,
                    PhysicsProperty::Damping => &mut c.damping,
                    PhysicsProperty::Mass => &mut c.mass_inverse,
                    PhysicsProperty::Wind => &mut c.wind,
                    PhysicsProperty::Gravity => &mut c.gravity,
                    PhysicsProperty::Mix => &mut c.mix,
                }
            }
            fn setup_of(d: &PhysicsConstraintData, p: PhysicsProperty) -> (f32, bool) {
                match p {
                    PhysicsProperty::Inertia => (d.inertia, d.inertia_global),
                    PhysicsProperty::Strength => (d.strength, d.strength_global),
                    PhysicsProperty::Damping => (d.damping, d.damping_global),
                    PhysicsProperty::Mass => (d.mass_inverse, d.mass_global),
                    PhysicsProperty::Wind => (d.wind, d.wind_global),
                    PhysicsProperty::Gravity => (d.gravity, d.gravity_global),
                    PhysicsProperty::Mix => (d.mix, d.mix_global),
                }
            }
            // Mass timelines key the mass; the constraint stores its inverse.
            let mass = *property == PhysicsProperty::Mass;
            let get = |v: f32| if mass { 1.0 / v } else { v };
            match constraint {
                None => {
                    let value = if time >= curve.frames[0] { curve.curve_value(time) } else { 0.0 };
                    for (ci, c) in sk.physics.iter_mut().enumerate() {
                        let (setup, global) = setup_of(&data.physics_constraints[ci], *property);
                        if c.active && global {
                            let f = field(c, *property);
                            let v = curve.absolute2(time, alpha, blend, get(*f), get(setup), value);
                            *f = get(v);
                        }
                    }
                }
                Some(ci) => {
                    let ci = *ci as usize;
                    let (setup, _) = setup_of(&data.physics_constraints[ci], *property);
                    let c = &mut sk.physics[ci];
                    if c.active {
                        let f = field(c, *property);
                        let v = curve.absolute(time, alpha, blend, get(*f), get(setup));
                        *f = get(v);
                    }
                }
            }
        }
        Timeline::PhysicsReset { constraint, frames } => {
            if let Some(ci) = constraint {
                if !sk.physics[*ci as usize].active {
                    return;
                }
            }
            let mut last_time = last_time;
            if last_time > time {
                apply_timeline(t, data, sk, last_time, f32::MAX, None, alpha, blend, direction);
                last_time = -1.0;
            } else if last_time >= frames[frames.len() - 1] {
                return;
            }
            if time < frames[0] {
                return;
            }
            if last_time < frames[0] || time >= frames[search1(frames, last_time) + 1] {
                match constraint {
                    Some(ci) => sk.reset_physics_one(*ci as usize),
                    None => {
                        for ci in 0..sk.physics.len() {
                            if sk.physics[ci].active {
                                sk.reset_physics_one(ci);
                            }
                        }
                    }
                }
            }
        }
        Timeline::Sequence { slot, attachment, frames } => {
            let si = *slot as usize;
            if !sk.bones[sk.slots[si].bone as usize].active {
                return;
            }
            let Some(sa) = sk.slots[si].attachment else { return };
            if sa != *attachment {
                let a = &data.attachments[sa as usize];
                if a.vertices().is_none() || a.timeline_attachment != *attachment {
                    return;
                }
            }
            if direction == MixDirection::Out {
                if blend == MixBlend::Setup {
                    sk.slots[si].sequence_index = -1;
                }
                return;
            }
            if time < frames[0] {
                if matches!(blend, MixBlend::Setup | MixBlend::First) {
                    sk.slots[si].sequence_index = -1;
                }
                return;
            }
            let Some(seq) = data.attachments[*attachment as usize].sequence() else { return };
            let i = search(frames, time, 3);
            let before = frames[i];
            let mode_and_index = frames[i + 1].to_bits() as i32;
            let delay = frames[i + 2];
            let mut index = mode_and_index >> 4;
            let count = seq.regions.len() as i32;
            let mode = SequenceMode::from_u32((mode_and_index & 0xf) as u32);
            if mode != SequenceMode::Hold {
                index += ((time - before) / delay + 0.00001) as i32;
                match mode {
                    SequenceMode::Once => index = index.min(count - 1),
                    SequenceMode::Loop => index %= count,
                    SequenceMode::Pingpong => {
                        let n = (count << 1) - 2;
                        index = if n == 0 { 0 } else { index % n };
                        if index >= count {
                            index = n - index;
                        }
                    }
                    SequenceMode::OnceReverse => index = (count - 1 - index).max(0),
                    SequenceMode::LoopReverse => index = count - 1 - (index % count),
                    SequenceMode::PingpongReverse => {
                        let n = (count << 1) - 2;
                        index = if n == 0 { 0 } else { (index + count - 1) % n };
                        if index >= count {
                            index = n - index;
                        }
                    }
                    SequenceMode::Hold => {}
                }
            }
            sk.slots[si].sequence_index = index;
        }
    }
}

pub fn set_attachment_by_name(data: &SkeletonData, sk: &mut Skeleton, si: usize, name: Option<&str>) {
    let id = name.and_then(|n| sk.attachment(data, si as u16, n));
    set_slot_attachment(data, &mut sk.slots[si], id);
}

fn apply_events(frames: &[f32], last_time: f32, time: f32, out: &mut Vec<u16>) {
    let frame_count = frames.len();
    let mut last_time = last_time;
    if last_time > time {
        apply_events(frames, last_time, f32::MAX, out);
        last_time = -1.0;
    } else if last_time >= frames[frame_count - 1] {
        return;
    }
    if time < frames[0] {
        return;
    }
    let mut i;
    if last_time < frames[0] {
        i = 0;
    } else {
        i = search1(frames, last_time) + 1;
        let frame_time = frames[i.min(frame_count - 1)];
        while i > 0 {
            if frames[i - 1] != frame_time {
                break;
            }
            i -= 1;
        }
    }
    while i < frame_count && time >= frames[i] {
        out.push(i as u16);
        i += 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_deform(
    data: &SkeletonData,
    sk: &mut Skeleton,
    si: usize,
    attachment: u32,
    curve: &Curve,
    vertices: &[Vec<f32>],
    constant: bool,
    time: f32,
    alpha: f32,
    mut blend: MixBlend,
) {
    if !sk.bones[sk.slots[si].bone as usize].active {
        return;
    }
    let Some(sa) = sk.slots[si].attachment else { return };
    let a = &data.attachments[sa as usize];
    let Some(verts) = a.vertices() else { return };
    if a.timeline_attachment != attachment {
        return;
    }
    let slot = &mut sk.slots[si];
    // A constant timeline at full weight (not added) leaves the deform equal to its key, whatever
    // the time past the first frame and whatever was there (the general paths below compute
    // exactly the key then: p + (p - p) * percent == p). When the deform already holds that key,
    // there is nothing to do: idle animations can carry tens of thousands of such values.
    if constant && alpha == 1.0 && blend != MixBlend::Add && time >= curve.frames[0] {
        let key = &vertices[0];
        let id = key.as_ptr() as usize;
        if slot.deform_key != id || slot.deform.len() != key.len() {
            slot.deform.clear();
            slot.deform.extend_from_slice(key);
            slot.deform_key = id;
        }
        return;
    }
    slot.deform_key = 0;
    let weighted = verts.is_weighted();
    let setup = &verts.vertices;
    let deform = &mut slot.deform;
    if deform.is_empty() {
        blend = MixBlend::Setup;
    }
    let vertex_count = vertices[0].len();
    let frames = &curve.frames;
    if time < frames[0] {
        match blend {
            MixBlend::Setup => deform.clear(),
            MixBlend::First => {
                if alpha == 1.0 {
                    deform.clear();
                    return;
                }
                deform.resize(vertex_count, 0.0);
                if !weighted {
                    for i in 0..vertex_count {
                        deform[i] += (setup[i] - deform[i]) * alpha;
                    }
                } else {
                    let a = 1.0 - alpha;
                    for v in deform.iter_mut().take(vertex_count) {
                        *v *= a;
                    }
                }
            }
            _ => {}
        }
        return;
    }
    deform.resize(vertex_count, 0.0);
    if time >= frames[frames.len() - 1] {
        let last = &vertices[frames.len() - 1];
        if alpha == 1.0 {
            if blend == MixBlend::Add {
                if !weighted {
                    for i in 0..vertex_count {
                        deform[i] += last[i] - setup[i];
                    }
                } else {
                    for i in 0..vertex_count {
                        deform[i] += last[i];
                    }
                }
            } else {
                deform[..vertex_count].copy_from_slice(&last[..vertex_count]);
            }
        } else {
            match blend {
                MixBlend::Setup => {
                    if !weighted {
                        for i in 0..vertex_count {
                            let s = setup[i];
                            deform[i] = s + (last[i] - s) * alpha;
                        }
                    } else {
                        for i in 0..vertex_count {
                            deform[i] = last[i] * alpha;
                        }
                    }
                }
                MixBlend::First | MixBlend::Replace => {
                    for i in 0..vertex_count {
                        deform[i] += (last[i] - deform[i]) * alpha;
                    }
                }
                MixBlend::Add => {
                    if !weighted {
                        for i in 0..vertex_count {
                            deform[i] += (last[i] - setup[i]) * alpha;
                        }
                    } else {
                        for i in 0..vertex_count {
                            deform[i] += last[i] * alpha;
                        }
                    }
                }
            }
        }
        return;
    }
    let frame = search1(frames, time);
    let percent = curve.curve_percent(time, frame);
    let prev = &vertices[frame];
    let next = &vertices[frame + 1];
    if alpha == 1.0 {
        if blend == MixBlend::Add {
            if !weighted {
                for i in 0..vertex_count {
                    let p = prev[i];
                    deform[i] += p + (next[i] - p) * percent - setup[i];
                }
            } else {
                for i in 0..vertex_count {
                    let p = prev[i];
                    deform[i] += p + (next[i] - p) * percent;
                }
            }
        } else {
            for i in 0..vertex_count {
                let p = prev[i];
                deform[i] = p + (next[i] - p) * percent;
            }
        }
    } else {
        match blend {
            MixBlend::Setup => {
                if !weighted {
                    for i in 0..vertex_count {
                        let p = prev[i];
                        let s = setup[i];
                        deform[i] = s + (p + (next[i] - p) * percent - s) * alpha;
                    }
                } else {
                    for i in 0..vertex_count {
                        let p = prev[i];
                        deform[i] = (p + (next[i] - p) * percent) * alpha;
                    }
                }
            }
            MixBlend::First | MixBlend::Replace => {
                for i in 0..vertex_count {
                    let p = prev[i];
                    deform[i] += (p + (next[i] - p) * percent - deform[i]) * alpha;
                }
            }
            MixBlend::Add => {
                if !weighted {
                    for i in 0..vertex_count {
                        let p = prev[i];
                        deform[i] += (p + (next[i] - p) * percent - setup[i]) * alpha;
                    }
                } else {
                    for i in 0..vertex_count {
                        let p = prev[i];
                        deform[i] += (p + (next[i] - p) * percent) * alpha;
                    }
                }
            }
        }
    }
}

/// Applies all of an animation's timelines (`Animation.apply`).
#[allow(clippy::too_many_arguments)]
pub fn apply_animation(
    data: &SkeletonData,
    animation: usize,
    sk: &mut Skeleton,
    mut last_time: f32,
    mut time: f32,
    looped: bool,
    mut events: Option<&mut Vec<FiredEvent>>,
    alpha: f32,
    blend: MixBlend,
    direction: MixDirection,
) {
    let a = &data.animations[animation];
    if looped && a.duration != 0.0 {
        time = fmod(time, a.duration);
        if last_time > 0.0 {
            last_time = fmod(last_time, a.duration);
        }
    }
    let mut scratch = Vec::new();
    for t in &a.timelines {
        let ev = if matches!(t, Timeline::Event { .. }) && events.is_some() { Some(&mut scratch) } else { None };
        apply_timeline(t, data, sk, last_time, time, ev, alpha, blend, direction);
    }
    if let Some(out) = events.as_deref_mut() {
        for k in scratch {
            out.push(FiredEvent { animation: animation as u16, key: k });
        }
    }
}

/// `binary::sample_bezier`'s forward differencing, one sample at a time.
struct Bezier {
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    ddx: f32,
    ddy: f32,
    dddx: f32,
    dddy: f32,
}

impl Bezier {
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn new(time1: f32, value1: f32, cx1: f32, cy1: f32, cx2: f32, cy2: f32, time2: f32, value2: f32) -> Bezier {
        let tmpx = (time1 - cx1 * 2.0 + cx2) * 0.03;
        let tmpy = (value1 - cy1 * 2.0 + cy2) * 0.03;
        let dddx = ((cx1 - cx2) * 3.0 - time1 + time2) * 0.006;
        let dddy = ((cy1 - cy2) * 3.0 - value1 + value2) * 0.006;
        let ddx = tmpx * 2.0 + dddx;
        let ddy = tmpy * 2.0 + dddy;
        let dx = (cx1 - time1) * 0.3 + tmpx + dddx * 0.166_666_67;
        let dy = (cy1 - value1) * 0.3 + tmpy + dddy * 0.166_666_67;
        Bezier { x: time1 + dx, y: value1 + dy, dx, dy, ddx, ddy, dddx, dddy }
    }

    /// The next sample (x, y).
    #[inline]
    fn next(&mut self) -> (f32, f32) {
        let out = (self.x, self.y);
        self.dx += self.ddx;
        self.dy += self.ddy;
        self.ddx += self.dddx;
        self.ddy += self.dddy;
        self.x += self.dx;
        self.y += self.dy;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Three keys of (x, y) with Beziers between them: the first segment linked (y's handles are
    /// x's easing mapped to y's keys), the second not. The third channel never moves.
    fn translate() -> Curve {
        let frames = vec![0.0, 10.0, -4.0, 7.0, 1.5, 30.0, 6.0, 7.0, 3.0, 20.0, 8.0, 7.0];
        let ease = |t1: f32, t2: f32, v1: f32, v2: f32| [t1 + 0.25 * (t2 - t1), v1 + 0.1 * (v2 - v1), t1 + 0.7 * (t2 - t1), v1 + 1.2 * (v2 - v1)];
        let mut curves = vec![CURVE_BEZIER + 3.0, CURVE_BEZIER + 15.0, CURVE_STEPPED];
        for (t1, t2, k) in [(0.0, 1.5, 0), (1.5, 3.0, 1)] {
            curves.extend_from_slice(&ease(t1, t2, frames[k * 4 + 1], frames[k * 4 + 5]));
            let mut y = ease(t1, t2, frames[k * 4 + 2], frames[k * 4 + 6]);
            if k == 1 {
                y[1] += 0.5;
            }
            curves.extend_from_slice(&y);
            curves.extend_from_slice(&[t1 + 0.4, 7.0, t2 - 0.4, 7.0]);
        }
        Curve { entries: 4, channels: 3, frames, curves, constant: false }
    }

    #[test]
    fn linked_channels_store_one_easing_and_evaluate_the_same() {
        let plain = translate();
        let mut shared = plain.clone();
        shared.share_easings();
        assert!(shared.curves[0] <= -CURVE_BEZIER && shared.curves[1] >= CURVE_BEZIER);
        assert_eq!(shared.curves.len(), plain.curves.len() - 2 * BEZIER_SIZE);
        assert_eq!(shared.bezier_count(), plain.bezier_count());
        // The channel that never moves takes the shared timing (its value stays put either way).
        for k in 0..2 {
            for v in 0..3 {
                let (a, b) = (plain.bezier_controls(k, v), shared.bezier_controls(k, v));
                let check = if v == 2 { &[1, 3][..] } else { &[0, 1, 2, 3][..] };
                assert!(check.iter().all(|&j| (a[j] - b[j]).abs() <= 1e-5 * a[j].abs().max(1.0)), "{k} {v}: {a:?} {b:?}");
            }
        }
        let (mut va, mut vb) = ([0f32; 3], [0f32; 3]);
        for s in 0..300 {
            let t = s as f32 * 0.01;
            let i = plain.index(t);
            plain.values_at(t, i, &mut va);
            shared.values_at(t, i, &mut vb);
            for v in 0..3 {
                assert!((va[v] - vb[v]).abs() < 1e-4, "t {t} value {v}: {} vs {}", va[v], vb[v]);
                assert!((plain.value_at(t, i, v + 1) - shared.value_at(t, i, v + 1)).abs() < 1e-4);
            }
            assert_eq!(vb[2], 7.0);
        }
    }
}

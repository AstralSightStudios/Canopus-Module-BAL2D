//! AnimationState: tracks of queued animations with mixing, following Spine 4.2 semantics
//! (timeline modes and hold computation, shortest-rotation mixing, unkeyed attachment reset,
//! event and completion queues).

use alloc::vec::Vec;

use crate::data::*;
use crate::math::*;
use crate::skeleton::{Skeleton, set_slot_attachment};
use crate::timeline::*;

const SUBSEQUENT: u8 = 0;
const FIRST: u8 = 1;
const HOLD_SUBSEQUENT: u8 = 2;
const HOLD_FIRST: u8 = 3;
const HOLD_MIX: u8 = 4;

const SETUP: u32 = 1;
const CURRENT: u32 = 2;

pub type EntryId = u32;

#[derive(Clone, Debug)]
pub struct TrackEntry {
    pub track_index: usize,
    /// `None` is the empty animation.
    pub animation: Option<usize>,
    pub looped: bool,
    pub hold_previous: bool,
    pub reverse: bool,
    pub shortest_rotation: bool,
    pub event_threshold: f32,
    pub alpha_attachment_threshold: f32,
    pub mix_attachment_threshold: f32,
    pub mix_draw_order_threshold: f32,
    pub animation_start: f32,
    pub animation_end: f32,
    pub animation_last: f32,
    pub next_animation_last: f32,
    pub delay: f32,
    pub track_time: f32,
    pub track_last: f32,
    pub next_track_last: f32,
    pub track_end: f32,
    pub time_scale: f32,
    pub alpha: f32,
    pub mix_time: f32,
    pub mix_duration: f32,
    pub interrupt_alpha: f32,
    pub total_alpha: f32,
    pub mix_blend: MixBlend,
    timeline_mode: Vec<u8>,
    timeline_hold_mix: Vec<Option<EntryId>>,
    timelines_rotation: Vec<f32>,
    pub next: Option<EntryId>,
    pub previous: Option<EntryId>,
    pub mixing_from: Option<EntryId>,
    pub mixing_to: Option<EntryId>,
}

impl TrackEntry {
    pub fn animation_time(&self) -> f32 {
        if self.looped {
            let duration = self.animation_end - self.animation_start;
            if duration == 0.0 {
                return self.animation_start;
            }
            return fmod(self.track_time, duration) + self.animation_start;
        }
        f32::min(self.track_time + self.animation_start, self.animation_end)
    }

    pub fn track_complete(&self) -> f32 {
        let duration = self.animation_end - self.animation_start;
        if duration != 0.0 {
            if self.looped {
                return duration * (1.0 + to_int(self.track_time / duration));
            }
            if self.track_time < duration {
                return duration;
            }
        }
        self.track_time
    }

    pub fn is_complete(&self) -> bool {
        self.track_time >= self.animation_end - self.animation_start
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateEventKind {
    Start,
    Interrupt,
    End,
    Dispose,
    Complete,
    /// A keyed event: the animation and the key index in its event timeline.
    Event { animation: u16, key: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateEvent {
    pub kind: StateEventKind,
    pub entry: EntryId,
    pub track: usize,
    pub animation: Option<usize>,
}

/// Mix durations between animations.
#[derive(Clone, Debug, Default)]
pub struct AnimationStateData {
    pub default_mix: f32,
    pub mixes: Vec<(usize, usize, f32)>,
}

impl AnimationStateData {
    pub fn mix(&self, from: Option<usize>, to: Option<usize>) -> f32 {
        if let (Some(f), Some(t)) = (from, to) {
            if let Some(m) = self.mixes.iter().find(|m| m.0 == f && m.1 == t) {
                return m.2;
            }
        }
        self.default_mix
    }
}

#[derive(Clone, Debug)]
pub struct AnimationState {
    pub data: AnimationStateData,
    pub tracks: Vec<Option<EntryId>>,
    pub time_scale: f32,
    entries: Vec<Option<TrackEntry>>,
    free: Vec<EntryId>,
    unkeyed_state: u32,
    attachment_state: Vec<u32>,
    fired: Vec<FiredEvent>,
    queue: Vec<StateEvent>,
    /// Events drained so far, for the caller to consume.
    pub output: Vec<StateEvent>,
    animations_changed: bool,
    property_ids: Vec<u64>,
    scratch_ids: Vec<u64>,
    /// Per-animation sorted property ids (`hasTimeline`).
    animation_ids: Vec<Vec<u64>>,
}

/// Tiny sort (lists are a few hundred ids; keeps the generic sort out of the watch image).
fn insertion_sort(v: &mut [u64]) {
    for i in 1..v.len() {
        let x = v[i];
        let mut j = i;
        while j > 0 && v[j - 1] > x {
            v[j] = v[j - 1];
            j -= 1;
        }
        v[j] = x;
    }
}

fn animation_property_ids(data: &SkeletonData) -> Vec<Vec<u64>> {
    data.animations
        .iter()
        .map(|a| {
            let mut ids = Vec::new();
            for t in &a.timelines {
                property_ids(t, &mut ids);
            }
            insertion_sort(&mut ids);
            ids.dedup();
            ids
        })
        .collect()
}

impl AnimationState {
    pub fn new(skeleton_data: &SkeletonData, data: AnimationStateData) -> AnimationState {
        AnimationState {
            data,
            tracks: Vec::new(),
            time_scale: 1.0,
            entries: Vec::new(),
            free: Vec::new(),
            unkeyed_state: 0,
            attachment_state: alloc::vec![0; skeleton_data.slots.len()],
            fired: Vec::new(),
            queue: Vec::new(),
            output: Vec::new(),
            animations_changed: false,
            property_ids: Vec::new(),
            scratch_ids: Vec::new(),
            animation_ids: animation_property_ids(skeleton_data),
        }
    }

    /// Whether any entry (playing, queued or mixing out) plays `animation`.
    pub fn uses(&self, animation: usize) -> bool {
        self.entries.iter().flatten().any(|e| e.animation == Some(animation))
    }

    pub fn entry(&self, id: EntryId) -> &TrackEntry {
        self.entries[id as usize].as_ref().expect("live track entry")
    }

    pub fn entry_mut(&mut self, id: EntryId) -> &mut TrackEntry {
        self.entries[id as usize].as_mut().expect("live track entry")
    }

    pub fn current(&self, track: usize) -> Option<EntryId> {
        self.tracks.get(track).copied().flatten()
    }

    fn e(&self, id: EntryId) -> &TrackEntry {
        self.entry(id)
    }

    fn em(&mut self, id: EntryId) -> &mut TrackEntry {
        self.entry_mut(id)
    }

    fn duration(&self, data: &SkeletonData, animation: Option<usize>) -> f32 {
        animation.map_or(0.0, |a| data.animations[a].duration)
    }

    // ---- event queue ----

    fn q(&mut self, kind: StateEventKind, entry: EntryId) {
        let e = self.e(entry);
        let ev = StateEvent { kind, entry, track: e.track_index, animation: e.animation };
        self.queue.push(ev);
        if matches!(kind, StateEventKind::Start | StateEventKind::End) {
            self.animations_changed = true;
        }
    }

    fn drain(&mut self) {
        let queued = core::mem::take(&mut self.queue);
        for ev in queued {
            self.output.push(ev);
            match ev.kind {
                StateEventKind::End => {
                    // `end` falls through to `dispose` in the reference: the entry is freed.
                    self.output.push(StateEvent { kind: StateEventKind::Dispose, ..ev });
                    self.release(ev.entry);
                }
                StateEventKind::Dispose => self.release(ev.entry),
                _ => {}
            }
        }
    }

    fn release(&mut self, id: EntryId) {
        if self.entries[id as usize].take().is_some() {
            self.free.push(id);
        }
    }

    // ---- update ----

    pub fn update(&mut self, data: &SkeletonData, delta: f32) {
        let delta = delta * self.time_scale;
        for i in 0..self.tracks.len() {
            let Some(cur) = self.tracks[i] else { continue };
            {
                let c = self.em(cur);
                c.animation_last = c.next_animation_last;
                c.track_last = c.next_track_last;
            }
            let mut current_delta = delta * self.e(cur).time_scale;
            if self.e(cur).delay > 0.0 {
                let c = self.em(cur);
                c.delay -= current_delta;
                if c.delay > 0.0 {
                    continue;
                }
                current_delta = -c.delay;
                c.delay = 0.0;
            }
            if let Some(next) = self.e(cur).next {
                let next_time = self.e(cur).track_last - self.e(next).delay;
                if next_time >= 0.0 {
                    let cur_scale = self.e(cur).time_scale;
                    let next_scale = self.e(next).time_scale;
                    {
                        let n = self.em(next);
                        n.delay = 0.0;
                        n.track_time += if cur_scale == 0.0 { 0.0 } else { (next_time / cur_scale + delta) * next_scale };
                    }
                    self.em(cur).track_time += current_delta;
                    self.set_current(data, i, next, true);
                    let mut n = Some(next);
                    while let Some(id) = n {
                        let from = self.e(id).mixing_from;
                        if from.is_none() {
                            break;
                        }
                        self.em(id).mix_time += delta;
                        n = from;
                    }
                    continue;
                }
            } else if self.e(cur).track_last >= self.e(cur).track_end && self.e(cur).mixing_from.is_none() {
                self.tracks[i] = None;
                self.q(StateEventKind::End, cur);
                self.clear_next(cur);
                continue;
            }
            if self.e(cur).mixing_from.is_some() && self.update_mixing_from(cur, delta) {
                let mut from = self.e(cur).mixing_from;
                self.em(cur).mixing_from = None;
                if let Some(f) = from {
                    self.em(f).mixing_to = None;
                }
                while let Some(f) = from {
                    self.q(StateEventKind::End, f);
                    from = self.e(f).mixing_from;
                }
            }
            self.em(cur).track_time += current_delta;
        }
        self.drain();
    }

    fn update_mixing_from(&mut self, to: EntryId, delta: f32) -> bool {
        let Some(from) = self.e(to).mixing_from else { return true };
        let finished = self.update_mixing_from(from, delta);
        {
            let f = self.em(from);
            f.animation_last = f.next_animation_last;
            f.track_last = f.next_track_last;
        }
        let (to_ntl, to_mt, to_md) = {
            let t = self.e(to);
            (t.next_track_last, t.mix_time, t.mix_duration)
        };
        if to_ntl != -1.0 && to_mt >= to_md {
            if self.e(from).total_alpha == 0.0 || to_md == 0.0 {
                let ff = self.e(from).mixing_from;
                let ia = self.e(from).interrupt_alpha;
                self.em(to).mixing_from = ff;
                if let Some(ff) = ff {
                    self.em(ff).mixing_to = Some(to);
                }
                self.em(to).interrupt_alpha = ia;
                self.q(StateEventKind::End, from);
            }
            return finished;
        }
        {
            let f = self.em(from);
            f.track_time += delta * f.time_scale;
        }
        self.em(to).mix_time += delta;
        false
    }

    // ---- apply ----

    pub fn apply(&mut self, data: &SkeletonData, sk: &mut Skeleton) -> bool {
        if self.animations_changed {
            self.animations_changed_impl(data);
        }
        let mut applied = false;
        for i in 0..self.tracks.len() {
            let Some(cur) = self.tracks[i] else { continue };
            if self.e(cur).delay > 0.0 {
                continue;
            }
            applied = true;
            let blend = if i == 0 { MixBlend::First } else { self.e(cur).mix_blend };
            let mut alpha = self.e(cur).alpha;
            if self.e(cur).mixing_from.is_some() {
                alpha *= self.apply_mixing_from(data, cur, sk, blend);
            } else if self.e(cur).track_time >= self.e(cur).track_end && self.e(cur).next.is_none() {
                alpha = 0.0;
            }
            let mut attachments = alpha >= self.e(cur).alpha_attachment_threshold;
            let animation_last = self.e(cur).animation_last;
            let animation_time = self.e(cur).animation_time();
            let mut apply_time = animation_time;
            let reverse = self.e(cur).reverse;
            let animation = self.e(cur).animation;
            if reverse {
                apply_time = self.duration(data, animation) - apply_time;
            }
            let collect_events = !reverse;
            if let Some(ai) = animation {
                let timelines = &data.animations[ai].timelines;
                let timeline_count = timelines.len();
                let mut fired = core::mem::take(&mut self.fired);
                fired.clear();
                if (i == 0 && alpha == 1.0) || blend == MixBlend::Add {
                    if i == 0 {
                        attachments = true;
                    }
                    for t in timelines {
                        if let Timeline::Attachment { .. } = t {
                            self.apply_attachment_timeline(data, t, sk, apply_time, blend, attachments);
                        } else {
                            self.apply_one(data, ai, t, sk, animation_last, apply_time, collect_events, &mut fired, alpha, blend, MixDirection::In);
                        }
                    }
                } else {
                    let shortest = self.e(cur).shortest_rotation;
                    let first_frame = !shortest && self.e(cur).timelines_rotation.len() != timeline_count << 1;
                    if first_frame {
                        self.em(cur).timelines_rotation.resize(timeline_count << 1, 0.0);
                    }
                    for (ii, t) in timelines.iter().enumerate() {
                        let mode = self.e(cur).timeline_mode[ii];
                        let timeline_blend = if mode == SUBSEQUENT { blend } else { MixBlend::Setup };
                        match t {
                            Timeline::Bone { property: BoneProperty::Rotate, .. } if !shortest => {
                                let mut rot = core::mem::take(&mut self.em(cur).timelines_rotation);
                                apply_rotate_timeline(data, t, sk, apply_time, alpha, timeline_blend, &mut rot, ii << 1, first_frame);
                                self.em(cur).timelines_rotation = rot;
                            }
                            Timeline::Attachment { .. } => {
                                self.apply_attachment_timeline(data, t, sk, apply_time, blend, attachments);
                            }
                            _ => self.apply_one(
                                data,
                                ai,
                                t,
                                sk,
                                animation_last,
                                apply_time,
                                collect_events,
                                &mut fired,
                                alpha,
                                timeline_blend,
                                MixDirection::In,
                            ),
                        }
                    }
                }
                self.queue_events(data, cur, animation_time, &fired);
                fired.clear();
                self.fired = fired;
            } else {
                self.queue_events(data, cur, animation_time, &[]);
            }
            let tt = self.e(cur).track_time;
            let c = self.em(cur);
            c.next_animation_last = animation_time;
            c.next_track_last = tt;
        }
        let setup_state = self.unkeyed_state + SETUP;
        for i in 0..sk.slots.len() {
            if self.attachment_state[i] == setup_state {
                let name = data.slots[i].attachment.as_deref();
                let id = name.and_then(|n| sk.attachment(data, i as u16, n));
                set_slot_attachment(data, &mut sk.slots[i], id);
            }
        }
        self.unkeyed_state += 2;
        self.drain();
        applied
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_one(
        &mut self,
        data: &SkeletonData,
        animation: usize,
        t: &Timeline,
        sk: &mut Skeleton,
        last: f32,
        time: f32,
        collect: bool,
        fired: &mut Vec<FiredEvent>,
        alpha: f32,
        blend: MixBlend,
        dir: MixDirection,
    ) {
        if let Timeline::Event { .. } = t {
            if collect {
                let mut keys = Vec::new();
                apply_timeline(t, data, sk, last, time, Some(&mut keys), alpha, blend, dir);
                for k in keys {
                    fired.push(FiredEvent { animation: animation as u16, key: k });
                }
            }
            return;
        }
        apply_timeline(t, data, sk, last, time, None, alpha, blend, dir);
    }

    fn apply_mixing_from(&mut self, data: &SkeletonData, to: EntryId, sk: &mut Skeleton, blend: MixBlend) -> f32 {
        let from = self.e(to).mixing_from.expect("mixing from");
        if self.e(from).mixing_from.is_some() {
            self.apply_mixing_from(data, from, sk, blend);
        }
        let mut blend = blend;
        let mix;
        let to_md = self.e(to).mix_duration;
        if to_md == 0.0 {
            mix = 1.0;
            if blend == MixBlend::First {
                blend = MixBlend::Setup;
            }
        } else {
            let mut m = self.e(to).mix_time / to_md;
            if m > 1.0 {
                m = 1.0;
            }
            mix = m;
            if blend != MixBlend::First {
                blend = self.e(from).mix_blend;
            }
        }
        let f = self.e(from);
        let attachments = mix < f.mix_attachment_threshold;
        let draw_order = mix < f.mix_draw_order_threshold;
        let alpha_hold = f.alpha * self.e(to).interrupt_alpha;
        let alpha_mix = alpha_hold * (1.0 - mix);
        let animation_last = f.animation_last;
        let animation_time = f.animation_time();
        let mut apply_time = animation_time;
        let animation = f.animation;
        let alpha_attachment_threshold = f.alpha_attachment_threshold;
        let mut collect = false;
        if f.reverse {
            apply_time = self.duration(data, animation) - apply_time;
        } else if mix < f.event_threshold {
            collect = true;
        }
        let mut fired = core::mem::take(&mut self.fired);
        fired.clear();
        if let Some(ai) = animation {
            let timelines = &data.animations[ai].timelines;
            let timeline_count = timelines.len();
            if blend == MixBlend::Add {
                for t in timelines {
                    self.apply_one(data, ai, t, sk, animation_last, apply_time, collect, &mut fired, alpha_mix, blend, MixDirection::Out);
                }
            } else {
                let shortest = self.e(from).shortest_rotation;
                let first_frame = !shortest && self.e(from).timelines_rotation.len() != timeline_count << 1;
                if first_frame {
                    self.em(from).timelines_rotation.resize(timeline_count << 1, 0.0);
                }
                self.em(from).total_alpha = 0.0;
                for (i, t) in timelines.iter().enumerate() {
                    let mut direction = MixDirection::Out;
                    let timeline_blend;
                    let alpha;
                    match self.e(from).timeline_mode[i] {
                        SUBSEQUENT => {
                            if !draw_order && matches!(t, Timeline::DrawOrder { .. }) {
                                continue;
                            }
                            timeline_blend = blend;
                            alpha = alpha_mix;
                        }
                        FIRST => {
                            timeline_blend = MixBlend::Setup;
                            alpha = alpha_mix;
                        }
                        HOLD_SUBSEQUENT => {
                            timeline_blend = blend;
                            alpha = alpha_hold;
                        }
                        HOLD_FIRST => {
                            timeline_blend = MixBlend::Setup;
                            alpha = alpha_hold;
                        }
                        _ => {
                            timeline_blend = MixBlend::Setup;
                            let hm = self.e(from).timeline_hold_mix[i].expect("hold mix entry");
                            let h = self.e(hm);
                            alpha = alpha_hold * f32::max(0.0, 1.0 - h.mix_time / h.mix_duration);
                        }
                    }
                    self.em(from).total_alpha += alpha;
                    match t {
                        Timeline::Bone { property: BoneProperty::Rotate, .. } if !shortest => {
                            let mut rot = core::mem::take(&mut self.em(from).timelines_rotation);
                            apply_rotate_timeline(data, t, sk, apply_time, alpha, timeline_blend, &mut rot, i << 1, first_frame);
                            self.em(from).timelines_rotation = rot;
                        }
                        Timeline::Attachment { .. } => {
                            self.apply_attachment_timeline(
                                data,
                                t,
                                sk,
                                apply_time,
                                timeline_blend,
                                attachments && alpha >= alpha_attachment_threshold,
                            );
                        }
                        _ => {
                            if draw_order && matches!(t, Timeline::DrawOrder { .. }) && timeline_blend == MixBlend::Setup {
                                direction = MixDirection::In;
                            }
                            self.apply_one(data, ai, t, sk, animation_last, apply_time, collect, &mut fired, alpha, timeline_blend, direction);
                        }
                    }
                }
            }
        }
        if to_md > 0.0 {
            self.queue_events(data, from, animation_time, &fired);
        }
        fired.clear();
        self.fired = fired;
        let tt = self.e(from).track_time;
        let f = self.em(from);
        f.next_animation_last = animation_time;
        f.next_track_last = tt;
        mix
    }

    fn apply_attachment_timeline(&mut self, data: &SkeletonData, t: &Timeline, sk: &mut Skeleton, time: f32, blend: MixBlend, attachments: bool) {
        let Timeline::Attachment { slot, frames, names } = t else { return };
        let si = *slot as usize;
        if !sk.bones[sk.slots[si].bone as usize].active {
            return;
        }
        if time < frames[0] {
            if matches!(blend, MixBlend::Setup | MixBlend::First) {
                let name = data.slots[si].attachment.as_deref();
                self.set_attachment(data, sk, si, name, attachments);
            }
        } else {
            let name = names[search1(frames, time)].as_deref();
            self.set_attachment(data, sk, si, name, attachments);
        }
        if self.attachment_state[si] <= self.unkeyed_state {
            self.attachment_state[si] = self.unkeyed_state + SETUP;
        }
    }

    fn set_attachment(&mut self, data: &SkeletonData, sk: &mut Skeleton, si: usize, name: Option<&str>, attachments: bool) {
        let id = name.and_then(|n| sk.attachment(data, si as u16, n));
        set_slot_attachment(data, &mut sk.slots[si], id);
        if attachments {
            self.attachment_state[si] = self.unkeyed_state + CURRENT;
        }
    }

    fn queue_events(&mut self, data: &SkeletonData, entry: EntryId, animation_time: f32, fired: &[FiredEvent]) {
        let e = self.e(entry);
        let (animation_start, animation_end) = (e.animation_start, e.animation_end);
        let duration = animation_end - animation_start;
        let track_last_wrapped = fmod(e.track_last, duration);
        let event_time = |f: &FiredEvent| -> f32 {
            let a = &data.animations[f.animation as usize];
            for t in &a.timelines {
                if let Timeline::Event { events, .. } = t {
                    return events[f.key as usize].time;
                }
            }
            0.0
        };
        let mut i = 0;
        let n = fired.len();
        while i < n {
            let t = event_time(&fired[i]);
            if t < track_last_wrapped {
                break;
            }
            if t > animation_end {
                i += 1;
                continue;
            }
            self.q_event(entry, fired[i]);
            i += 1;
        }
        let e = self.e(entry);
        let complete = if e.looped {
            if duration == 0.0 {
                true
            } else {
                let cycles = floor(e.track_time / duration);
                cycles > 0.0 && cycles > floor(e.track_last / duration)
            }
        } else {
            animation_time >= animation_end && e.animation_last < animation_end
        };
        if complete {
            self.q(StateEventKind::Complete, entry);
        }
        while i < n {
            let t = event_time(&fired[i]);
            if t >= animation_start {
                self.q_event(entry, fired[i]);
            }
            i += 1;
        }
    }

    fn q_event(&mut self, entry: EntryId, f: FiredEvent) {
        let e = self.e(entry);
        self.queue.push(StateEvent {
            kind: StateEventKind::Event { animation: f.animation, key: f.key },
            entry,
            track: e.track_index,
            animation: e.animation,
        });
    }

    // ---- track management ----

    pub fn clear_tracks(&mut self) {
        for i in 0..self.tracks.len() {
            self.clear_track_nodrain(i);
        }
        self.tracks.clear();
        self.drain();
    }

    pub fn clear_track(&mut self, track: usize) {
        self.clear_track_nodrain(track);
        self.drain();
    }

    fn clear_track_nodrain(&mut self, track: usize) {
        let Some(Some(cur)) = self.tracks.get(track).copied() else { return };
        self.q(StateEventKind::End, cur);
        self.clear_next(cur);
        let mut entry = cur;
        while let Some(from) = self.e(entry).mixing_from {
            self.q(StateEventKind::End, from);
            self.em(entry).mixing_from = None;
            self.em(entry).mixing_to = None;
            entry = from;
        }
        let ti = self.e(cur).track_index;
        self.tracks[ti] = None;
    }

    fn set_current(&mut self, _data: &SkeletonData, index: usize, current: EntryId, interrupt: bool) {
        let from = self.expand_to_index(index);
        self.tracks[index] = Some(current);
        self.em(current).previous = None;
        if let Some(from) = from {
            if interrupt {
                self.q(StateEventKind::Interrupt, from);
            }
            self.em(current).mixing_from = Some(from);
            self.em(from).mixing_to = Some(current);
            self.em(current).mix_time = 0.0;
            let (ffrom, fmt, fmd) = {
                let f = self.e(from);
                (f.mixing_from, f.mix_time, f.mix_duration)
            };
            if ffrom.is_some() && fmd > 0.0 {
                self.em(current).interrupt_alpha *= f32::min(1.0, fmt / fmd);
            }
            self.em(from).timelines_rotation.clear();
        }
        self.q(StateEventKind::Start, current);
    }

    pub fn set_animation(&mut self, data: &SkeletonData, track: usize, animation: Option<usize>, looped: bool) -> EntryId {
        let mut interrupt = true;
        let mut current = self.expand_to_index(track);
        if let Some(cur) = current {
            if self.e(cur).next_track_last == -1.0 {
                let mf = self.e(cur).mixing_from;
                self.tracks[track] = mf;
                self.q(StateEventKind::Interrupt, cur);
                self.q(StateEventKind::End, cur);
                self.clear_next(cur);
                current = mf;
                interrupt = false;
            } else {
                self.clear_next(cur);
            }
        }
        let entry = self.track_entry(data, track, animation, looped, current);
        self.set_current(data, track, entry, interrupt);
        self.drain();
        entry
    }

    pub fn add_animation(&mut self, data: &SkeletonData, track: usize, animation: Option<usize>, looped: bool, delay: f32) -> EntryId {
        let mut last = self.expand_to_index(track);
        if let Some(mut l) = last {
            while let Some(n) = self.e(l).next {
                l = n;
            }
            last = Some(l);
        }
        let entry = self.track_entry(data, track, animation, looped, last);
        let mut delay = delay;
        match last {
            None => {
                self.set_current(data, track, entry, true);
                self.drain();
                if delay < 0.0 {
                    delay = 0.0;
                }
            }
            Some(l) => {
                self.em(l).next = Some(entry);
                self.em(entry).previous = Some(l);
                if delay <= 0.0 {
                    let tc = self.e(l).track_complete();
                    let md = self.e(entry).mix_duration;
                    delay = f32::max(delay + tc - md, 0.0);
                }
            }
        }
        self.em(entry).delay = delay;
        entry
    }

    pub fn set_empty_animation(&mut self, data: &SkeletonData, track: usize, mix_duration: f32) -> EntryId {
        let entry = self.set_animation(data, track, None, false);
        let e = self.em(entry);
        e.mix_duration = mix_duration;
        e.track_end = mix_duration;
        entry
    }

    pub fn add_empty_animation(&mut self, data: &SkeletonData, track: usize, mix_duration: f32, delay: f32) -> EntryId {
        let entry = self.add_animation(data, track, None, false, delay);
        let e = self.em(entry);
        if delay <= 0.0 {
            e.delay = f32::max(e.delay + e.mix_duration - mix_duration, 0.0);
        }
        e.mix_duration = mix_duration;
        e.track_end = mix_duration;
        entry
    }

    pub fn set_empty_animations(&mut self, data: &SkeletonData, mix_duration: f32) {
        for i in 0..self.tracks.len() {
            if let Some(cur) = self.tracks[i] {
                let ti = self.e(cur).track_index;
                let entry = self.set_animation_nodrain(data, ti, None, false);
                let e = self.em(entry);
                e.mix_duration = mix_duration;
                e.track_end = mix_duration;
            }
        }
        self.drain();
    }

    fn set_animation_nodrain(&mut self, data: &SkeletonData, track: usize, animation: Option<usize>, looped: bool) -> EntryId {
        let mut interrupt = true;
        let mut current = self.expand_to_index(track);
        if let Some(cur) = current {
            if self.e(cur).next_track_last == -1.0 {
                let mf = self.e(cur).mixing_from;
                self.tracks[track] = mf;
                self.q(StateEventKind::Interrupt, cur);
                self.q(StateEventKind::End, cur);
                self.clear_next(cur);
                current = mf;
                interrupt = false;
            } else {
                self.clear_next(cur);
            }
        }
        let entry = self.track_entry(data, track, animation, looped, current);
        self.set_current(data, track, entry, interrupt);
        entry
    }

    fn expand_to_index(&mut self, index: usize) -> Option<EntryId> {
        if index < self.tracks.len() {
            return self.tracks[index];
        }
        self.tracks.resize(index + 1, None);
        None
    }

    fn track_entry(&mut self, data: &SkeletonData, track: usize, animation: Option<usize>, looped: bool, last: Option<EntryId>) -> EntryId {
        let mix_duration = match last {
            None => 0.0,
            Some(l) => self.data.mix(self.e(l).animation, animation),
        };
        let entry = TrackEntry {
            track_index: track,
            animation,
            looped,
            hold_previous: false,
            reverse: false,
            shortest_rotation: false,
            event_threshold: 0.0,
            alpha_attachment_threshold: 0.0,
            mix_attachment_threshold: 0.0,
            mix_draw_order_threshold: 0.0,
            animation_start: 0.0,
            animation_end: self.duration(data, animation),
            animation_last: -1.0,
            next_animation_last: -1.0,
            delay: 0.0,
            track_time: 0.0,
            track_last: -1.0,
            next_track_last: -1.0,
            track_end: f32::MAX,
            time_scale: 1.0,
            alpha: 1.0,
            mix_time: 0.0,
            mix_duration,
            interrupt_alpha: 1.0,
            total_alpha: 0.0,
            mix_blend: MixBlend::Replace,
            timeline_mode: Vec::new(),
            timeline_hold_mix: Vec::new(),
            timelines_rotation: Vec::new(),
            next: None,
            previous: None,
            mixing_from: None,
            mixing_to: None,
        };
        if let Some(id) = self.free.pop() {
            self.entries[id as usize] = Some(entry);
            id
        } else {
            self.entries.push(Some(entry));
            (self.entries.len() - 1) as EntryId
        }
    }

    fn clear_next(&mut self, entry: EntryId) {
        let mut next = self.e(entry).next;
        while let Some(n) = next {
            self.q(StateEventKind::Dispose, n);
            next = self.e(n).next;
        }
        self.em(entry).next = None;
    }

    fn animations_changed_impl(&mut self, data: &SkeletonData) {
        self.animations_changed = false;
        self.property_ids.clear();
        for i in 0..self.tracks.len() {
            let Some(mut entry) = self.tracks[i] else { continue };
            while let Some(f) = self.e(entry).mixing_from {
                entry = f;
            }
            let mut cur = Some(entry);
            while let Some(e) = cur {
                if self.e(e).mixing_to.is_none() || self.e(e).mix_blend != MixBlend::Add {
                    self.compute_hold(data, e);
                }
                cur = self.e(e).mixing_to;
            }
        }
    }

    /// Adds ids to the property set; true if any was new.
    fn add_all(&mut self, ids: &[u64]) -> bool {
        let mut added = false;
        for id in ids {
            if let Err(pos) = self.property_ids.binary_search(id) {
                self.property_ids.insert(pos, *id);
                added = true;
            }
        }
        added
    }

    fn has_timeline(&self, animation: Option<usize>, ids: &[u64]) -> bool {
        let Some(a) = animation else { return false };
        let set = &self.animation_ids[a];
        ids.iter().any(|id| set.binary_search(id).is_ok())
    }

    fn compute_hold(&mut self, data: &SkeletonData, entry: EntryId) {
        let to = self.e(entry).mixing_to;
        let animation = self.e(entry).animation;
        let timelines: &[Timeline] = animation.map_or(&[], |a| &data.animations[a].timelines[..]);
        let count = timelines.len();
        let mut mode = core::mem::take(&mut self.em(entry).timeline_mode);
        let mut hold = core::mem::take(&mut self.em(entry).timeline_hold_mix);
        mode.clear();
        mode.resize(count, SUBSEQUENT);
        hold.clear();
        hold.resize(count, None);
        let mut ids = core::mem::take(&mut self.scratch_ids);
        if let Some(t) = to {
            if self.e(t).hold_previous {
                for (i, tl) in timelines.iter().enumerate() {
                    ids.clear();
                    property_ids(tl, &mut ids);
                    mode[i] = if self.add_all(&ids) { HOLD_FIRST } else { HOLD_SUBSEQUENT };
                }
                self.scratch_ids = ids;
                let e = self.em(entry);
                e.timeline_mode = mode;
                e.timeline_hold_mix = hold;
                return;
            }
        }
        let entry_md = self.e(entry).mix_duration;
        'outer: for (i, tl) in timelines.iter().enumerate() {
            ids.clear();
            property_ids(tl, &mut ids);
            if !self.add_all(&ids) {
                mode[i] = SUBSEQUENT;
            } else if to.is_none()
                || matches!(tl, Timeline::Attachment { .. } | Timeline::DrawOrder { .. } | Timeline::Event { .. })
                || !self.has_timeline(self.e(to.unwrap()).animation, &ids)
            {
                mode[i] = FIRST;
            } else {
                let mut next = self.e(to.unwrap()).mixing_to;
                while let Some(n) = next {
                    if self.has_timeline(self.e(n).animation, &ids) {
                        next = self.e(n).mixing_to;
                        continue;
                    }
                    if entry_md > 0.0 {
                        mode[i] = HOLD_MIX;
                        hold[i] = Some(n);
                        continue 'outer;
                    }
                    break;
                }
                mode[i] = HOLD_FIRST;
            }
        }
        self.scratch_ids = ids;
        let e = self.em(entry);
        e.timeline_mode = mode;
        e.timeline_hold_mix = hold;
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_rotate_timeline(
    data: &SkeletonData,
    t: &Timeline,
    sk: &mut Skeleton,
    time: f32,
    alpha: f32,
    blend: MixBlend,
    rot: &mut [f32],
    i: usize,
    first_frame: bool,
) {
    if first_frame {
        rot[i] = 0.0;
    }
    if alpha == 1.0 {
        apply_timeline(t, data, sk, 0.0, time, None, 1.0, blend, MixDirection::In);
        return;
    }
    let Timeline::Bone { bone, curve, .. } = t else { return };
    let bi = *bone as usize;
    if !sk.bones[bi].active {
        return;
    }
    let setup = data.bones[bi].rotation;
    let b = &mut sk.bones[bi];
    let (r1, r2);
    if time < curve.frames[0] {
        match blend {
            MixBlend::Setup => {
                b.rotation = setup;
                return;
            }
            MixBlend::First => {
                r1 = b.rotation;
                r2 = setup;
            }
            _ => return,
        }
    } else {
        r1 = if blend == MixBlend::Setup { setup } else { b.rotation };
        r2 = setup + curve.curve_value(time);
    }
    let mut diff = r2 - r1;
    diff -= ceil(diff / 360.0 - 0.5) * 360.0;
    let total;
    if diff == 0.0 {
        total = rot[i];
    } else {
        let (last_total, last_diff) = if first_frame { (0.0, diff) } else { (rot[i], rot[i + 1]) };
        let loops = last_total - fmod(last_total, 360.0);
        let mut tot = diff + loops;
        let current = diff >= 0.0;
        let mut dir = last_total >= 0.0;
        if abs(last_diff) <= 90.0 && signum(last_diff) != signum(diff) {
            if abs(last_total - loops) > 180.0 {
                tot += 360.0 * signum(last_total);
                dir = current;
            } else if loops != 0.0 {
                tot -= 360.0 * signum(last_total);
            } else {
                dir = current;
            }
        }
        if dir != current {
            tot += 360.0 * signum(last_total);
        }
        rot[i] = tot;
        total = tot;
    }
    rot[i + 1] = diff;
    b.rotation = r1 + total * alpha;
}

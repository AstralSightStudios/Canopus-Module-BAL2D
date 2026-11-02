//! The memorial-lobby player shared by the watch and the host tools.
//!
//! A `Lobby` owns the skeletons of a pack (background layers first), plays the intro then the
//! idle loop, and turns touches into the lobby interactions: a tap plays the next talk, a
//! press-and-drag inside a drag box moves that drag's bone (head pat, eye follow, pinch) while
//! its "ing" clips play, and releasing plays the end clips while the bone springs back.
#![no_std]

extern crate alloc;

pub mod idle;

use alloc::string::String;
use alloc::vec::Vec;

use bal2d_pack::{Drag, Index, Interactions, PageHeader, Source};
use bal2d_render::DrawList;
use bal2d_spine::math::{clamp, pow};
use bal2d_spine::state::{AnimationState, AnimationStateData, StateEventKind};
use bal2d_spine::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub cx: f32,
    pub cy: f32,
    pub scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    Down,
    Move,
    Up,
}

/// A page in the pack, not yet decoded: the caller inflates it straight into its own buffer
/// with `read_page` (the section is the 8-byte page header, the Index8 palette, then pixels).
#[derive(Clone, Copy, Debug)]
pub struct Page {
    pub layer: u16,
    pub index: u16,
    pub header: PageHeader,
    pub entry: bal2d_pack::Entry,
}

impl Page {
    /// Bytes before the pixels in the section: header, plus the palette for Index8.
    pub fn prefix(&self) -> usize {
        8 + if self.header.format == bal2d_pack::PageFormat::Index8 { 1024 } else { 0 }
    }

    pub fn section_len(&self) -> usize {
        self.entry.raw_len as usize
    }
}

/// Inflates a page section into `out` (exactly `page.section_len()` bytes).
/// The baked static bottom of the scene (see the compiler's `bake`): an opaque BGR565 image at
/// (x, y) of the pack's screen, drawn before the layers.
#[derive(Clone, Debug)]
pub struct Bake {
    pub header: bal2d_pack::BakeHeader,
    pub entry: bal2d_pack::Entry,
    /// The intro moves baked slots: the image replaces them only once the idle loop has settled.
    pub after_intro: bool,
    /// Per layer, per slot: replaced by the image.
    pub masks: Vec<Vec<bool>>,
}

impl Bake {
    /// Bytes before the pixels in the section.
    pub const PREFIX: usize = 8;

    pub fn section_len(&self) -> usize {
        self.entry.raw_len as usize
    }
}

pub fn read_page(src: &mut dyn Source, page: &Page, out: &mut [u8]) -> Result<(), bal2d_pack::PackError> {
    bal2d_pack::read_section(src, &page.entry, out)
}

pub struct Layer {
    pub data: SkeletonData,
    pub skeleton: Skeleton,
    pub state: AnimationState,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DragPhase {
    /// Finger down, waiting for the trigger delay (a release now is a tap).
    Pressing,
    Dragging,
    Releasing,
}

#[derive(Clone, Debug)]
struct ActiveDrag {
    /// The camera at the press: the finger maps through it for the whole drag.
    cam: Camera,
    drag: usize,
    bone: usize,
    phase: DragPhase,
    elapsed: f32,
    /// Finger position at press, skeleton world units.
    start: (f32, f32),
    finger: (f32, f32),
    /// Bone setup local position and current overridden position.
    origin: (f32, f32),
    pos: (f32, f32),
}

/// Something the platform may want to react to (voice playback, subtitles).
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    /// A keyed Spine event fired (name, string value, audio path).
    Event { name: String, string: Option<String>, audio: Option<String> },
    TalkStarted(usize),
}

/// How long the framing takes to move to another phase's camera (seconds).
pub const CAMERA_BLEND_S: f32 = 0.6;

pub struct Lobby {
    pub layers: Vec<Layer>,
    pub interactions: Interactions,
    /// The framing now: the current phase's camera, or on the way to it (see `cameras`).
    pub camera: Camera,
    /// Each phase's camera (`bal2d_pack::phase`); phases a pack does not frame use the idle one.
    pub cameras: [Camera; bal2d_pack::phase::COUNT],
    /// The camera blend: from where, to which phase, how far (0..=1).
    cam_from: Camera,
    cam_phase: u8,
    cam_t: f32,
    /// A talk or a drag the lobby is busy with (its phase), until both tracks are free again.
    activity: Option<u8>,
    pub width: f32,
    pub height: f32,
    pub name: String,
    talk_next: usize,
    drag: Option<ActiveDrag>,
    pending_tap: bool,
    pub notices: Vec<Notice>,
    /// Total animation time (seconds).
    pub time: f32,
    pub bake: Option<Bake>,
    /// Per layer: its idle loop baked into GPU calls (`idle`), once loaded (`load_idle`).
    pub idle: Vec<Option<idle::IdleLayer>>,
    /// Per layer: the pack section of its baked idle, if it has one.
    pub idle_entries: Vec<Option<bal2d_pack::Entry>>,
    /// Per layer: time its baked idle has played that its skeleton has not (`update_with`).
    idle_pending: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    Pack(bal2d_pack::PackError),
    Skeleton(u16, ReadError),
    MissingSection(&'static str, u16),
    Atlas(u16, String),
    Interactions,
}

impl Lobby {
    /// Parses a pack held in memory (host tools), its baked idle included.
    pub fn load(bytes: &[u8]) -> Result<(Lobby, Vec<Page>), LoadError> {
        let mut src: &[u8] = bytes;
        let (mut lobby, pages) = Self::load_from(&mut src)?;
        for li in 0..lobby.layers.len() {
            lobby.load_idle(&mut src, li);
        }
        Ok((lobby, pages))
    }

    /// Reads layer `layer`'s baked idle from the pack; false if it has none, it cannot be read
    /// (memory included) or it does not fit the skeleton (the layer is then animated).
    pub fn load_idle(&mut self, src: &mut dyn Source, layer: usize) -> bool {
        let Some(Some(e)) = self.idle_entries.get(layer) else { return false };
        let Ok(d) = bal2d_pack::read_section_vec(src, e) else { return false };
        let Some(bake) = bal2d_pack::IdleBake::decode_owned(d) else { return false };
        self.idle[layer] = idle::IdleLayer::new(bake, &self.layers[layer].data);
        self.idle[layer].is_some()
    }

    /// Parses a pack from a random-access source. Only the skeletons, atlases and tables are
    /// read; pages are listed so the caller can inflate each straight into its own buffer
    /// with `read_page`.
    pub fn load_from(src: &mut dyn Source) -> Result<(Lobby, Vec<Page>), LoadError> {
        let idx = Index::read(src).map_err(LoadError::Pack)?;
        let h = idx.header;
        let mut layers = Vec::new();
        let mut pages = Vec::new();
        for li in 0..h.layer_count as u16 {
            let e = idx.find(bal2d_pack::KIND_SKEL, li, 0).ok_or(LoadError::MissingSection("skeleton", li))?;
            let buf = bal2d_pack::read_section_vec(src, e).map_err(LoadError::Pack)?;
            let mut data = read_skeleton(&buf, 1.0).map_err(|e| LoadError::Skeleton(li, e))?;
            drop(buf);
            let e = idx.find(bal2d_pack::KIND_ATLAS, li, 0).ok_or(LoadError::MissingSection("atlas", li))?;
            let buf = bal2d_pack::read_section_vec(src, e).map_err(LoadError::Pack)?;
            let atlas = Atlas::parse(core::str::from_utf8(&buf).unwrap_or(""));
            drop(buf);
            let missing = bind_atlas(&mut data, &atlas);
            if let Some(m) = missing.into_iter().next() {
                return Err(LoadError::Atlas(li, m));
            }
            for pi in 0..idx.count(bal2d_pack::KIND_PAGE, li) as u16 {
                let e = *idx.find(bal2d_pack::KIND_PAGE, li, pi).ok_or(LoadError::MissingSection("page", li))?;
                let mut head = [0u8; 8];
                bal2d_pack::read_section_prefix(src, &e, &mut head).map_err(LoadError::Pack)?;
                let header = bal2d_pack::page_header(&head).ok_or(LoadError::MissingSection("page header", li))?;
                pages.push(Page { layer: li, index: pi, header, entry: e });
            }
            let mut skeleton = Skeleton::new(&data);
            skeleton.update_world_transform(&data, Physics::Update);
            let state = AnimationState::new(&data, AnimationStateData { default_mix: 0.25, mixes: Vec::new() });
            layers.push(Layer { data, skeleton, state });
        }
        let interactions = match idx.find(bal2d_pack::KIND_INTERACTIONS, 0, 0) {
            Some(e) => Interactions::decode(&bal2d_pack::read_section_vec(src, e).map_err(LoadError::Pack)?).ok_or(LoadError::Interactions)?,
            None => Interactions::default(),
        };
        let bake = match idx.find(bal2d_pack::KIND_BAKE, 0, 0) {
            Some(e) => {
                let mut head = [0u8; 8];
                bal2d_pack::read_section_prefix(src, e, &mut head).map_err(LoadError::Pack)?;
                let header = bal2d_pack::bake_header(&head).ok_or(LoadError::MissingSection("bake header", 0))?;
                if e.raw_len as usize != 8 + header.width as usize * header.height as usize * 2 {
                    return Err(LoadError::MissingSection("bake pixels", 0));
                }
                let list = match idx.find(bal2d_pack::KIND_BAKE_SLOTS, 0, 0) {
                    Some(e) => bal2d_pack::BakeSlots::decode(&bal2d_pack::read_section_vec(src, e).map_err(LoadError::Pack)?)
                        .ok_or(LoadError::MissingSection("bake slots", 0))?,
                    None => bal2d_pack::BakeSlots::default(),
                };
                let mut masks: Vec<Vec<bool>> = layers.iter().map(|l: &Layer| alloc::vec![false; l.data.slots.len()]).collect();
                for (l, slot) in list.slots {
                    if let Some(m) = masks.get_mut(l as usize).and_then(|m| m.get_mut(slot as usize)) {
                        *m = true;
                    }
                }
                Some(Bake { header, entry: *e, after_intro: list.after_intro, masks })
            }
            None => None,
        };
        let base = Camera { cx: h.camera_cx, cy: h.camera_cy, scale: h.camera_scale };
        let mut cameras = [base; bal2d_pack::phase::COUNT];
        if let Some(e) = idx.find(bal2d_pack::KIND_CAMERAS, 0, 0) {
            let list = bal2d_pack::PhaseCameras::decode(&bal2d_pack::read_section_vec(src, e).map_err(LoadError::Pack)?)
                .ok_or(LoadError::MissingSection("cameras", 0))?;
            for (p, cx, cy, scale) in list.cameras {
                if let Some(c) = cameras.get_mut(p as usize) {
                    *c = Camera { cx, cy, scale };
                }
            }
        }
        let name = match idx.find(bal2d_pack::KIND_NAME, 0, 0) {
            Some(e) => String::from_utf8(bal2d_pack::read_section_vec(src, e).map_err(LoadError::Pack)?).unwrap_or_default(),
            None => String::new(),
        };
        // The baked idle is read on request (`load_idle`): the watch reads it last, if memory
        // is left once everything else is in.
        let idle_entries: Vec<Option<bal2d_pack::Entry>> = (0..layers.len()).map(|li| idx.find(bal2d_pack::KIND_IDLE, li as u16, 0).copied()).collect();
        let idle = (0..layers.len()).map(|_| None).collect();
        let layers_len = layers.len();
        let mut lobby = Lobby {
            layers,
            interactions,
            camera: base,
            cameras,
            cam_from: base,
            cam_phase: bal2d_pack::phase::IDLE,
            cam_t: 1.0,
            activity: None,
            width: h.screen_width as f32,
            height: h.screen_height as f32,
            name,
            talk_next: 0,
            drag: None,
            pending_tap: false,
            notices: Vec::new(),
            time: 0.0,
            bake,
            idle_pending: alloc::vec![0.0; layers_len],
            idle,
            idle_entries,
        };
        lobby.start();
        Ok((lobby, pages))
    }

    fn main(&self) -> usize {
        (self.interactions.layer as usize).min(self.layers.len().saturating_sub(1))
    }

    /// Intro then idle on the main layer; other layers loop their own idle.
    pub fn start(&mut self) {
        let main = self.main();
        let default_mix = self.interactions.default_mix;
        for (i, l) in self.layers.iter_mut().enumerate() {
            l.state.data.default_mix = default_mix;
            let d = &l.data;
            if i == main {
                let intro = d.find_animation(&self.interactions.intro);
                let idle = d.find_animation(&self.interactions.idle);
                match intro {
                    Some(_) => {
                        l.state.set_animation(d, 0, intro, false);
                        if idle.is_some() {
                            l.state.add_animation(d, 0, idle, true, 0.0);
                        }
                    }
                    None if idle.is_some() => {
                        l.state.set_animation(d, 0, idle, true);
                    }
                    None => {}
                }
            } else {
                let idle = d
                    .animations
                    .iter()
                    .position(|a| a.name.starts_with("Idle"))
                    .or_else(|| d.animations.iter().position(|a| a.name != "Dummy" && a.duration > 0.0));
                if idle.is_some() {
                    l.state.set_animation(d, 0, idle, true);
                }
            }
        }
        self.time = 0.0;
        self.idle_pending.iter_mut().for_each(|p| *p = 0.0);
        self.talk_next = 0;
        self.drag = None;
        self.activity = None;
        let phase = self.phase();
        self.camera = self.cameras[phase as usize];
        self.cam_from = self.camera;
        self.cam_phase = phase;
        self.cam_t = 1.0;
    }

    /// The phase the lobby is in, for its framing (`bal2d_pack::phase`).
    pub fn phase(&self) -> u8 {
        if let Some(a) = self.activity {
            return a;
        }
        let l = &self.layers[self.main()];
        let intro = l.data.find_animation(&self.interactions.intro);
        let playing = l.state.current(0).map(|e| l.state.entry(e).animation);
        if intro.is_some() && playing == Some(intro) { bal2d_pack::phase::INTRO } else { bal2d_pack::phase::IDLE }
    }

    /// Moves the framing towards the current phase's camera (`CAMERA_BLEND_S`, eased). The view
    /// size (1 / scale) and the centre move linearly: no powers (a software double on the watch).
    fn update_camera(&mut self, dt: f32) {
        if self.activity.is_some() && self.drag.is_none() && self.settled() {
            self.activity = None;
        }
        let phase = self.phase();
        let target = self.cameras[phase as usize];
        if phase != self.cam_phase {
            self.cam_phase = phase;
            if target != self.camera {
                self.cam_from = self.camera;
                self.cam_t = 0.0;
            }
        }
        if self.cam_t >= 1.0 {
            self.camera = target;
            return;
        }
        self.cam_t = (self.cam_t + dt / CAMERA_BLEND_S).min(1.0);
        let t = self.cam_t * self.cam_t * (3.0 - 2.0 * self.cam_t);
        let (a, b) = (self.cam_from, target);
        let size = 1.0 / a.scale + (1.0 / b.scale - 1.0 / a.scale) * t;
        self.camera = if self.cam_t >= 1.0 {
            b
        } else {
            Camera { cx: a.cx + (b.cx - a.cx) * t, cy: a.cy + (b.cy - a.cy) * t, scale: 1.0 / size }
        };
    }

    /// True once the intro has finished and the idle loop plays on track 0.
    pub fn is_idle(&self) -> bool {
        let l = &self.layers[self.main()];
        let idle = l.data.find_animation(&self.interactions.idle);
        l.state.current(0).is_some_and(|e| l.state.entry(e).animation == idle)
    }

    /// Nothing playing or queued on tracks 1 and 2 (`is_free` also counts a track whose clip is
    /// still queued behind an empty mix-in as free).
    fn settled(&self) -> bool {
        let l = &self.layers[self.main()];
        [1, 2].iter().all(|&t| match l.state.current(t) {
            None => true,
            Some(e) => {
                let e = l.state.entry(e);
                e.next.is_none() && (e.animation.is_none() || (!e.looped && e.is_complete()))
            }
        })
    }

    /// No talk / drag clip is playing on tracks 1 and 2.
    pub fn is_free(&self) -> bool {
        let l = &self.layers[self.main()];
        [1, 2].iter().all(|&t| match l.state.current(t) {
            None => true,
            Some(e) => {
                let e = l.state.entry(e);
                e.animation.is_none() || (!e.looped && e.is_complete() && e.next.is_none())
            }
        })
    }

    pub fn screen_to_world(&self, x: f32, y: f32) -> (f32, f32) {
        self.screen_to_world_with(self.camera, x, y)
    }

    fn screen_to_world_with(&self, c: Camera, x: f32, y: f32) -> (f32, f32) {
        ((x - self.width * 0.5) / c.scale + c.cx, c.cy - (y - self.height * 0.5) / c.scale)
    }

    pub fn world_to_screen(&self, x: f32, y: f32) -> (f32, f32) {
        let c = self.camera;
        ((x - c.cx) * c.scale + self.width * 0.5, self.height * 0.5 - (y - c.cy) * c.scale)
    }

    /// Plays clip `m` on track 1 and `a` on track 2 with the clip mixes, then mixes out.
    fn play_pair(&mut self, m: &str, a: &str, looped: bool) {
        let main = self.main();
        let inter = &self.interactions;
        let l = &mut self.layers[main];
        for (track, clip) in [(1usize, m), (2usize, a)] {
            let anim = l.data.find_animation(clip);
            if anim.is_none() {
                continue;
            }
            l.state.set_empty_animation(&l.data, track, 0.0);
            let e = l.state.add_animation(&l.data, track, anim, looped, 0.0);
            l.state.entry_mut(e).mix_duration = inter.intro_mix(clip);
            if !looped {
                l.state.add_empty_animation(&l.data, track, inter.outro_mix(clip), 0.0);
            }
        }
    }

    pub fn tap(&mut self) {
        if !self.is_idle() || !self.is_free() || self.interactions.talks.is_empty() {
            return;
        }
        let i = self.talk_next % self.interactions.talks.len();
        self.talk_next = i + 1;
        let (m, a) = self.interactions.talks[i].clone();
        self.play_pair(&m, &a, false);
        self.activity = Some(bal2d_pack::phase::TALK);
        self.notices.push(Notice::TalkStarted(i));
    }

    fn drag_hit(&self, world: (f32, f32)) -> Option<usize> {
        let l = &self.layers[self.main()];
        for (i, d) in self.interactions.drags.iter().enumerate() {
            let Some(b) = l.data.find_bone(&d.bone) else { continue };
            if d.box_w <= 0.0 || d.box_h <= 0.0 {
                return Some(i);
            }
            let bone = &l.skeleton.bones[b];
            if (world.0 - bone.world_x).abs() <= d.box_w * 0.5 && (world.1 - bone.world_y).abs() <= d.box_h * 0.5 {
                return Some(i);
            }
        }
        None
    }

    /// Whether a press at screen coordinates is on a drag area (head pat and the like), busy or
    /// not: the host then keeps the gesture for itself instead of letting the system scroll or
    /// swipe, so a pat never turns the watchface page.
    pub fn grabs(&self, x: f32, y: f32) -> bool {
        // Boxless drags (eye follow) take the whole screen: they must not hold system gestures.
        let world = self.screen_to_world(x, y);
        let l = &self.layers[self.main()];
        self.interactions.drags.iter().any(|d| {
            d.box_w > 0.0
                && d.box_h > 0.0
                && l.data.find_bone(&d.bone).is_some_and(|b| {
                    let bone = &l.skeleton.bones[b];
                    (world.0 - bone.world_x).abs() <= d.box_w * 0.5 && (world.1 - bone.world_y).abs() <= d.box_h * 0.5
                })
        })
    }

    /// The screen rectangle (x, y, w, h) covering the boxed drag areas at the current pose.
    pub fn grab_box(&self) -> Option<(f32, f32, f32, f32)> {
        let l = &self.layers[self.main()];
        let mut r: Option<(f32, f32, f32, f32)> = None;
        for d in &self.interactions.drags {
            if d.box_w <= 0.0 || d.box_h <= 0.0 {
                continue;
            }
            let Some(b) = l.data.find_bone(&d.bone) else { continue };
            let bone = &l.skeleton.bones[b];
            let (cx, cy) = self.world_to_screen(bone.world_x, bone.world_y);
            let (hw, hh) = (d.box_w * 0.5 * self.camera.scale, d.box_h * 0.5 * self.camera.scale);
            let b = (cx - hw, cy - hh, cx + hw, cy + hh);
            r = Some(match r {
                None => b,
                Some(a) => (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)),
            });
        }
        r.map(|(x0, y0, x1, y1)| (x0, y0, x1 - x0, y1 - y0))
    }

    /// Feeds a touch at screen coordinates.
    pub fn touch(&mut self, kind: Touch, x: f32, y: f32) {
        // A drag follows the finger through the camera it started with: the framing may move to
        // the touch phase's camera meanwhile.
        let cam = self.drag.as_ref().filter(|_| kind != Touch::Down).map_or(self.camera, |d| d.cam);
        let world = self.screen_to_world_with(cam, x, y);
        match kind {
            Touch::Down => {
                if !self.is_idle() {
                    return;
                }
                if self.drag.as_ref().is_some_and(|d| d.phase != DragPhase::Releasing) {
                    return;
                }
                let main = self.main();
                match self.drag_hit(world) {
                    Some(i) => {
                        let bone = self.layers[main].data.find_bone(&self.interactions.drags[i].bone).unwrap();
                        let bd = &self.layers[main].data.bones[bone];
                        let origin = (bd.x, bd.y);
                        let pos = match &self.drag {
                            Some(d) if d.bone == bone => d.pos,
                            _ => origin,
                        };
                        self.drag = Some(ActiveDrag {
                            cam: self.camera,
                            drag: i,
                            bone,
                            phase: DragPhase::Pressing,
                            elapsed: 0.0,
                            start: world,
                            finger: world,
                            origin,
                            pos,
                        });
                    }
                    None => {
                        // Outside every drag box: the press is only a potential tap.
                        self.drag = None;
                        self.pending_tap = true;
                    }
                }
            }
            Touch::Move => {
                if let Some(d) = &mut self.drag {
                    if d.phase != DragPhase::Releasing {
                        d.finger = world;
                    }
                }
            }
            Touch::Up => {
                let tap = self.pending_tap;
                self.pending_tap = false;
                let Some(d) = self.drag.clone() else {
                    if tap {
                        self.tap();
                    }
                    return;
                };
                match d.phase {
                    DragPhase::Pressing => {
                        // Released before the trigger delay: a tap.
                        self.drag = if d.pos != d.origin { Some(ActiveDrag { phase: DragPhase::Releasing, ..d }) } else { None };
                        self.tap();
                    }
                    DragPhase::Dragging => {
                        let drag = self.interactions.drags[d.drag].clone();
                        self.play_pair(&drag.end_m, &drag.end_a, false);
                        self.drag = Some(ActiveDrag { phase: DragPhase::Releasing, ..d });
                    }
                    DragPhase::Releasing => {}
                }
            }
        }
    }

    /// Advances the drag: trigger delay, finger following, spring-back.
    fn update_drag(&mut self, dt: f32) {
        let Some(mut d) = self.drag.clone() else { return };
        let drag: Drag = self.interactions.drags[d.drag].clone();
        d.elapsed += dt;
        if d.phase == DragPhase::Pressing && d.elapsed >= drag.delay {
            d.phase = DragPhase::Dragging;
            self.play_pair(&drag.ing_m, &drag.ing_a, true);
            self.activity = Some(bal2d_pack::phase::TOUCH);
        }
        let main = self.main();
        let target = match d.phase {
            DragPhase::Dragging => {
                // Finger travel in the bone's parent space.
                let l = &self.layers[main];
                let parent = l.data.bones[d.bone].parent.map(|p| &l.skeleton.bones[p as usize]);
                let (dx, dy) = (d.finger.0 - d.start.0, d.finger.1 - d.start.1);
                let (lx, ly) = match parent {
                    Some(p) => {
                        let det = p.a * p.d - p.b * p.c;
                        if det.abs() < 1e-6 { (dx, dy) } else { ((dx * p.d - dy * p.b) / det, (dy * p.a - dx * p.c) / det) }
                    }
                    None => (dx, dy),
                };
                let tx = clamp(d.origin.0 + lx, drag.min_x.min(drag.max_x), drag.max_x.max(drag.min_x));
                let ty = clamp(d.origin.1 + ly, drag.min_y.min(drag.max_y), drag.max_y.max(drag.min_y));
                (tx, ty)
            }
            _ => d.origin,
        };
        let speed = if d.phase == DragPhase::Releasing { drag.release } else { drag.follow };
        // The prefab speeds are per 1/60 s frame; make them frame-rate independent.
        let k = 1.0 - pow(clamp(1.0 - speed, 0.0, 1.0), dt * 60.0);
        d.pos.0 += (target.0 - d.pos.0) * k;
        d.pos.1 += (target.1 - d.pos.1) * k;
        let done = d.phase == DragPhase::Releasing && (d.pos.0 - d.origin.0).abs() < 0.05 && (d.pos.1 - d.origin.1).abs() < 0.05;
        self.drag = if done { None } else { Some(d) };
    }

    /// Advances animation by `dt` seconds and poses every layer.
    #[inline(never)]
    pub fn update(&mut self, dt: f32) {
        self.update_with(dt, false);
    }

    /// `update`, with the layers that only play their baked idle loop left to their bakes:
    /// their clocks move, their skeletons catch up only when they animate again. Returns the
    /// layers to draw from their bakes this frame (bit = layer; see `draw_idle`).
    #[inline(never)]
    pub fn update_with(&mut self, dt: f32, use_bakes: bool) -> u32 {
        self.time += dt;
        self.update_drag(dt);
        self.update_camera(dt);
        let baked: u32 = if use_bakes { (0..self.layers.len().min(32)).filter(|&i| self.layer_idle_ready(i)).fold(0, |m, i| m | 1 << i) } else { 0 };
        let main = self.main();
        let drag = self.drag.as_ref().map(|d| (d.bone, d.pos));
        for i in 0..self.layers.len() {
            // A bake with live slots still needs the pose (for those).
            if baked & (1 << i.min(31)) != 0 && !self.idle[i].as_ref().is_some_and(|b| b.needs_pose()) {
                self.idle_pending[i] += dt;
                continue;
            }
            self.resume_layer(i);
            let l = &mut self.layers[i];
            l.state.update(&l.data, dt);
            l.state.apply(&l.data, &mut l.skeleton);
            if i == main {
                if let Some((b, (x, y))) = drag {
                    l.skeleton.bones[b].x = x;
                    l.skeleton.bones[b].y = y;
                }
            }
            l.skeleton.update(dt);
            l.skeleton.update_world_transform(&l.data, Physics::Update);
            for ev in l.state.output.drain(..) {
                if let StateEventKind::Event { animation, key } = ev.kind {
                    for t in &l.data.animations[animation as usize].timelines {
                        if let Timeline::Event { events, .. } = t {
                            let k = &events[key as usize];
                            let ed = &l.data.events[k.data as usize];
                            self.notices.push(Notice::Event {
                                name: ed.name.clone(),
                                string: k.string_value.clone(),
                                audio: ed.audio_path.clone(),
                            });
                        }
                    }
                }
            }
        }
        baked
    }

    /// Whether layer `layer` plays only its idle loop, as its bake has it: the looping animation
    /// on track 0 unmixed, nothing else playing or queued (on the main layer: no talk, drag or
    /// finger either), and the framing is the idle one (with the baked image in place when the
    /// pack has one).
    pub fn layer_idle_ready(&self, layer: usize) -> bool {
        let (Some(l), Some(Some(b))) = (self.layers.get(layer), self.idle.get(layer)) else { return false };
        if self.cam_t < 1.0 || self.camera != self.cameras[bal2d_pack::phase::IDLE as usize] {
            return false;
        }
        if self.bake.is_some() && !self.bake_active() {
            return false;
        }
        if layer == self.main() && (self.drag.is_some() || self.activity.is_some() || self.pending_tap) {
            return false;
        }
        let Some(e) = l.state.current(0) else { return false };
        let e = l.state.entry(e);
        if e.animation != Some(b.bake.animation as usize) || !e.looped || e.mixing_from.is_some() || e.next.is_some() {
            return false;
        }
        (1..4).all(|t| match l.state.current(t) {
            None => true,
            Some(e) => {
                let e = l.state.entry(e);
                e.animation.is_none() && e.mixing_from.is_none() && e.next.is_none()
            }
        })
    }

    /// Every layer can be drawn from its bake (`layer_idle_ready`).
    pub fn idle_ready(&self) -> bool {
        !self.layers.is_empty() && (0..self.layers.len()).all(|i| self.layer_idle_ready(i))
    }

    /// Brings every skeleton to where its baked idle has played to (before animating again).
    pub fn resume(&mut self) {
        for i in 0..self.layers.len() {
            self.resume_layer(i);
        }
    }

    fn resume_layer(&mut self, i: usize) {
        let Some(p) = self.idle_pending.get_mut(i) else { return };
        if *p <= 0.0 {
            return;
        }
        let dt = core::mem::take(p);
        let l = &mut self.layers[i];
        l.state.update(&l.data, dt);
        l.state.apply(&l.data, &mut l.skeleton);
        // Physics takes the step a frame would: a long one would throw it.
        l.skeleton.update(dt.min(1.0 / 30.0));
        l.skeleton.update_world_transform(&l.data, Physics::Update);
        l.state.output.clear();
    }

    /// Draws layer `layer` from its baked idle (see `layer_idle_ready`) into a target of `size`
    /// whose top-left is `origin` on the pack's screen. Returns the triangles drawn.
    pub fn draw_idle(&mut self, layer: usize, origin: (f32, f32), size: (f32, f32), page_size: &dyn Fn(u32) -> Option<(f32, f32)>, emit: &mut dyn FnMut(&bal2d_render::Call)) -> u32 {
        let pending = self.idle_pending.get(layer).copied().unwrap_or(0.0);
        let c = self.camera;
        let cam = (c.cx + (origin.0 + size.0 * 0.5 - self.width * 0.5) / c.scale, c.cy - (origin.1 + size.1 * 0.5 - self.height * 0.5) / c.scale, c.scale);
        let (Some(l), Some(Some(b))) = (self.layers.get(layer), self.idle.get_mut(layer)) else { return 0 };
        let Some(e) = l.state.current(0) else { return 0 };
        let tau = idle::wrap(l.state.entry(e).track_time + pending - b.bake.start, b.bake.period);
        b.draw(&l.data, &l.skeleton, cam, tau, origin, size, page_size, emit)
    }

    /// Whether the baked image stands in for its slots now: always, or once the intro is over
    /// and the idle loop plays unmixed.
    pub fn bake_active(&self) -> bool {
        // The image is composited through the idle framing.
        if self.camera != self.cameras[bal2d_pack::phase::IDLE as usize] {
            return false;
        }
        match &self.bake {
            None => false,
            Some(b) if !b.after_intro => true,
            Some(_) => {
                let l = &self.layers[self.main()];
                self.is_idle() && l.state.current(0).is_some_and(|e| l.state.entry(e).mixing_from.is_none())
            }
        }
    }

    /// The layer's draw list; the baked slots are left out while the image stands in for them.
    #[inline(never)]
    pub fn draw_list(&self, layer: usize, out: &mut DrawList) {
        let l = &self.layers[layer];
        let mask = self.bake.as_ref().filter(|_| self.bake_active()).and_then(|b| b.masks.get(layer)).map(|m| &m[..]);
        bal2d_render::build_masked(&l.data, &l.skeleton, out, mask);
    }
}

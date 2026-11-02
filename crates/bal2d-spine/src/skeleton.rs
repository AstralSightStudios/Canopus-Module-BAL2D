//! Skeleton instance: bone and slot poses, constraint state, the update order and world
//! transforms. The immutable `SkeletonData` is passed to every call that needs it, so a
//! skeleton can live next to its data without self-references.

use alloc::vec::Vec;

use crate::data::*;
use crate::math::*;

/// How physics constraints are updated by `update_world_transform`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Physics {
    /// Physics are not updated or applied.
    None,
    /// Physics are reset to the current pose.
    Reset,
    /// Physics are updated and the pose from physics is applied.
    Update,
    /// Physics are not updated but the pose from physics is applied.
    Pose,
}

#[derive(Clone, Debug)]
pub struct Bone {
    pub parent: Option<u16>,
    pub children: Vec<u16>,
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub shear_x: f32,
    pub shear_y: f32,
    pub inherit: Inherit,
    pub ax: f32,
    pub ay: f32,
    pub arotation: f32,
    pub ascale_x: f32,
    pub ascale_y: f32,
    pub ashear_x: f32,
    pub ashear_y: f32,
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub world_x: f32,
    pub world_y: f32,
    pub sorted: bool,
    pub active: bool,
}

impl Bone {
    pub fn world_rotation_x(&self) -> f32 {
        atan2(self.c, self.a) * RAD_DEG
    }

    pub fn world_scale_x(&self) -> f32 {
        sqrt(self.a * self.a + self.c * self.c)
    }

    pub fn world_scale_y(&self) -> f32 {
        sqrt(self.b * self.b + self.d * self.d)
    }

    pub fn world_to_local(&self, x: f32, y: f32) -> (f32, f32) {
        let inv = 1.0 / (self.a * self.d - self.b * self.c);
        let (x, y) = (x - self.world_x, y - self.world_y);
        (x * self.d * inv - y * self.b * inv, y * self.a * inv - x * self.c * inv)
    }

    pub fn local_to_world(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.a + y * self.b + self.world_x, x * self.c + y * self.d + self.world_y)
    }
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub bone: u16,
    pub color: Color,
    pub dark: Option<Color>,
    pub attachment: Option<u32>,
    pub sequence_index: i32,
    /// Deform values for the current vertex attachment (empty: none).
    pub deform: Vec<f32>,
}

#[derive(Clone, Debug)]
pub struct IkConstraint {
    pub mix: f32,
    pub softness: f32,
    pub bend_direction: i8,
    pub compress: bool,
    pub stretch: bool,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct TransformConstraint {
    pub mix_rotate: f32,
    pub mix_x: f32,
    pub mix_y: f32,
    pub mix_scale_x: f32,
    pub mix_scale_y: f32,
    pub mix_shear_y: f32,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct PathConstraint {
    pub position: f32,
    pub spacing: f32,
    pub mix_rotate: f32,
    pub mix_x: f32,
    pub mix_y: f32,
    pub active: bool,
    spaces: Vec<f32>,
    positions: Vec<f32>,
    world: Vec<f32>,
    curves: Vec<f32>,
    lengths: Vec<f32>,
    segments: [f32; 10],
}

#[derive(Clone, Debug, Default)]
pub struct PhysicsConstraint {
    pub inertia: f32,
    pub strength: f32,
    pub damping: f32,
    pub mass_inverse: f32,
    pub wind: f32,
    pub gravity: f32,
    pub mix: f32,
    pub active: bool,
    reset: bool,
    ux: f32,
    uy: f32,
    cx: f32,
    cy: f32,
    tx: f32,
    ty: f32,
    x_offset: f32,
    x_velocity: f32,
    y_offset: f32,
    y_velocity: f32,
    rotate_offset: f32,
    rotate_velocity: f32,
    scale_offset: f32,
    scale_velocity: f32,
    remaining: f32,
    last_time: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Update {
    Bone(u16),
    Ik(u16),
    Transform(u16),
    Path(u16),
    Physics(u16),
}

#[derive(Clone, Debug)]
pub struct Skeleton {
    pub bones: Vec<Bone>,
    pub slots: Vec<Slot>,
    pub draw_order: Vec<u16>,
    pub ik: Vec<IkConstraint>,
    pub transform: Vec<TransformConstraint>,
    pub path: Vec<PathConstraint>,
    pub physics: Vec<PhysicsConstraint>,
    update_cache: Vec<Update>,
    pub skin: Option<usize>,
    pub color: Color,
    pub x: f32,
    pub y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub time: f32,
}

impl Skeleton {
    pub fn new(data: &SkeletonData) -> Skeleton {
        let mut bones: Vec<Bone> = data
            .bones
            .iter()
            .map(|b| Bone {
                parent: b.parent,
                children: Vec::new(),
                x: 0.0,
                y: 0.0,
                rotation: 0.0,
                scale_x: 0.0,
                scale_y: 0.0,
                shear_x: 0.0,
                shear_y: 0.0,
                inherit: b.inherit,
                ax: 0.0,
                ay: 0.0,
                arotation: 0.0,
                ascale_x: 0.0,
                ascale_y: 0.0,
                ashear_x: 0.0,
                ashear_y: 0.0,
                a: 0.0,
                b: 0.0,
                c: 0.0,
                d: 0.0,
                world_x: 0.0,
                world_y: 0.0,
                sorted: false,
                active: false,
            })
            .collect();
        for i in 0..bones.len() {
            if let Some(p) = bones[i].parent {
                bones[p as usize].children.push(i as u16);
            }
        }
        let slots = data
            .slots
            .iter()
            .map(|s| Slot {
                bone: s.bone,
                color: s.color,
                dark: s.dark,
                attachment: None,
                sequence_index: -1,
                deform: Vec::new(),
            })
            .collect();
        let ik = data
            .ik_constraints
            .iter()
            .map(|c| IkConstraint {
                mix: c.mix,
                softness: c.softness,
                bend_direction: c.bend_direction,
                compress: c.compress,
                stretch: c.stretch,
                active: false,
            })
            .collect();
        let transform = data
            .transform_constraints
            .iter()
            .map(|c| TransformConstraint {
                mix_rotate: c.mix_rotate,
                mix_x: c.mix_x,
                mix_y: c.mix_y,
                mix_scale_x: c.mix_scale_x,
                mix_scale_y: c.mix_scale_y,
                mix_shear_y: c.mix_shear_y,
                active: false,
            })
            .collect();
        let path = data
            .path_constraints
            .iter()
            .map(|c| PathConstraint {
                position: c.position,
                spacing: c.spacing,
                mix_rotate: c.mix_rotate,
                mix_x: c.mix_x,
                mix_y: c.mix_y,
                active: false,
                spaces: Vec::new(),
                positions: Vec::new(),
                world: Vec::new(),
                curves: Vec::new(),
                lengths: Vec::new(),
                segments: [0.0; 10],
            })
            .collect();
        let physics = data
            .physics_constraints
            .iter()
            .map(|c| PhysicsConstraint {
                inertia: c.inertia,
                strength: c.strength,
                damping: c.damping,
                mass_inverse: c.mass_inverse,
                wind: c.wind,
                gravity: c.gravity,
                mix: c.mix,
                reset: true,
                ..Default::default()
            })
            .collect();
        let mut s = Skeleton {
            bones,
            slots,
            draw_order: (0..data.slots.len() as u16).collect(),
            ik,
            transform,
            path,
            physics,
            update_cache: Vec::new(),
            skin: None,
            color: Color::WHITE,
            x: 0.0,
            y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            time: 0.0,
        };
        s.update_cache(data);
        s.set_to_setup_pose(data);
        s
    }

    pub fn update(&mut self, delta: f32) {
        self.time += delta;
    }

    pub fn find_bone(&self, data: &SkeletonData, name: &str) -> Option<usize> {
        data.find_bone(name)
    }

    /// Looks an attachment up in the active skin, then the default skin.
    pub fn attachment(&self, data: &SkeletonData, slot: u16, name: &str) -> Option<u32> {
        if let Some(skin) = self.skin {
            if let Some(a) = data.skins[skin].get(slot, name) {
                return Some(a);
            }
        }
        data.default_skin().and_then(|s| s.get(slot, name))
    }

    pub fn set_skin(&mut self, data: &SkeletonData, skin: Option<usize>) {
        if skin == self.skin {
            return;
        }
        if let Some(new) = skin {
            if self.skin.is_some() {
                // Attach all attachments from the new skin that the old skin had set.
                let old = self.skin.unwrap();
                for &(slot, ref name, old_id) in &data.skins[old].attachments {
                    if self.slots[slot as usize].attachment == Some(old_id) {
                        if let Some(id) = data.skins[new].get(slot, name) {
                            set_slot_attachment(data, &mut self.slots[slot as usize], Some(id));
                        }
                    }
                }
            } else {
                for i in 0..self.slots.len() {
                    if let Some(name) = &data.slots[i].attachment {
                        if let Some(id) = data.skins[new].get(i as u16, name) {
                            set_slot_attachment(data, &mut self.slots[i], Some(id));
                        }
                    }
                }
            }
        }
        self.skin = skin;
        self.update_cache(data);
    }

    pub fn set_to_setup_pose(&mut self, data: &SkeletonData) {
        self.set_bones_to_setup_pose(data);
        self.set_slots_to_setup_pose(data);
    }

    pub fn set_bones_to_setup_pose(&mut self, data: &SkeletonData) {
        for (b, d) in self.bones.iter_mut().zip(&data.bones) {
            b.x = d.x;
            b.y = d.y;
            b.rotation = d.rotation;
            b.scale_x = d.scale_x;
            b.scale_y = d.scale_y;
            b.shear_x = d.shear_x;
            b.shear_y = d.shear_y;
            b.inherit = d.inherit;
        }
        for (c, d) in self.ik.iter_mut().zip(&data.ik_constraints) {
            c.mix = d.mix;
            c.softness = d.softness;
            c.bend_direction = d.bend_direction;
            c.compress = d.compress;
            c.stretch = d.stretch;
        }
        for (c, d) in self.transform.iter_mut().zip(&data.transform_constraints) {
            c.mix_rotate = d.mix_rotate;
            c.mix_x = d.mix_x;
            c.mix_y = d.mix_y;
            c.mix_scale_x = d.mix_scale_x;
            c.mix_scale_y = d.mix_scale_y;
            c.mix_shear_y = d.mix_shear_y;
        }
        for (c, d) in self.path.iter_mut().zip(&data.path_constraints) {
            c.position = d.position;
            c.spacing = d.spacing;
            c.mix_rotate = d.mix_rotate;
            c.mix_x = d.mix_x;
            c.mix_y = d.mix_y;
        }
        for (c, d) in self.physics.iter_mut().zip(&data.physics_constraints) {
            c.inertia = d.inertia;
            c.strength = d.strength;
            c.damping = d.damping;
            c.mass_inverse = d.mass_inverse;
            c.wind = d.wind;
            c.gravity = d.gravity;
            c.mix = d.mix;
        }
    }

    pub fn set_slots_to_setup_pose(&mut self, data: &SkeletonData) {
        for (i, o) in self.draw_order.iter_mut().enumerate() {
            *o = i as u16;
        }
        for i in 0..self.slots.len() {
            self.set_slot_to_setup_pose(data, i);
        }
    }

    pub fn set_slot_to_setup_pose(&mut self, data: &SkeletonData, i: usize) {
        let d = &data.slots[i];
        let attachment = d.attachment.as_deref().and_then(|name| self.attachment(data, i as u16, name));
        let slot = &mut self.slots[i];
        slot.color = d.color;
        if let (Some(dark), Some(setup)) = (&mut slot.dark, d.dark) {
            *dark = setup;
        }
        slot.attachment = None;
        set_slot_attachment(data, slot, attachment);
    }

    pub fn reset_physics(&mut self) {
        let time = self.time;
        for p in &mut self.physics {
            physics_reset(p, time);
        }
    }

    pub fn reset_physics_one(&mut self, i: usize) {
        let time = self.time;
        physics_reset(&mut self.physics[i], time);
    }

    /// Moves physics as if the skeleton were translated by (x, y) in world space.
    pub fn physics_translate(&mut self, x: f32, y: f32) {
        for p in &mut self.physics {
            p.ux -= x;
            p.uy -= y;
            p.cx -= x;
            p.cy -= y;
        }
    }

    // ---- update order ----

    pub fn update_cache(&mut self, data: &SkeletonData) {
        self.update_cache.clear();
        for (b, d) in self.bones.iter_mut().zip(&data.bones) {
            b.sorted = d.skin_required;
            b.active = !b.sorted;
        }
        if let Some(skin) = self.skin {
            for &bi in &data.skins[skin].bones {
                let mut bone = Some(bi);
                while let Some(b) = bone {
                    self.bones[b as usize].sorted = false;
                    self.bones[b as usize].active = true;
                    bone = self.bones[b as usize].parent;
                }
            }
        }
        let skin_has = |s: &Skeleton, c: ConstraintRef| s.skin.is_some_and(|k| data.skins[k].constraints.contains(&c));
        let total = data.ik_constraints.len()
            + data.transform_constraints.len()
            + data.path_constraints.len()
            + data.physics_constraints.len();
        'outer: for order in 0..total as u32 {
            for (i, c) in data.ik_constraints.iter().enumerate() {
                if c.order == order {
                    let active = self.bones[c.target as usize].active
                        && (!c.skin_required || skin_has(self, ConstraintRef::Ik(i as u16)));
                    self.ik[i].active = active;
                    if active {
                        self.sort_bone(c.target);
                        let parent = c.bones[0];
                        self.sort_bone(parent);
                        if c.bones.len() == 1 {
                            self.update_cache.push(Update::Ik(i as u16));
                            self.sort_reset_children(parent);
                        } else {
                            let child = *c.bones.last().unwrap();
                            self.sort_bone(child);
                            self.update_cache.push(Update::Ik(i as u16));
                            self.sort_reset_children(parent);
                            self.bones[child as usize].sorted = true;
                        }
                    }
                    continue 'outer;
                }
            }
            for (i, c) in data.transform_constraints.iter().enumerate() {
                if c.order == order {
                    let active = self.bones[c.target as usize].active
                        && (!c.skin_required || skin_has(self, ConstraintRef::Transform(i as u16)));
                    self.transform[i].active = active;
                    if active {
                        self.sort_bone(c.target);
                        if c.local {
                            for &b in &c.bones {
                                if let Some(p) = self.bones[b as usize].parent {
                                    self.sort_bone(p);
                                }
                                self.sort_bone(b);
                            }
                        } else {
                            for &b in &c.bones {
                                self.sort_bone(b);
                            }
                        }
                        self.update_cache.push(Update::Transform(i as u16));
                        for &b in &c.bones {
                            self.sort_reset_children(b);
                        }
                        for &b in &c.bones {
                            self.bones[b as usize].sorted = true;
                        }
                    }
                    continue 'outer;
                }
            }
            for (i, c) in data.path_constraints.iter().enumerate() {
                if c.order == order {
                    let slot_bone = data.slots[c.target as usize].bone;
                    let active = self.bones[slot_bone as usize].active
                        && (!c.skin_required || skin_has(self, ConstraintRef::Path(i as u16)));
                    self.path[i].active = active;
                    if active {
                        let slot = c.target;
                        let mut skins: Vec<usize> = Vec::new();
                        if let Some(s) = self.skin {
                            skins.push(s);
                        }
                        if data.has_default_skin && self.skin != Some(0) {
                            skins.push(0);
                        }
                        skins.extend(0..data.skins.len());
                        for s in skins {
                            for (sl, _, id) in &data.skins[s].attachments {
                                if *sl == slot {
                                    self.sort_path_attachment(data, *id, slot_bone);
                                }
                            }
                        }
                        if let Some(a) = self.slots[slot as usize].attachment {
                            self.sort_path_attachment(data, a, slot_bone);
                        }
                        for &b in &c.bones {
                            self.sort_bone(b);
                        }
                        self.update_cache.push(Update::Path(i as u16));
                        for &b in &c.bones {
                            self.sort_reset_children(b);
                        }
                        for &b in &c.bones {
                            self.bones[b as usize].sorted = true;
                        }
                    }
                    continue 'outer;
                }
            }
            for (i, c) in data.physics_constraints.iter().enumerate() {
                if c.order == order {
                    let active = self.bones[c.bone as usize].active
                        && (!c.skin_required || skin_has(self, ConstraintRef::Physics(i as u16)));
                    self.physics[i].active = active;
                    if active {
                        self.sort_bone(c.bone);
                        self.update_cache.push(Update::Physics(i as u16));
                        self.sort_reset_children(c.bone);
                        self.bones[c.bone as usize].sorted = true;
                    }
                    continue 'outer;
                }
            }
        }
        for i in 0..self.bones.len() {
            self.sort_bone(i as u16);
        }
    }

    fn sort_path_attachment(&mut self, data: &SkeletonData, attachment: u32, slot_bone: u16) {
        if let AttachmentKind::Path(p) = &data.attachments[attachment as usize].kind {
            if !p.vertices.is_weighted() {
                self.sort_bone(slot_bone);
            } else {
                let bones = &p.vertices.bones;
                let mut i = 0;
                while i < bones.len() {
                    let nn = bones[i] as usize;
                    i += 1;
                    let end = i + nn;
                    while i < end {
                        self.sort_bone(bones[i] as u16);
                        i += 1;
                    }
                }
            }
        }
    }

    fn sort_bone(&mut self, bone: u16) {
        if self.bones[bone as usize].sorted {
            return;
        }
        if let Some(p) = self.bones[bone as usize].parent {
            self.sort_bone(p);
        }
        self.bones[bone as usize].sorted = true;
        self.update_cache.push(Update::Bone(bone));
    }

    fn sort_reset_children(&mut self, bone: u16) {
        let n = self.bones[bone as usize].children.len();
        for k in 0..n {
            let c = self.bones[bone as usize].children[k];
            self.sort_reset(c);
        }
    }

    fn sort_reset(&mut self, bone: u16) {
        if !self.bones[bone as usize].active {
            return;
        }
        if self.bones[bone as usize].sorted {
            self.sort_reset_children(bone);
        }
        self.bones[bone as usize].sorted = false;
    }

    // ---- world transforms ----

    pub fn update_world_transform(&mut self, data: &SkeletonData, physics: Physics) {
        for b in &mut self.bones {
            b.ax = b.x;
            b.ay = b.y;
            b.arotation = b.rotation;
            b.ascale_x = b.scale_x;
            b.ascale_y = b.scale_y;
            b.ashear_x = b.shear_x;
            b.ashear_y = b.shear_y;
        }
        for k in 0..self.update_cache.len() {
            match self.update_cache[k] {
                Update::Bone(i) => {
                    let b = &self.bones[i as usize];
                    let (x, y, r, sx, sy, shx, shy) = (b.ax, b.ay, b.arotation, b.ascale_x, b.ascale_y, b.ashear_x, b.ashear_y);
                    self.update_bone(i as usize, x, y, r, sx, sy, shx, shy);
                }
                Update::Ik(i) => self.apply_ik(data, i as usize),
                Update::Transform(i) => self.apply_transform(data, i as usize),
                Update::Path(i) => self.apply_path(data, i as usize),
                Update::Physics(i) => self.apply_physics(data, i as usize, physics),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_bone(&mut self, i: usize, x: f32, y: f32, rotation: f32, scale_x: f32, scale_y: f32, shear_x: f32, shear_y: f32) {
        let (ssx, ssy, skx, sky) = (self.scale_x, self.scale_y, self.x, self.y);
        let parent = self.bones[i].parent.map(|p| {
            let p = &self.bones[p as usize];
            (p.a, p.b, p.c, p.d, p.world_x, p.world_y)
        });
        let bone = &mut self.bones[i];
        bone.ax = x;
        bone.ay = y;
        bone.arotation = rotation;
        bone.ascale_x = scale_x;
        bone.ascale_y = scale_y;
        bone.ashear_x = shear_x;
        bone.ashear_y = shear_y;
        let Some((mut pa, mut pb, mut pc, mut pd, pwx, pwy)) = parent else {
            let rx = (rotation + shear_x) * DEG_RAD;
            let ry = (rotation + 90.0 + shear_y) * DEG_RAD;
            bone.a = cos(rx) * scale_x * ssx;
            bone.b = cos(ry) * scale_y * ssx;
            bone.c = sin(rx) * scale_x * ssy;
            bone.d = sin(ry) * scale_y * ssy;
            bone.world_x = x * ssx + skx;
            bone.world_y = y * ssy + sky;
            return;
        };
        bone.world_x = pa * x + pb * y + pwx;
        bone.world_y = pc * x + pd * y + pwy;
        match bone.inherit {
            Inherit::Normal => {
                let rx = (rotation + shear_x) * DEG_RAD;
                let ry = (rotation + 90.0 + shear_y) * DEG_RAD;
                let la = cos(rx) * scale_x;
                let lb = cos(ry) * scale_y;
                let lc = sin(rx) * scale_x;
                let ld = sin(ry) * scale_y;
                bone.a = pa * la + pb * lc;
                bone.b = pa * lb + pb * ld;
                bone.c = pc * la + pd * lc;
                bone.d = pc * lb + pd * ld;
                return;
            }
            Inherit::OnlyTranslation => {
                let rx = (rotation + shear_x) * DEG_RAD;
                let ry = (rotation + 90.0 + shear_y) * DEG_RAD;
                bone.a = cos(rx) * scale_x;
                bone.b = cos(ry) * scale_y;
                bone.c = sin(rx) * scale_x;
                bone.d = sin(ry) * scale_y;
            }
            Inherit::NoRotationOrReflection => {
                let sx = 1.0 / ssx;
                let sy = 1.0 / ssy;
                pa *= sx;
                pc *= sy;
                let mut s = pa * pa + pc * pc;
                let prx;
                if s > 0.0001 {
                    s = abs(pa * pd * sy - pb * sx * pc) / s;
                    pb = pc * s;
                    pd = pa * s;
                    prx = atan2(pc, pa) * RAD_DEG;
                } else {
                    pa = 0.0;
                    pc = 0.0;
                    prx = 90.0 - atan2(pd, pb) * RAD_DEG;
                }
                let rx = (rotation + shear_x - prx) * DEG_RAD;
                let ry = (rotation + shear_y - prx + 90.0) * DEG_RAD;
                let la = cos(rx) * scale_x;
                let lb = cos(ry) * scale_y;
                let lc = sin(rx) * scale_x;
                let ld = sin(ry) * scale_y;
                bone.a = pa * la - pb * lc;
                bone.b = pa * lb - pb * ld;
                bone.c = pc * la + pd * lc;
                bone.d = pc * lb + pd * ld;
            }
            Inherit::NoScale | Inherit::NoScaleOrReflection => {
                let r = rotation * DEG_RAD;
                let (co, si) = (cos(r), sin(r));
                let mut za = (pa * co + pb * si) / ssx;
                let mut zc = (pc * co + pd * si) / ssy;
                let mut s = sqrt(za * za + zc * zc);
                if s > 0.00001 {
                    s = 1.0 / s;
                }
                za *= s;
                zc *= s;
                s = sqrt(za * za + zc * zc);
                if bone.inherit == Inherit::NoScale && ((pa * pd - pb * pc < 0.0) != ((ssx < 0.0) != (ssy < 0.0))) {
                    s = -s;
                }
                let r = PI / 2.0 + atan2(zc, za);
                let zb = cos(r) * s;
                let zd = sin(r) * s;
                let shx = shear_x * DEG_RAD;
                let shy = (90.0 + shear_y) * DEG_RAD;
                let la = cos(shx) * scale_x;
                let lb = cos(shy) * scale_y;
                let lc = sin(shx) * scale_x;
                let ld = sin(shy) * scale_y;
                bone.a = za * la + zb * lc;
                bone.b = za * lb + zb * ld;
                bone.c = zc * la + zd * lc;
                bone.d = zc * lb + zd * ld;
            }
        }
        bone.a *= ssx;
        bone.b *= ssx;
        bone.c *= ssy;
        bone.d *= ssy;
    }

    /// Recomputes the applied (local) transform from the world transform.
    pub fn update_applied_transform(&mut self, i: usize) {
        let (ssx, ssy, skx, sky) = (self.scale_x, self.scale_y, self.x, self.y);
        let parent = self.bones[i].parent.map(|p| {
            let p = &self.bones[p as usize];
            (p.a, p.b, p.c, p.d, p.world_x, p.world_y)
        });
        let bone = &mut self.bones[i];
        let Some((mut pa, mut pb, pc, mut pd, pwx, pwy)) = parent else {
            bone.ax = bone.world_x - skx;
            bone.ay = bone.world_y - sky;
            bone.arotation = atan2(bone.c, bone.a) * RAD_DEG;
            bone.ascale_x = sqrt(bone.a * bone.a + bone.c * bone.c);
            bone.ascale_y = sqrt(bone.b * bone.b + bone.d * bone.d);
            bone.ashear_x = 0.0;
            bone.ashear_y = atan2(bone.a * bone.b + bone.c * bone.d, bone.a * bone.d - bone.b * bone.c) * RAD_DEG;
            return;
        };
        let mut pc = pc;
        let mut pid = 1.0 / (pa * pd - pb * pc);
        let (mut ia, mut ib, mut ic, mut id) = (pd * pid, pb * pid, pc * pid, pa * pid);
        let dx = bone.world_x - pwx;
        let dy = bone.world_y - pwy;
        bone.ax = dx * ia - dy * ib;
        bone.ay = dy * id - dx * ic;
        let (ra, rb, rc, rd);
        if bone.inherit == Inherit::OnlyTranslation {
            ra = bone.a;
            rb = bone.b;
            rc = bone.c;
            rd = bone.d;
        } else {
            match bone.inherit {
                Inherit::NoRotationOrReflection => {
                    let s = abs(pa * pd - pb * pc) / (pa * pa + pc * pc);
                    pb = -pc * ssx * s / ssy;
                    pd = pa * ssy * s / ssx;
                    pid = 1.0 / (pa * pd - pb * pc);
                    ia = pd * pid;
                    ib = pb * pid;
                }
                Inherit::NoScale | Inherit::NoScaleOrReflection => {
                    let co = cos_deg(bone.rotation);
                    let si = sin_deg(bone.rotation);
                    pa = (pa * co + pb * si) / ssx;
                    pc = (pc * co + pd * si) / ssy;
                    let mut s = sqrt(pa * pa + pc * pc);
                    if s > 0.00001 {
                        s = 1.0 / s;
                    }
                    pa *= s;
                    pc *= s;
                    s = sqrt(pa * pa + pc * pc);
                    if bone.inherit == Inherit::NoScale && ((pid < 0.0) != ((ssx < 0.0) != (ssy < 0.0))) {
                        s = -s;
                    }
                    let r = PI / 2.0 + atan2(pc, pa);
                    pb = cos(r) * s;
                    pd = sin(r) * s;
                    pid = 1.0 / (pa * pd - pb * pc);
                    ia = pd * pid;
                    ib = pb * pid;
                    ic = pc * pid;
                    id = pa * pid;
                }
                _ => {}
            }
            ra = ia * bone.a - ib * bone.c;
            rb = ia * bone.b - ib * bone.d;
            rc = id * bone.c - ic * bone.a;
            rd = id * bone.d - ic * bone.b;
        }
        bone.ashear_x = 0.0;
        bone.ascale_x = sqrt(ra * ra + rc * rc);
        if bone.ascale_x > 0.0001 {
            let det = ra * rd - rb * rc;
            bone.ascale_y = det / bone.ascale_x;
            bone.ashear_y = -atan2(ra * rb + rc * rd, det) * RAD_DEG;
            bone.arotation = atan2(rc, ra) * RAD_DEG;
        } else {
            bone.ascale_x = 0.0;
            bone.ascale_y = sqrt(rb * rb + rd * rd);
            bone.ashear_y = 0.0;
            bone.arotation = 90.0 - atan2(rd, rb) * RAD_DEG;
        }
    }

    // ---- IK ----

    fn apply_ik(&mut self, data: &SkeletonData, i: usize) {
        let c = &self.ik[i];
        if c.mix == 0.0 {
            return;
        }
        let d = &data.ik_constraints[i];
        let (tx, ty) = {
            let t = &self.bones[d.target as usize];
            (t.world_x, t.world_y)
        };
        let (mix, softness, bend, compress, stretch) = (c.mix, c.softness, c.bend_direction, c.compress, c.stretch);
        match d.bones.len() {
            1 => self.ik_apply1(data, d.bones[0] as usize, tx, ty, compress, stretch, d.uniform, mix),
            2 => self.ik_apply2(data, d.bones[0] as usize, d.bones[1] as usize, tx, ty, bend as f32, stretch, d.uniform, softness, mix),
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ik_apply1(&mut self, data: &SkeletonData, bi: usize, target_x: f32, target_y: f32, compress: bool, stretch: bool, uniform: bool, alpha: f32) {
        let Some(pi) = self.bones[bi].parent else { return };
        let p = &self.bones[pi as usize];
        let (mut pa, mut pb, pc, mut pd, pwx, pwy) = (p.a, p.b, p.c, p.d, p.world_x, p.world_y);
        let bone = &self.bones[bi];
        let mut rotation_ik = -bone.ashear_x - bone.arotation;
        let (mut tx, mut ty);
        let inherit = bone.inherit;
        match inherit {
            Inherit::OnlyTranslation => {
                tx = (target_x - bone.world_x) * signum(self.scale_x);
                ty = (target_y - bone.world_y) * signum(self.scale_y);
            }
            _ => {
                if inherit == Inherit::NoRotationOrReflection {
                    let s = abs(pa * pd - pb * pc) / f32::max(0.0001, pa * pa + pc * pc);
                    let sa = pa / self.scale_x;
                    let sc = pc / self.scale_y;
                    pb = -sc * s * self.scale_x;
                    pd = sa * s * self.scale_y;
                    rotation_ik += atan2(sc, sa) * RAD_DEG;
                    let _ = &mut pa;
                }
                let x = target_x - pwx;
                let y = target_y - pwy;
                let d = pa * pd - pb * pc;
                if abs(d) <= 0.0001 {
                    tx = 0.0;
                    ty = 0.0;
                } else {
                    tx = (x * pd - y * pb) / d - bone.ax;
                    ty = (y * pa - x * pc) / d - bone.ay;
                }
            }
        }
        rotation_ik += atan2(ty, tx) * RAD_DEG;
        if bone.ascale_x < 0.0 {
            rotation_ik += 180.0;
        }
        if rotation_ik > 180.0 {
            rotation_ik -= 360.0;
        } else if rotation_ik < -180.0 {
            rotation_ik += 360.0;
        }
        let (mut sx, mut sy) = (bone.ascale_x, bone.ascale_y);
        if compress || stretch {
            if matches!(inherit, Inherit::NoScale | Inherit::NoScaleOrReflection) {
                tx = target_x - bone.world_x;
                ty = target_y - bone.world_y;
            }
            let b = data.bones[bi].length * sx;
            if b > 0.0001 {
                let dd = tx * tx + ty * ty;
                if (compress && dd < b * b) || (stretch && dd > b * b) {
                    let s = (sqrt(dd) / b - 1.0) * alpha + 1.0;
                    sx *= s;
                    if uniform {
                        sy *= s;
                    }
                }
            }
        }
        let (ax, ay, ar, ashx, ashy) = (bone.ax, bone.ay, bone.arotation, bone.ashear_x, bone.ashear_y);
        self.update_bone(bi, ax, ay, ar + rotation_ik * alpha, sx, sy, ashx, ashy);
    }

    #[allow(clippy::too_many_arguments)]
    fn ik_apply2(
        &mut self,
        data: &SkeletonData,
        pi: usize,
        ci: usize,
        target_x: f32,
        target_y: f32,
        bend_dir: f32,
        stretch: bool,
        uniform: bool,
        mut softness: f32,
        alpha: f32,
    ) {
        if self.bones[pi].inherit != Inherit::Normal || self.bones[ci].inherit != Inherit::Normal {
            return;
        }
        let parent = &self.bones[pi];
        let child = &self.bones[ci];
        let (px, py) = (parent.ax, parent.ay);
        let (mut psx, mut psy) = (parent.ascale_x, parent.ascale_y);
        let (mut sx, mut sy) = (psx, psy);
        let mut csx = child.ascale_x;
        let (os1, mut s2);
        if psx < 0.0 {
            psx = -psx;
            os1 = 180.0;
            s2 = -1.0;
        } else {
            os1 = 0.0;
            s2 = 1.0;
        }
        if psy < 0.0 {
            psy = -psy;
            s2 = -s2;
        }
        let os2 = if csx < 0.0 {
            csx = -csx;
            180.0
        } else {
            0.0
        };
        let cx = child.ax;
        let (cy, cwx, cwy);
        let (mut a, mut b, mut c, mut d) = (parent.a, parent.b, parent.c, parent.d);
        let u = abs(psx - psy) <= 0.0001;
        if !u || stretch {
            cy = 0.0;
            cwx = a * cx + parent.world_x;
            cwy = c * cx + parent.world_y;
        } else {
            cy = child.ay;
            cwx = a * cx + b * cy + parent.world_x;
            cwy = c * cx + d * cy + parent.world_y;
        }
        let Some(ppi) = parent.parent else { return };
        let pp = &self.bones[ppi as usize];
        a = pp.a;
        b = pp.b;
        c = pp.c;
        d = pp.d;
        let (ppwx, ppwy) = (pp.world_x, pp.world_y);
        let mut id = a * d - b * c;
        let mut x = cwx - ppwx;
        let mut y = cwy - ppwy;
        id = if abs(id) <= 0.0001 { 0.0 } else { 1.0 / id };
        let dx = (x * d - y * b) * id - px;
        let dy = (y * a - x * c) * id - py;
        let l1 = sqrt(dx * dx + dy * dy);
        let mut l2 = data.bones[ci].length * csx;
        let (a1, a2);
        if l1 < 0.0001 {
            self.ik_apply1(data, pi, target_x, target_y, false, stretch, false, alpha);
            let ch = &self.bones[ci];
            let (csx2, csy2, cshx, cshy) = (ch.ascale_x, ch.ascale_y, ch.ashear_x, ch.ashear_y);
            self.update_bone(ci, cx, cy, 0.0, csx2, csy2, cshx, cshy);
            return;
        }
        x = target_x - ppwx;
        y = target_y - ppwy;
        let mut tx = (x * d - y * b) * id - px;
        let mut ty = (y * a - x * c) * id - py;
        let mut dd = tx * tx + ty * ty;
        if softness != 0.0 {
            softness *= psx * (csx + 1.0) * 0.5;
            let td = sqrt(dd);
            let sd = td - l1 - l2 * psx + softness;
            if sd > 0.0 {
                let mut p = f32::min(1.0, sd / (softness * 2.0)) - 1.0;
                p = (sd - softness * (1.0 - p * p)) / td;
                tx -= p * tx;
                ty -= p * ty;
                dd = tx * tx + ty * ty;
            }
        }
        'outer: {
            if u {
                l2 *= psx;
                let mut cs = (dd - l1 * l1 - l2 * l2) / (2.0 * l1 * l2);
                let aa2;
                if cs < -1.0 {
                    cs = -1.0;
                    aa2 = core::f32::consts::PI * bend_dir;
                } else if cs > 1.0 {
                    cs = 1.0;
                    aa2 = 0.0;
                    if stretch {
                        let s = (sqrt(dd) / (l1 + l2) - 1.0) * alpha + 1.0;
                        sx *= s;
                        if uniform {
                            sy *= s;
                        }
                    }
                } else {
                    aa2 = acos(cs) * bend_dir;
                }
                let aa = l1 + l2 * cs;
                let bb = l2 * sin(aa2);
                a1 = atan2(ty * aa - tx * bb, tx * aa + ty * bb);
                a2 = aa2;
            } else {
                let aa_ = psx * l2;
                let bb_ = psy * l2;
                let aa = aa_ * aa_;
                let bb = bb_ * bb_;
                let ta = atan2(ty, tx);
                let mut cc = bb * l1 * l1 + aa * dd - aa * bb;
                let c1 = -2.0 * bb * l1;
                let c2 = bb - aa;
                let disc = c1 * c1 - 4.0 * c2 * cc;
                if disc >= 0.0 {
                    let mut q = sqrt(disc);
                    if c1 < 0.0 {
                        q = -q;
                    }
                    q = -(c1 + q) * 0.5;
                    let r0 = q / c2;
                    let r1 = cc / q;
                    let r = if abs(r0) < abs(r1) { r0 } else { r1 };
                    let r0 = dd - r * r;
                    if r0 >= 0.0 {
                        let yy = sqrt(r0) * bend_dir;
                        a1 = ta - atan2(yy, r);
                        a2 = atan2(yy / psy, (r - l1) / psx);
                        break 'outer;
                    }
                }
                let mut min_angle = PI;
                let mut min_x = l1 - aa_;
                let mut min_dist = min_x * min_x;
                let mut min_y = 0.0;
                let mut max_angle = 0.0;
                let mut max_x = l1 + aa_;
                let mut max_dist = max_x * max_x;
                let mut max_y = 0.0;
                cc = -aa_ * l1 / (aa - bb);
                if (-1.0..=1.0).contains(&cc) {
                    cc = acos(cc);
                    let xx = aa_ * cos(cc) + l1;
                    let yy = bb_ * sin(cc);
                    let dist = xx * xx + yy * yy;
                    if dist < min_dist {
                        min_angle = cc;
                        min_dist = dist;
                        min_x = xx;
                        min_y = yy;
                    }
                    if dist > max_dist {
                        max_angle = cc;
                        max_dist = dist;
                        max_x = xx;
                        max_y = yy;
                    }
                }
                if dd <= (min_dist + max_dist) * 0.5 {
                    a1 = ta - atan2(min_y * bend_dir, min_x);
                    a2 = min_angle * bend_dir;
                } else {
                    a1 = ta - atan2(max_y * bend_dir, max_x);
                    a2 = max_angle * bend_dir;
                }
            }
        }
        let os = atan2(cy, cx) * s2;
        let rotation = self.bones[pi].arotation;
        let mut a1 = (a1 - os) * RAD_DEG + os1 - rotation;
        if a1 > 180.0 {
            a1 -= 360.0;
        } else if a1 < -180.0 {
            a1 += 360.0;
        }
        self.update_bone(pi, px, py, rotation + a1 * alpha, sx, sy, 0.0, 0.0);
        let ch = &self.bones[ci];
        let rotation = ch.arotation;
        let mut a2 = ((a2 + os) * RAD_DEG - ch.ashear_x) * s2 + os2 - rotation;
        if a2 > 180.0 {
            a2 -= 360.0;
        } else if a2 < -180.0 {
            a2 += 360.0;
        }
        let (csx2, csy2, cshx, cshy) = (ch.ascale_x, ch.ascale_y, ch.ashear_x, ch.ashear_y);
        self.update_bone(ci, cx, cy, rotation + a2 * alpha, csx2, csy2, cshx, cshy);
    }

    // ---- transform constraint ----

    fn apply_transform(&mut self, data: &SkeletonData, i: usize) {
        let c = self.transform[i].clone();
        if c.mix_rotate == 0.0 && c.mix_x == 0.0 && c.mix_y == 0.0 && c.mix_scale_x == 0.0 && c.mix_scale_y == 0.0 && c.mix_shear_y == 0.0 {
            return;
        }
        let d = &data.transform_constraints[i];
        let t = self.bones[d.target as usize].clone();
        if d.local {
            for &bi in &d.bones {
                let bi = bi as usize;
                let bone = &self.bones[bi];
                let (x, y, rotation, scale_x, scale_y, shear_x, shear_y);
                if d.relative {
                    rotation = bone.arotation + (t.arotation + d.offset_rotation) * c.mix_rotate;
                    x = bone.ax + (t.ax + d.offset_x) * c.mix_x;
                    y = bone.ay + (t.ay + d.offset_y) * c.mix_y;
                    scale_x = bone.ascale_x * (((t.ascale_x - 1.0 + d.offset_scale_x) * c.mix_scale_x) + 1.0);
                    scale_y = bone.ascale_y * (((t.ascale_y - 1.0 + d.offset_scale_y) * c.mix_scale_y) + 1.0);
                    shear_y = bone.ashear_y + (t.ashear_y + d.offset_shear_y) * c.mix_shear_y;
                } else {
                    let mut r = bone.arotation;
                    if c.mix_rotate != 0.0 {
                        r += (t.arotation - r + d.offset_rotation) * c.mix_rotate;
                    }
                    rotation = r;
                    x = bone.ax + (t.ax - bone.ax + d.offset_x) * c.mix_x;
                    y = bone.ay + (t.ay - bone.ay + d.offset_y) * c.mix_y;
                    let (mut sx, mut sy) = (bone.ascale_x, bone.ascale_y);
                    if c.mix_scale_x != 0.0 && sx != 0.0 {
                        sx = (sx + (t.ascale_x - sx + d.offset_scale_x) * c.mix_scale_x) / sx;
                    }
                    if c.mix_scale_y != 0.0 && sy != 0.0 {
                        sy = (sy + (t.ascale_y - sy + d.offset_scale_y) * c.mix_scale_y) / sy;
                    }
                    scale_x = sx;
                    scale_y = sy;
                    let mut shy = bone.ashear_y;
                    if c.mix_shear_y != 0.0 {
                        shy += (t.ashear_y - shy + d.offset_shear_y) * c.mix_shear_y;
                    }
                    shear_y = shy;
                }
                shear_x = bone.ashear_x;
                self.update_bone(bi, x, y, rotation, scale_x, scale_y, shear_x, shear_y);
            }
            return;
        }
        let (ta, tb, tc, td) = (t.a, t.b, t.c, t.d);
        let deg_rad_reflect = if ta * td - tb * tc > 0.0 { DEG_RAD } else { -DEG_RAD };
        let offset_rotation = d.offset_rotation * deg_rad_reflect;
        let offset_shear_y = d.offset_shear_y * deg_rad_reflect;
        let translate = c.mix_x != 0.0 || c.mix_y != 0.0;
        for &bi in &d.bones {
            let bi = bi as usize;
            let bone = &mut self.bones[bi];
            if c.mix_rotate != 0.0 {
                let (a, b, cc, dd) = (bone.a, bone.b, bone.c, bone.d);
                let mut r = if d.relative {
                    atan2(tc, ta) + offset_rotation
                } else {
                    atan2(tc, ta) - atan2(cc, a) + offset_rotation
                };
                if r > PI {
                    r -= PI2;
                } else if r < -PI {
                    r += PI2;
                }
                r *= c.mix_rotate;
                let (co, si) = (cos(r), sin(r));
                bone.a = co * a - si * cc;
                bone.b = co * b - si * dd;
                bone.c = si * a + co * cc;
                bone.d = si * b + co * dd;
            }
            if translate {
                let (wx, wy) = t.local_to_world(d.offset_x, d.offset_y);
                if d.relative {
                    bone.world_x += wx * c.mix_x;
                    bone.world_y += wy * c.mix_y;
                } else {
                    bone.world_x += (wx - bone.world_x) * c.mix_x;
                    bone.world_y += (wy - bone.world_y) * c.mix_y;
                }
            }
            if c.mix_scale_x != 0.0 {
                let s = if d.relative {
                    (sqrt(ta * ta + tc * tc) - 1.0 + d.offset_scale_x) * c.mix_scale_x + 1.0
                } else {
                    let mut s = sqrt(bone.a * bone.a + bone.c * bone.c);
                    if s != 0.0 {
                        s = (s + (sqrt(ta * ta + tc * tc) - s + d.offset_scale_x) * c.mix_scale_x) / s;
                    }
                    s
                };
                bone.a *= s;
                bone.c *= s;
            }
            if c.mix_scale_y != 0.0 {
                let s = if d.relative {
                    (sqrt(tb * tb + td * td) - 1.0 + d.offset_scale_y) * c.mix_scale_y + 1.0
                } else {
                    let mut s = sqrt(bone.b * bone.b + bone.d * bone.d);
                    if s != 0.0 {
                        s = (s + (sqrt(tb * tb + td * td) - s + d.offset_scale_y) * c.mix_scale_y) / s;
                    }
                    s
                };
                bone.b *= s;
                bone.d *= s;
            }
            if c.mix_shear_y > 0.0 {
                let (b, dd) = (bone.b, bone.d);
                let r = if d.relative {
                    let mut r = atan2(td, tb) - atan2(tc, ta);
                    if r > PI {
                        r -= PI2;
                    } else if r < -PI {
                        r += PI2;
                    }
                    atan2(dd, b) + (r - PI / 2.0 + offset_shear_y) * c.mix_shear_y
                } else {
                    let by = atan2(dd, b);
                    let mut r = atan2(td, tb) - atan2(tc, ta) - (by - atan2(bone.c, bone.a));
                    if r > PI {
                        r -= PI2;
                    } else if r < -PI {
                        r += PI2;
                    }
                    by + (r + offset_shear_y) * c.mix_shear_y
                };
                let s = sqrt(b * b + dd * dd);
                bone.b = cos(r) * s;
                bone.d = sin(r) * s;
            }
            self.update_applied_transform(bi);
        }
    }

    // ---- path constraint ----

    fn apply_path(&mut self, data: &SkeletonData, i: usize) {
        let d = &data.path_constraints[i];
        let Some(att) = self.slots[d.target as usize].attachment else { return };
        let AttachmentKind::Path(path) = &data.attachments[att as usize].kind else { return };
        let (mix_rotate, mix_x, mix_y) = (self.path[i].mix_rotate, self.path[i].mix_x, self.path[i].mix_y);
        if mix_rotate == 0.0 && mix_x == 0.0 && mix_y == 0.0 {
            return;
        }
        let tangents = d.rotate_mode == RotateMode::Tangent;
        let scale = d.rotate_mode == RotateMode::ChainScale;
        let bone_count = d.bones.len();
        let spaces_count = if tangents { bone_count } else { bone_count + 1 };
        let mut pc = core::mem::replace(
            &mut self.path[i],
            PathConstraint {
                position: 0.0,
                spacing: 0.0,
                mix_rotate: 0.0,
                mix_x: 0.0,
                mix_y: 0.0,
                active: false,
                spaces: Vec::new(),
                positions: Vec::new(),
                world: Vec::new(),
                curves: Vec::new(),
                lengths: Vec::new(),
                segments: [0.0; 10],
            },
        );
        pc.spaces.clear();
        pc.spaces.resize(spaces_count, 0.0);
        if scale {
            pc.lengths.clear();
            pc.lengths.resize(bone_count, 0.0);
        }
        let spacing = pc.spacing;
        const EPSILON: f32 = 0.00001;
        match d.spacing_mode {
            SpacingMode::Percent => {
                if scale {
                    for k in 0..spaces_count - 1 {
                        let bone = &self.bones[d.bones[k] as usize];
                        let setup_length = data.bones[d.bones[k] as usize].length;
                        let x = setup_length * bone.a;
                        let y = setup_length * bone.c;
                        pc.lengths[k] = sqrt(x * x + y * y);
                    }
                }
                for s in pc.spaces.iter_mut().skip(1) {
                    *s = spacing;
                }
            }
            SpacingMode::Proportional => {
                let mut sum = 0.0;
                let mut k = 0;
                while k < spaces_count - 1 {
                    let bone = &self.bones[d.bones[k] as usize];
                    let setup_length = data.bones[d.bones[k] as usize].length;
                    if setup_length < EPSILON {
                        if scale {
                            pc.lengths[k] = 0.0;
                        }
                        k += 1;
                        pc.spaces[k] = spacing;
                    } else {
                        let x = setup_length * bone.a;
                        let y = setup_length * bone.c;
                        let length = sqrt(x * x + y * y);
                        if scale {
                            pc.lengths[k] = length;
                        }
                        k += 1;
                        pc.spaces[k] = length;
                        sum += length;
                    }
                }
                if sum > 0.0 {
                    let f = spaces_count as f32 / sum * spacing;
                    for s in pc.spaces.iter_mut().skip(1) {
                        *s *= f;
                    }
                }
            }
            _ => {
                let length_spacing = d.spacing_mode == SpacingMode::Length;
                let mut k = 0;
                while k < spaces_count - 1 {
                    let bone = &self.bones[d.bones[k] as usize];
                    let setup_length = data.bones[d.bones[k] as usize].length;
                    if setup_length < EPSILON {
                        if scale {
                            pc.lengths[k] = 0.0;
                        }
                        k += 1;
                        pc.spaces[k] = spacing;
                    } else {
                        let x = setup_length * bone.a;
                        let y = setup_length * bone.c;
                        let length = sqrt(x * x + y * y);
                        if scale {
                            pc.lengths[k] = length;
                        }
                        k += 1;
                        pc.spaces[k] = (if length_spacing { setup_length + spacing } else { spacing }) * length / setup_length;
                    }
                }
            }
        }
        self.path_world_positions(data, &mut pc, d, path, spaces_count, tangents);
        let positions = core::mem::take(&mut pc.positions);
        let mut bone_x = positions[0];
        let mut bone_y = positions[1];
        let mut offset_rotation = d.offset_rotation;
        let tip;
        if offset_rotation == 0.0 {
            tip = d.rotate_mode == RotateMode::Chain;
        } else {
            tip = false;
            let p = &self.bones[data.slots[d.target as usize].bone as usize];
            offset_rotation *= if p.a * p.d - p.b * p.c > 0.0 { DEG_RAD } else { -DEG_RAD };
        }
        let mut p = 3;
        for k in 0..bone_count {
            let bi = d.bones[k] as usize;
            let length_setup = data.bones[bi].length;
            let bone = &mut self.bones[bi];
            bone.world_x += (bone_x - bone.world_x) * mix_x;
            bone.world_y += (bone_y - bone.world_y) * mix_y;
            let x = positions[p];
            let y = positions[p + 1];
            let dx = x - bone_x;
            let dy = y - bone_y;
            if scale {
                let length = pc.lengths[k];
                if length != 0.0 {
                    let s = (sqrt(dx * dx + dy * dy) / length - 1.0) * mix_rotate + 1.0;
                    bone.a *= s;
                    bone.c *= s;
                }
            }
            bone_x = x;
            bone_y = y;
            if mix_rotate > 0.0 {
                let (a, b, c, dd) = (bone.a, bone.b, bone.c, bone.d);
                let mut r = if tangents {
                    positions[p - 1]
                } else if pc.spaces[k + 1] == 0.0 {
                    positions[p + 2]
                } else {
                    atan2(dy, dx)
                };
                r -= atan2(c, a);
                if tip {
                    let co = cos(r);
                    let si = sin(r);
                    bone_x += (length_setup * (co * a - si * c) - dx) * mix_rotate;
                    bone_y += (length_setup * (si * a + co * c) - dy) * mix_rotate;
                } else {
                    r += offset_rotation;
                }
                if r > PI {
                    r -= PI2;
                } else if r < -PI {
                    r += PI2;
                }
                r *= mix_rotate;
                let co = cos(r);
                let si = sin(r);
                bone.a = co * a - si * c;
                bone.b = co * b - si * dd;
                bone.c = si * a + co * c;
                bone.d = si * b + co * dd;
            }
            self.update_applied_transform(bi);
            p += 3;
        }
        pc.positions = positions;
        self.path[i] = pc;
    }

    #[allow(clippy::too_many_arguments)]
    fn path_world_positions(
        &self,
        data: &SkeletonData,
        pc: &mut PathConstraint,
        d: &PathConstraintData,
        path: &PathAttachment,
        spaces_count: usize,
        tangents: bool,
    ) {
        let slot = d.target as usize;
        let mut position = pc.position;
        pc.positions.clear();
        pc.positions.resize(spaces_count * 3 + 2, 0.0);
        let closed = path.closed;
        let mut vertices_length = path.vertices.world_len as usize;
        let mut curve_count = vertices_length / 6;
        const NONE: i64 = -1;
        const BEFORE: i64 = -2;
        const AFTER: i64 = -3;
        let mut prev_curve: i64 = NONE;
        let out = &mut pc.positions;
        if !path.constant_speed {
            let lengths = &path.lengths;
            curve_count -= if closed { 1 } else { 2 };
            let path_length = lengths[curve_count];
            if d.position_mode == PositionMode::Percent {
                position *= path_length;
            }
            let multiplier = match d.spacing_mode {
                SpacingMode::Percent => path_length,
                SpacingMode::Proportional => path_length / spaces_count as f32,
                _ => 1.0,
            };
            let world = &mut pc.world;
            world.clear();
            world.resize(8, 0.0);
            let mut curve = 0usize;
            let mut o = 0;
            for k in 0..spaces_count {
                let space = pc.spaces[k] * multiplier;
                position += space;
                let mut p = position;
                if closed {
                    p = fmod(p, path_length);
                    if p < 0.0 {
                        p += path_length;
                    }
                    curve = 0;
                } else if p < 0.0 {
                    if prev_curve != BEFORE {
                        prev_curve = BEFORE;
                        self.vertex_world(data, slot, &path.vertices, 2, 4, world, 0, 2);
                    }
                    add_before_position(p, world, 0, out, o);
                    o += 3;
                    continue;
                } else if p > path_length {
                    if prev_curve != AFTER {
                        prev_curve = AFTER;
                        self.vertex_world(data, slot, &path.vertices, vertices_length - 6, 4, world, 0, 2);
                    }
                    add_after_position(p - path_length, world, 0, out, o);
                    o += 3;
                    continue;
                }
                loop {
                    let length = lengths[curve];
                    if p > length {
                        curve += 1;
                        continue;
                    }
                    if curve == 0 {
                        p /= length;
                    } else {
                        let prev = lengths[curve - 1];
                        p = (p - prev) / (length - prev);
                    }
                    break;
                }
                if curve as i64 != prev_curve {
                    prev_curve = curve as i64;
                    if closed && curve == curve_count {
                        self.vertex_world(data, slot, &path.vertices, vertices_length - 4, 4, world, 0, 2);
                        self.vertex_world(data, slot, &path.vertices, 0, 4, world, 4, 2);
                    } else {
                        self.vertex_world(data, slot, &path.vertices, curve * 6 + 2, 8, world, 0, 2);
                    }
                }
                add_curve_position(
                    p,
                    world[0],
                    world[1],
                    world[2],
                    world[3],
                    world[4],
                    world[5],
                    world[6],
                    world[7],
                    out,
                    o,
                    tangents || (k > 0 && space == 0.0),
                );
                o += 3;
            }
            return;
        }
        let world = &mut pc.world;
        if closed {
            vertices_length += 2;
            world.clear();
            world.resize(vertices_length, 0.0);
            self.vertex_world(data, slot, &path.vertices, 2, vertices_length - 4, world, 0, 2);
            self.vertex_world(data, slot, &path.vertices, 0, 2, world, vertices_length - 4, 2);
            world[vertices_length - 2] = world[0];
            world[vertices_length - 1] = world[1];
        } else {
            curve_count -= 1;
            vertices_length -= 4;
            world.clear();
            world.resize(vertices_length, 0.0);
            self.vertex_world(data, slot, &path.vertices, 2, vertices_length, world, 0, 2);
        }
        let curves = &mut pc.curves;
        curves.clear();
        curves.resize(curve_count, 0.0);
        let mut path_length = 0.0;
        let (mut x1, mut y1) = (world[0], world[1]);
        let (mut cx1, mut cy1, mut cx2, mut cy2, mut x2, mut y2);
        let mut w = 2;
        for k in 0..curve_count {
            cx1 = world[w];
            cy1 = world[w + 1];
            cx2 = world[w + 2];
            cy2 = world[w + 3];
            x2 = world[w + 4];
            y2 = world[w + 5];
            let tmpx = (x1 - cx1 * 2.0 + cx2) * 0.1875;
            let tmpy = (y1 - cy1 * 2.0 + cy2) * 0.1875;
            let dddfx = ((cx1 - cx2) * 3.0 - x1 + x2) * 0.09375;
            let dddfy = ((cy1 - cy2) * 3.0 - y1 + y2) * 0.09375;
            let mut ddfx = tmpx * 2.0 + dddfx;
            let mut ddfy = tmpy * 2.0 + dddfy;
            let mut dfx = (cx1 - x1) * 0.75 + tmpx + dddfx * 0.166_666_67;
            let mut dfy = (cy1 - y1) * 0.75 + tmpy + dddfy * 0.166_666_67;
            path_length += sqrt(dfx * dfx + dfy * dfy);
            dfx += ddfx;
            dfy += ddfy;
            ddfx += dddfx;
            ddfy += dddfy;
            path_length += sqrt(dfx * dfx + dfy * dfy);
            dfx += ddfx;
            dfy += ddfy;
            path_length += sqrt(dfx * dfx + dfy * dfy);
            dfx += ddfx + dddfx;
            dfy += ddfy + dddfy;
            path_length += sqrt(dfx * dfx + dfy * dfy);
            curves[k] = path_length;
            x1 = x2;
            y1 = y2;
            w += 6;
        }
        if d.position_mode == PositionMode::Percent {
            position *= path_length;
        }
        let multiplier = match d.spacing_mode {
            SpacingMode::Percent => path_length,
            SpacingMode::Proportional => path_length / spaces_count as f32,
            _ => 1.0,
        };
        let segments = &mut pc.segments;
        let mut curve_length = 0.0;
        let (mut curve, mut segment) = (0usize, 0usize);
        let (mut x1, mut y1, mut cx1, mut cy1, mut cx2, mut cy2, mut x2, mut y2) = (0f32, 0f32, 0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
        let mut o = 0;
        for k in 0..spaces_count {
            let space = pc.spaces[k] * multiplier;
            position += space;
            let mut p = position;
            if closed {
                p = fmod(p, path_length);
                if p < 0.0 {
                    p += path_length;
                }
                curve = 0;
            } else if p < 0.0 {
                add_before_position(p, world, 0, out, o);
                o += 3;
                continue;
            } else if p > path_length {
                add_after_position(p - path_length, world, vertices_length - 4, out, o);
                o += 3;
                continue;
            }
            loop {
                let length = curves[curve];
                if p > length {
                    curve += 1;
                    continue;
                }
                if curve == 0 {
                    p /= length;
                } else {
                    let prev = curves[curve - 1];
                    p = (p - prev) / (length - prev);
                }
                break;
            }
            if curve as i64 != prev_curve {
                prev_curve = curve as i64;
                let ii = curve * 6;
                x1 = world[ii];
                y1 = world[ii + 1];
                cx1 = world[ii + 2];
                cy1 = world[ii + 3];
                cx2 = world[ii + 4];
                cy2 = world[ii + 5];
                x2 = world[ii + 6];
                y2 = world[ii + 7];
                let tmpx = (x1 - cx1 * 2.0 + cx2) * 0.03;
                let tmpy = (y1 - cy1 * 2.0 + cy2) * 0.03;
                let dddfx = ((cx1 - cx2) * 3.0 - x1 + x2) * 0.006;
                let dddfy = ((cy1 - cy2) * 3.0 - y1 + y2) * 0.006;
                let mut ddfx = tmpx * 2.0 + dddfx;
                let mut ddfy = tmpy * 2.0 + dddfy;
                let mut dfx = (cx1 - x1) * 0.3 + tmpx + dddfx * 0.166_666_67;
                let mut dfy = (cy1 - y1) * 0.3 + tmpy + dddfy * 0.166_666_67;
                curve_length = sqrt(dfx * dfx + dfy * dfy);
                segments[0] = curve_length;
                for s in segments.iter_mut().take(8).skip(1) {
                    dfx += ddfx;
                    dfy += ddfy;
                    ddfx += dddfx;
                    ddfy += dddfy;
                    curve_length += sqrt(dfx * dfx + dfy * dfy);
                    *s = curve_length;
                }
                dfx += ddfx;
                dfy += ddfy;
                curve_length += sqrt(dfx * dfx + dfy * dfy);
                segments[8] = curve_length;
                dfx += ddfx + dddfx;
                dfy += ddfy + dddfy;
                curve_length += sqrt(dfx * dfx + dfy * dfy);
                segments[9] = curve_length;
                segment = 0;
            }
            p *= curve_length;
            loop {
                let length = segments[segment];
                if p > length {
                    segment += 1;
                    continue;
                }
                if segment == 0 {
                    p /= length;
                } else {
                    let prev = segments[segment - 1];
                    p = segment as f32 + (p - prev) / (length - prev);
                }
                break;
            }
            add_curve_position(p * 0.1, x1, y1, cx1, cy1, cx2, cy2, x2, y2, out, o, tangents || (k > 0 && space == 0.0));
            o += 3;
        }
    }

    /// World positions of `count` floats of a vertex attachment's output starting at `start`,
    /// using the slot's bone and deform.
    #[allow(clippy::too_many_arguments)]
    pub fn vertex_world(
        &self,
        _data: &SkeletonData,
        slot: usize,
        verts: &Vertices,
        start: usize,
        count: usize,
        out: &mut [f32],
        offset: usize,
        stride: usize,
    ) {
        let s = &self.slots[slot];
        compute_vertex_world(&self.bones, s.bone as usize, &s.deform, verts, start, count, out, offset, stride);
    }

    // ---- physics ----

    fn apply_physics(&mut self, data: &SkeletonData, i: usize, physics: Physics) {
        let d = &data.physics_constraints[i];
        let mix = self.physics[i].mix;
        if mix == 0.0 {
            return;
        }
        let x = d.x > 0.0;
        let y = d.y > 0.0;
        let rotate_or_shear_x = d.rotate > 0.0 || d.shear_x > 0.0;
        let scale_x = d.scale_x > 0.0;
        let bi = d.bone as usize;
        let l = data.bones[bi].length;
        let (ssx, ssy, time) = (self.scale_x, self.scale_y, self.time);
        let reference_scale = data.reference_scale;
        let p = &mut self.physics[i];
        let bone = &mut self.bones[bi];
        match physics {
            Physics::None => return,
            Physics::Reset | Physics::Update => {
                if physics == Physics::Reset {
                    physics_reset(p, time);
                }
                let delta = f32::max(time - p.last_time, 0.0);
                p.remaining += delta;
                p.last_time = time;
                let (bx, by) = (bone.world_x, bone.world_y);
                if p.reset {
                    p.reset = false;
                    p.ux = bx;
                    p.uy = by;
                } else {
                    let mut a = p.remaining;
                    let inertia = p.inertia;
                    let t = d.step;
                    let f = reference_scale;
                    let mut dmp = -1.0;
                    let mut qx = d.limit * delta;
                    let qy = qx * abs(ssy);
                    qx *= abs(ssx);
                    if x || y {
                        if x {
                            let u = (p.ux - bx) * inertia;
                            p.x_offset += if u > qx { qx } else if u < -qx { -qx } else { u };
                            p.ux = bx;
                        }
                        if y {
                            let u = (p.uy - by) * inertia;
                            p.y_offset += if u > qy { qy } else if u < -qy { -qy } else { u };
                            p.uy = by;
                        }
                        if a >= t {
                            dmp = pow(p.damping, 60.0 * t);
                            let m = p.mass_inverse * t;
                            let e = p.strength;
                            let w = p.wind * f * ssx;
                            let g = p.gravity * f * ssy;
                            loop {
                                if x {
                                    p.x_velocity += (w - p.x_offset * e) * m;
                                    p.x_offset += p.x_velocity * t;
                                    p.x_velocity *= dmp;
                                }
                                if y {
                                    p.y_velocity -= (g + p.y_offset * e) * m;
                                    p.y_offset += p.y_velocity * t;
                                    p.y_velocity *= dmp;
                                }
                                a -= t;
                                if a < t {
                                    break;
                                }
                            }
                        }
                        if x {
                            bone.world_x += p.x_offset * mix * d.x;
                        }
                        if y {
                            bone.world_y += p.y_offset * mix * d.y;
                        }
                    }
                    if rotate_or_shear_x || scale_x {
                        let ca = atan2(bone.c, bone.a);
                        let (mut c, mut s, mut mr): (f32, f32, f32);
                        mr = 0.0;
                        let mut dx = p.cx - bone.world_x;
                        let mut dy = p.cy - bone.world_y;
                        if dx > qx {
                            dx = qx;
                        } else if dx < -qx {
                            dx = -qx;
                        }
                        if dy > qy {
                            dy = qy;
                        } else if dy < -qy {
                            dy = -qy;
                        }
                        if rotate_or_shear_x {
                            mr = (d.rotate + d.shear_x) * mix;
                            let mut r = atan2(dy + p.ty, dx + p.tx) - ca - p.rotate_offset * mr;
                            p.rotate_offset += (r - ceil(r * INV_PI2 - 0.5) * PI2) * inertia;
                            r = p.rotate_offset * mr + ca;
                            c = cos(r);
                            s = sin(r);
                            if scale_x {
                                let r = l * bone.world_scale_x();
                                if r > 0.0 {
                                    p.scale_offset += (dx * c + dy * s) * inertia / r;
                                }
                            }
                        } else {
                            c = cos(ca);
                            s = sin(ca);
                            let r = l * bone.world_scale_x();
                            if r > 0.0 {
                                p.scale_offset += (dx * c + dy * s) * inertia / r;
                            }
                        }
                        a = p.remaining;
                        if a >= t {
                            if dmp == -1.0 {
                                dmp = pow(p.damping, 60.0 * t);
                            }
                            let m = p.mass_inverse * t;
                            let e = p.strength;
                            let w = p.wind;
                            let g = p.gravity;
                            let h = l / f;
                            loop {
                                a -= t;
                                if scale_x {
                                    p.scale_velocity += (w * c - g * s - p.scale_offset * e) * m;
                                    p.scale_offset += p.scale_velocity * t;
                                    p.scale_velocity *= dmp;
                                }
                                if rotate_or_shear_x {
                                    p.rotate_velocity -= ((w * s + g * c) * h + p.rotate_offset * e) * m;
                                    p.rotate_offset += p.rotate_velocity * t;
                                    p.rotate_velocity *= dmp;
                                    if a < t {
                                        break;
                                    }
                                    let r = p.rotate_offset * mr + ca;
                                    c = cos(r);
                                    s = sin(r);
                                } else if a < t {
                                    break;
                                }
                            }
                        }
                    }
                    p.remaining = a;
                }
                p.cx = bone.world_x;
                p.cy = bone.world_y;
            }
            Physics::Pose => {
                if x {
                    bone.world_x += p.x_offset * mix * d.x;
                }
                if y {
                    bone.world_y += p.y_offset * mix * d.y;
                }
            }
        }
        if rotate_or_shear_x {
            let mut o = p.rotate_offset * mix;
            if d.shear_x > 0.0 {
                let mut r = 0.0;
                if d.rotate > 0.0 {
                    r = o * d.rotate;
                    let s = sin(r);
                    let c = cos(r);
                    let a = bone.b;
                    bone.b = c * a - s * bone.d;
                    bone.d = s * a + c * bone.d;
                }
                r += o * d.shear_x;
                let s = sin(r);
                let c = cos(r);
                let a = bone.a;
                bone.a = c * a - s * bone.c;
                bone.c = s * a + c * bone.c;
            } else {
                o *= d.rotate;
                let s = sin(o);
                let c = cos(o);
                let a = bone.a;
                bone.a = c * a - s * bone.c;
                bone.c = s * a + c * bone.c;
                let a = bone.b;
                bone.b = c * a - s * bone.d;
                bone.d = s * a + c * bone.d;
            }
        }
        if scale_x {
            let s = 1.0 + p.scale_offset * mix * d.scale_x;
            bone.a *= s;
            bone.c *= s;
        }
        if physics != Physics::Pose {
            p.tx = l * bone.a;
            p.ty = l * bone.c;
        }
        self.update_applied_transform(bi);
    }
}

fn physics_reset(p: &mut PhysicsConstraint, time: f32) {
    p.remaining = 0.0;
    p.last_time = time;
    p.reset = true;
    p.x_offset = 0.0;
    p.x_velocity = 0.0;
    p.y_offset = 0.0;
    p.y_velocity = 0.0;
    p.rotate_offset = 0.0;
    p.rotate_velocity = 0.0;
    p.scale_offset = 0.0;
    p.scale_velocity = 0.0;
}

/// Sets a slot's attachment, clearing its deform unless both attachments share the same
/// timeline attachment (so deform keys keep applying across linked meshes).
pub fn set_slot_attachment(data: &SkeletonData, slot: &mut Slot, attachment: Option<u32>) {
    if slot.attachment == attachment {
        return;
    }
    let same_timeline = match (slot.attachment, attachment) {
        (Some(old), Some(new)) => {
            let (o, n) = (&data.attachments[old as usize], &data.attachments[new as usize]);
            o.vertices().is_some() && n.vertices().is_some() && o.timeline_attachment == n.timeline_attachment
        }
        _ => false,
    };
    if !same_timeline {
        slot.deform.clear();
    }
    slot.attachment = attachment;
    slot.sequence_index = -1;
}

/// Transforms `count` floats of a vertex attachment's world output (starting at output float
/// `start`) into `out[offset..]` with the given stride.
#[allow(clippy::too_many_arguments)]
pub fn compute_vertex_world(
    bones: &[Bone],
    slot_bone: usize,
    deform: &[f32],
    verts: &Vertices,
    start: usize,
    count: usize,
    out: &mut [f32],
    offset: usize,
    stride: usize,
) {
    let end = offset + (count >> 1) * stride;
    if !verts.is_weighted() {
        let vertices = if !deform.is_empty() { deform } else { &verts.vertices[..] };
        let bone = &bones[slot_bone];
        let (x, y, a, b, c, d) = (bone.world_x, bone.world_y, bone.a, bone.b, bone.c, bone.d);
        let mut v = start;
        let mut w = offset;
        while w < end {
            let vx = vertices[v];
            let vy = vertices[v + 1];
            out[w] = vx * a + vy * b + x;
            out[w + 1] = vx * c + vy * d + y;
            v += 2;
            w += stride;
        }
        return;
    }
    let bones_table = &verts.bones;
    let vertices = &verts.vertices;
    let (mut v, mut skip) = (0usize, 0usize);
    let mut i = 0;
    while i < start {
        let n = bones_table[v] as usize;
        v += n + 1;
        skip += n;
        i += 2;
    }
    let mut w = offset;
    let mut b = skip * 3;
    let mut f = skip << 1;
    let has_deform = !deform.is_empty();
    // Each vertex's influences are taken as slices (one bounds check per vertex, not per
    // influence: this loop is the watch's skinning, among its hottest).
    while w < end {
        let (mut wx, mut wy) = (0.0f32, 0.0f32);
        let n = bones_table[v] as usize;
        let ids = &bones_table[v + 1..v + 1 + n];
        let inf = &vertices[b..b + 3 * n];
        if has_deform {
            let def = &deform[f..f + 2 * n];
            for ((&id, c), d) in ids.iter().zip(inf.chunks_exact(3)).zip(def.chunks_exact(2)) {
                let bone = &bones[id as usize];
                let (vx, vy) = (c[0] + d[0], c[1] + d[1]);
                wx += (vx * bone.a + vy * bone.b + bone.world_x) * c[2];
                wy += (vx * bone.c + vy * bone.d + bone.world_y) * c[2];
            }
        } else {
            for (&id, c) in ids.iter().zip(inf.chunks_exact(3)) {
                let bone = &bones[id as usize];
                let (vx, vy) = (c[0], c[1]);
                wx += (vx * bone.a + vy * bone.b + bone.world_x) * c[2];
                wy += (vx * bone.c + vy * bone.d + bone.world_y) * c[2];
            }
        }
        v += 1 + n;
        b += 3 * n;
        f += 2 * n;
        out[w] = wx;
        out[w + 1] = wy;
        w += stride;
    }
}

/// Region attachment world corners (BL, TL, TR, BR) into `out[offset..]`.
pub fn compute_region_world(bone: &Bone, offset_table: &[f32; 8], out: &mut [f32], offset: usize, stride: usize) {
    let (x, y, a, b, c, d) = (bone.world_x, bone.world_y, bone.a, bone.b, bone.c, bone.d);
    let mut o = offset;
    for k in 0..4 {
        let ox = offset_table[k * 2];
        let oy = offset_table[k * 2 + 1];
        out[o] = ox * a + oy * b + x;
        out[o + 1] = ox * c + oy * d + y;
        o += stride;
    }
}

fn add_before_position(p: f32, temp: &[f32], i: usize, out: &mut [f32], o: usize) {
    let x1 = temp[i];
    let y1 = temp[i + 1];
    let dx = temp[i + 2] - x1;
    let dy = temp[i + 3] - y1;
    let r = atan2(dy, dx);
    out[o] = x1 + p * cos(r);
    out[o + 1] = y1 + p * sin(r);
    out[o + 2] = r;
}

fn add_after_position(p: f32, temp: &[f32], i: usize, out: &mut [f32], o: usize) {
    let x1 = temp[i + 2];
    let y1 = temp[i + 3];
    let dx = x1 - temp[i];
    let dy = y1 - temp[i + 1];
    let r = atan2(dy, dx);
    out[o] = x1 + p * cos(r);
    out[o + 1] = y1 + p * sin(r);
    out[o + 2] = r;
}

#[allow(clippy::too_many_arguments)]
fn add_curve_position(
    p: f32,
    x1: f32,
    y1: f32,
    cx1: f32,
    cy1: f32,
    cx2: f32,
    cy2: f32,
    x2: f32,
    y2: f32,
    out: &mut [f32],
    o: usize,
    tangents: bool,
) {
    if p == 0.0 || p.is_nan() {
        out[o] = x1;
        out[o + 1] = y1;
        out[o + 2] = atan2(cy1 - y1, cx1 - x1);
        return;
    }
    let tt = p * p;
    let ttt = tt * p;
    let u = 1.0 - p;
    let uu = u * u;
    let uuu = uu * u;
    let ut = u * p;
    let ut3 = ut * 3.0;
    let uut3 = u * ut3;
    let utt3 = ut3 * p;
    let x = x1 * uuu + cx1 * uut3 + cx2 * utt3 + x2 * ttt;
    let y = y1 * uuu + cy1 * uut3 + cy2 * utt3 + y2 * ttt;
    out[o] = x;
    out[o + 1] = y;
    if tangents {
        out[o + 2] = if p < 0.001 {
            atan2(cy1 - y1, cx1 - x1)
        } else {
            atan2(y - (y1 * uu + cy1 * ut * 2.0 + cy2 * tt), x - (x1 * uu + cx1 * ut * 2.0 + cx2 * tt))
        };
    }
}

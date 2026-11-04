//! Writer for the Spine 4.2 binary format: the exact inverse of `binary::read_skeleton`
//! (without nonessential data). The pack compiler transforms a `SkeletonData` (culling,
//! resampling) and writes it back, so the watch reuses the same reader and the result can still
//! be checked against the reference runtime.

use alloc::string::String;
use alloc::vec::Vec;

use crate::data::*;

struct Out {
    buf: Vec<u8>,
    strings: Vec<String>,
}

impl Out {
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }

    fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    fn f32(&mut self, v: f32) {
        self.i32(v.to_bits() as i32);
    }

    fn varint(&mut self, v: i32, optimize_positive: bool) {
        let mut v = if optimize_positive { v as u32 } else { ((v << 1) ^ (v >> 31)) as u32 };
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(b);
                return;
            }
            self.buf.push(b | 0x80);
        }
    }

    fn count(&mut self, n: usize) {
        self.varint(n as i32, true);
    }

    fn string(&mut self, s: Option<&str>) {
        match s {
            None => self.varint(0, true),
            Some(s) => {
                self.varint(s.len() as i32 + 1, true);
                self.buf.extend_from_slice(s.as_bytes());
            }
        }
    }

    fn string_ref(&mut self, s: Option<&str>) {
        match s {
            None => self.varint(0, true),
            Some(s) => {
                let i = match self.strings.iter().position(|x| x == s) {
                    Some(i) => i,
                    None => panic!("string {s:?} missing from the string table"),
                };
                self.varint(i as i32 + 1, true);
            }
        }
    }
}

fn rgba8888(c: Color) -> u32 {
    let q = |v: f32| ((crate::math::clamp(v, 0.0, 1.0) * 255.0) + 0.5) as u32;
    (q(c.r) << 24) | (q(c.g) << 16) | (q(c.b) << 8) | q(c.a)
}

fn rgb888(c: Color) -> u32 {
    let q = |v: f32| ((crate::math::clamp(v, 0.0, 1.0) * 255.0) + 0.5) as u32;
    (q(c.r) << 16) | (q(c.g) << 8) | q(c.b)
}

/// Collects every string the format stores by reference.
fn string_table(d: &SkeletonData) -> Vec<String> {
    let mut t: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if !t.iter().any(|x| x == s) {
            t.push(String::from(s));
        }
    };
    for s in &d.slots {
        if let Some(a) = &s.attachment {
            add(a);
        }
    }
    for skin in &d.skins {
        for (_, key, id) in &skin.attachments {
            add(key);
            let a = &d.attachments[*id as usize];
            add(&a.name);
            match &a.kind {
                AttachmentKind::Region(r) => add(&r.path),
                AttachmentKind::Mesh(m) => {
                    add(&m.path);
                    if let Some(p) = m.parent_mesh {
                        add(&d.attachments[p as usize].name);
                        // The parent is looked up by its skin key; keys are added above.
                    }
                }
                _ => {}
            }
        }
    }
    for a in &d.animations {
        for tl in &a.timelines {
            if let Timeline::Attachment { names, .. } = tl {
                for n in names.iter().flatten() {
                    add(n);
                }
            }
        }
    }
    t
}

fn write_vertices(o: &mut Out, v: &Vertices, scale: f32) {
    let count = v.vertex_count();
    o.count(count);
    if !v.is_weighted() {
        for x in &v.vertices {
            o.f32(*x / scale);
        }
        return;
    }
    let (mut b, mut w) = (0usize, 0usize);
    for _ in 0..count {
        let n = v.bones[b] as usize;
        b += 1;
        o.count(n);
        for _ in 0..n {
            o.varint(v.bones[b] as i32, true);
            b += 1;
            o.f32(v.vertices[w] / scale);
            o.f32(v.vertices[w + 1] / scale);
            o.f32(v.vertices[w + 2]);
            w += 3;
        }
    }
}

fn write_sequence(o: &mut Out, s: &Sequence) {
    o.varint(s.count as i32, true);
    o.varint(s.start as i32, true);
    o.varint(s.digits as i32, true);
    o.varint(s.setup_index as i32, true);
}

/// Finds the skin key a linked mesh's parent is stored under.
fn parent_key<'a>(d: &'a SkeletonData, parent: u32) -> Option<(usize, &'a str)> {
    for (si, skin) in d.skins.iter().enumerate() {
        for (_, key, id) in &skin.attachments {
            if *id == parent {
                return Some((si, key.as_str()));
            }
        }
    }
    None
}

fn write_attachment(o: &mut Out, d: &SkeletonData, key: &str, id: u32, scale: f32) {
    let a = &d.attachments[id as usize];
    let name_differs = a.name != key;
    match &a.kind {
        AttachmentKind::Region(r) => {
            let path = (r.path != a.name).then_some(r.path.as_str());
            let color = rgba8888(r.color);
            let mut flags = 0u8;
            if name_differs {
                flags |= 8;
            }
            if path.is_some() {
                flags |= 16;
            }
            if color != 0xffff_ffff {
                flags |= 32;
            }
            if r.sequence.is_some() {
                flags |= 64;
            }
            if r.rotation != 0.0 {
                flags |= 128;
            }
            o.u8(flags);
            if name_differs {
                o.string_ref(Some(&a.name));
            }
            if let Some(p) = path {
                o.string_ref(Some(p));
            }
            if flags & 32 != 0 {
                o.i32(color as i32);
            }
            if let Some(s) = &r.sequence {
                write_sequence(o, s);
            }
            if flags & 128 != 0 {
                o.f32(r.rotation);
            }
            o.f32(r.x / scale);
            o.f32(r.y / scale);
            o.f32(r.scale_x);
            o.f32(r.scale_y);
            o.f32(r.width / scale);
            o.f32(r.height / scale);
        }
        AttachmentKind::Mesh(m) => {
            let path = (m.path != a.name).then_some(m.path.as_str());
            let color = rgba8888(m.color);
            if let Some(parent) = m.parent_mesh {
                let mut flags = 3u8;
                if name_differs {
                    flags |= 8;
                }
                if path.is_some() {
                    flags |= 16;
                }
                if color != 0xffff_ffff {
                    flags |= 32;
                }
                if m.sequence.is_some() {
                    flags |= 64;
                }
                if a.timeline_attachment == parent {
                    flags |= 128;
                }
                o.u8(flags);
                if name_differs {
                    o.string_ref(Some(&a.name));
                }
                if let Some(p) = path {
                    o.string_ref(Some(p));
                }
                if flags & 32 != 0 {
                    o.i32(color as i32);
                }
                if let Some(s) = &m.sequence {
                    write_sequence(o, s);
                }
                let (skin, pkey) = parent_key(d, parent).expect("linked mesh parent in a skin");
                o.count(skin);
                o.string_ref(Some(pkey));
                return;
            }
            let mut flags = 2u8;
            if name_differs {
                flags |= 8;
            }
            if path.is_some() {
                flags |= 16;
            }
            if color != 0xffff_ffff {
                flags |= 32;
            }
            if m.sequence.is_some() {
                flags |= 64;
            }
            if m.vertices.is_weighted() {
                flags |= 128;
            }
            o.u8(flags);
            if name_differs {
                o.string_ref(Some(&a.name));
            }
            if let Some(p) = path {
                o.string_ref(Some(p));
            }
            if flags & 32 != 0 {
                o.i32(color as i32);
            }
            if let Some(s) = &m.sequence {
                write_sequence(o, s);
            }
            o.count((m.hull_length >> 1) as usize);
            write_vertices(o, &m.vertices, scale);
            for uv in &m.region_uvs {
                o.f32(*uv);
            }
            for t in &m.triangles {
                o.varint(*t as i32, true);
            }
        }
        AttachmentKind::BoundingBox(v) => {
            let mut flags = 1u8;
            if name_differs {
                flags |= 8;
            }
            if v.is_weighted() {
                flags |= 16;
            }
            o.u8(flags);
            if name_differs {
                o.string_ref(Some(&a.name));
            }
            write_vertices(o, v, scale);
        }
        AttachmentKind::Path(p) => {
            let mut flags = 4u8;
            if name_differs {
                flags |= 8;
            }
            if p.closed {
                flags |= 16;
            }
            if p.constant_speed {
                flags |= 32;
            }
            if p.vertices.is_weighted() {
                flags |= 64;
            }
            o.u8(flags);
            if name_differs {
                o.string_ref(Some(&a.name));
            }
            write_vertices(o, &p.vertices, scale);
            for l in &p.lengths {
                o.f32(*l / scale);
            }
        }
        AttachmentKind::Point(p) => {
            let mut flags = 5u8;
            if name_differs {
                flags |= 8;
            }
            o.u8(flags);
            if name_differs {
                o.string_ref(Some(&a.name));
            }
            o.f32(p.rotation);
            o.f32(p.x / scale);
            o.f32(p.y / scale);
        }
        AttachmentKind::Clipping(c) => {
            let mut flags = 6u8;
            if name_differs {
                flags |= 8;
            }
            if c.vertices.is_weighted() {
                flags |= 16;
            }
            o.u8(flags);
            if name_differs {
                o.string_ref(Some(&a.name));
            }
            o.count(c.end_slot as usize);
            write_vertices(o, &c.vertices, scale);
        }
    }
}

fn write_skin_attachments(o: &mut Out, d: &SkeletonData, skin: &Skin, scale: f32) {
    // Group by slot (attachments are sorted by slot, then name).
    let mut slots: Vec<u16> = skin.attachments.iter().map(|(s, _, _)| *s).collect();
    slots.dedup();
    o.count(slots.len());
    for s in slots {
        o.count(s as usize);
        let entries: Vec<&(u16, String, u32)> = skin.attachments.iter().filter(|(x, _, _)| *x == s).collect();
        o.count(entries.len());
        for (_, key, id) in entries {
            o.string_ref(Some(key));
            write_attachment(o, d, key, *id, scale);
        }
    }
}

fn write_curve_values(o: &mut Out, c: &Curve, scales: &[f32]) {
    let e = c.entries as usize;
    let n = c.frame_count();
    let fc = n;
    // Frame 0, then per segment: the next frame's values followed by the segment's curve.
    for frame in 0..n {
        let base = frame * e;
        o.f32(c.frames[base]);
        for (k, s) in scales.iter().enumerate() {
            o.f32(c.frames[base + 1 + k] / s);
        }
        if frame > 0 {
            write_curve_type(o, c, frame - 1, fc, scales.len(), scales);
        }
    }
}

/// Writes the segment's curve type and, for Beziers, the control points recovered from the
/// sampled table (the reader re-samples them identically).
fn write_curve_type(o: &mut Out, c: &Curve, frame: usize, frame_count: usize, values: usize, scales: &[f32]) {
    let t = c.curves[frame];
    if t == CURVE_LINEAR {
        o.u8(0);
    } else if t == CURVE_STEPPED {
        o.u8(1);
    } else {
        o.u8(2);
        let _ = frame_count;
        for k in 0..values {
            let ctrl = c.bezier_controls(frame, k);
            o.f32(ctrl[0]);
            o.f32(ctrl[1] / scales[k]);
            o.f32(ctrl[2]);
            o.f32(ctrl[3] / scales[k]);
        }
    }
}

/// Number of Bezier segments a curve stores (per channel, as the file counts them).
fn bezier_count(c: &Curve) -> usize {
    c.bezier_count()
}

fn write_color_curve(o: &mut Out, c: &Curve, n: usize) {
    let e = c.entries as usize;
    let frames = c.frame_count();
    for frame in 0..frames {
        let base = frame * e;
        o.f32(c.frames[base]);
        for k in 0..n {
            o.u8(((crate::math::clamp(c.frames[base + 1 + k], 0.0, 1.0)) * 255.0 + 0.5) as u8);
        }
        if frame > 0 {
            write_curve_type(o, c, frame - 1, frames, n, &[1.0; 8][..n]);
        }
    }
}

/// Serializes `d` (scaled by 1/`scale` so that reading it back with `scale` restores it).
pub fn write_skeleton(d: &SkeletonData, scale: f32) -> Vec<u8> {
    let strings = string_table(d);
    let mut o = Out { buf: Vec::new(), strings };
    o.i32(0);
    o.i32(0);
    o.string(Some(d.version.as_deref().unwrap_or("4.2.33")));
    o.f32(d.x / scale);
    o.f32(d.y / scale);
    o.f32(d.width / scale);
    o.f32(d.height / scale);
    o.f32(d.reference_scale / scale);
    o.bool(false); // nonessential
    let strings = o.strings.clone();
    o.count(strings.len());
    for s in &strings {
        o.string(Some(s));
    }

    o.count(d.bones.len());
    for (i, b) in d.bones.iter().enumerate() {
        o.string(Some(&b.name));
        if i > 0 {
            o.count(b.parent.unwrap_or(0) as usize);
        }
        o.f32(b.rotation);
        o.f32(b.x / scale);
        o.f32(b.y / scale);
        o.f32(b.scale_x);
        o.f32(b.scale_y);
        o.f32(b.shear_x);
        o.f32(b.shear_y);
        o.f32(b.length / scale);
        o.u8(b.inherit as u8);
        o.bool(b.skin_required);
    }

    o.count(d.slots.len());
    for s in &d.slots {
        o.string(Some(&s.name));
        o.count(s.bone as usize);
        o.i32(rgba8888(s.color) as i32);
        o.i32(match s.dark {
            None => -1,
            Some(c) => rgb888(c) as i32,
        });
        o.string_ref(s.attachment.as_deref());
        o.count(s.blend as usize);
    }

    o.count(d.ik_constraints.len());
    for c in &d.ik_constraints {
        o.string(Some(&c.name));
        o.count(c.order as usize);
        o.count(c.bones.len());
        for b in &c.bones {
            o.count(*b as usize);
        }
        o.count(c.target as usize);
        let mut flags = 0u8;
        if c.skin_required {
            flags |= 1;
        }
        if c.bend_direction > 0 {
            flags |= 2;
        }
        if c.compress {
            flags |= 4;
        }
        if c.stretch {
            flags |= 8;
        }
        if c.uniform {
            flags |= 16;
        }
        if c.mix != 0.0 {
            flags |= 32;
            if c.mix != 1.0 {
                flags |= 64;
            }
        }
        if c.softness != 0.0 {
            flags |= 128;
        }
        o.u8(flags);
        if flags & 64 != 0 {
            o.f32(c.mix);
        }
        if flags & 128 != 0 {
            o.f32(c.softness / scale);
        }
    }

    o.count(d.transform_constraints.len());
    for c in &d.transform_constraints {
        o.string(Some(&c.name));
        o.count(c.order as usize);
        o.count(c.bones.len());
        for b in &c.bones {
            o.count(*b as usize);
        }
        o.count(c.target as usize);
        let mut f1 = 0u8;
        if c.skin_required {
            f1 |= 1;
        }
        if c.local {
            f1 |= 2;
        }
        if c.relative {
            f1 |= 4;
        }
        let opt1 = [c.offset_rotation, c.offset_x / scale, c.offset_y / scale, c.offset_scale_x, c.offset_scale_y];
        for (k, v) in opt1.iter().enumerate() {
            if *v != 0.0 {
                f1 |= 8 << k;
            }
        }
        o.u8(f1);
        for v in opt1 {
            if v != 0.0 {
                o.f32(v);
            }
        }
        let opt2 = [c.offset_shear_y, c.mix_rotate, c.mix_x, c.mix_y, c.mix_scale_x, c.mix_scale_y, c.mix_shear_y];
        let mut f2 = 0u8;
        for (k, v) in opt2.iter().enumerate() {
            if *v != 0.0 {
                f2 |= 1 << k;
            }
        }
        o.u8(f2);
        for v in opt2 {
            if v != 0.0 {
                o.f32(v);
            }
        }
    }

    o.count(d.path_constraints.len());
    for c in &d.path_constraints {
        o.string(Some(&c.name));
        o.count(c.order as usize);
        o.bool(c.skin_required);
        o.count(c.bones.len());
        for b in &c.bones {
            o.count(*b as usize);
        }
        o.count(c.target as usize);
        let mut flags = (c.position_mode as u8) | ((c.spacing_mode as u8) << 1) | ((c.rotate_mode as u8) << 3);
        if c.offset_rotation != 0.0 {
            flags |= 128;
        }
        o.u8(flags);
        if flags & 128 != 0 {
            o.f32(c.offset_rotation);
        }
        o.f32(if c.position_mode == PositionMode::Fixed { c.position / scale } else { c.position });
        o.f32(if matches!(c.spacing_mode, SpacingMode::Length | SpacingMode::Fixed) { c.spacing / scale } else { c.spacing });
        o.f32(c.mix_rotate);
        o.f32(c.mix_x);
        o.f32(c.mix_y);
    }

    o.count(d.physics_constraints.len());
    for c in &d.physics_constraints {
        o.string(Some(&c.name));
        o.count(c.order as usize);
        o.count(c.bone as usize);
        let mut f = 0u8;
        if c.skin_required {
            f |= 1;
        }
        let opt = [c.x, c.y, c.rotate, c.scale_x, c.shear_x];
        for (k, v) in opt.iter().enumerate() {
            if *v != 0.0 {
                f |= 2 << k;
            }
        }
        let limit = c.limit / scale;
        if limit != 5000.0 {
            f |= 64;
        }
        if c.mass_inverse != 1.0 {
            f |= 128;
        }
        o.u8(f);
        for v in opt {
            if v != 0.0 {
                o.f32(v);
            }
        }
        if f & 64 != 0 {
            o.f32(limit);
        }
        o.u8((1.0 / c.step + 0.5) as u8);
        o.f32(c.inertia);
        o.f32(c.strength);
        o.f32(c.damping);
        if f & 128 != 0 {
            o.f32(c.mass_inverse);
        }
        o.f32(c.wind);
        o.f32(c.gravity);
        let mut g = 0u8;
        for (k, b) in [c.inertia_global, c.strength_global, c.damping_global, c.mass_global, c.wind_global, c.gravity_global, c.mix_global]
            .iter()
            .enumerate()
        {
            if *b {
                g |= 1 << k;
            }
        }
        if c.mix != 1.0 {
            g |= 128;
        }
        o.u8(g);
        if g & 128 != 0 {
            o.f32(c.mix);
        }
    }

    // Default skin, then the named skins.
    let named_start = if d.has_default_skin {
        let skin = &d.skins[0];
        write_skin_attachments(&mut o, d, skin, scale);
        1
    } else {
        o.count(0);
        0
    };
    o.count(d.skins.len() - named_start);
    for skin in &d.skins[named_start..] {
        o.string(Some(&skin.name));
        o.count(skin.bones.len());
        for b in &skin.bones {
            o.count(*b as usize);
        }
        for kind in 0..4 {
            let list: Vec<u16> = skin
                .constraints
                .iter()
                .filter_map(|c| match (kind, c) {
                    (0, ConstraintRef::Ik(i)) | (1, ConstraintRef::Transform(i)) | (2, ConstraintRef::Path(i)) | (3, ConstraintRef::Physics(i)) => Some(*i),
                    _ => None,
                })
                .collect();
            o.count(list.len());
            for i in list {
                o.count(i as usize);
            }
        }
        write_skin_attachments(&mut o, d, skin, scale);
    }

    o.count(d.events.len());
    for e in &d.events {
        o.string(Some(&e.name));
        o.varint(e.int_value, false);
        o.f32(e.float_value);
        o.string(e.string_value.as_deref());
        o.string(e.audio_path.as_deref());
        if e.audio_path.as_deref().is_some_and(|p| !p.is_empty()) {
            o.f32(e.volume);
            o.f32(e.balance);
        }
    }

    o.count(d.animations.len());
    for a in &d.animations {
        o.string(Some(&a.name));
        write_animation(&mut o, d, a, scale);
    }
    o.buf
}

fn write_animation(o: &mut Out, d: &SkeletonData, a: &Animation, scale: f32) {
    o.count(a.timelines.len());
    // Slot timelines grouped by slot.
    let slot_tl: Vec<&Timeline> = a.timelines.iter().filter(|t| matches!(t, Timeline::Attachment { .. } | Timeline::SlotColor { .. })).collect();
    let mut slots: Vec<u16> = slot_tl
        .iter()
        .map(|t| match t {
            Timeline::Attachment { slot, .. } | Timeline::SlotColor { slot, .. } => *slot,
            _ => 0,
        })
        .collect();
    slots.sort_unstable();
    slots.dedup();
    o.count(slots.len());
    for s in slots {
        o.count(s as usize);
        let tls: Vec<&&Timeline> = slot_tl
            .iter()
            .filter(|t| matches!(t, Timeline::Attachment { slot, .. } | Timeline::SlotColor { slot, .. } if *slot == s))
            .collect();
        o.count(tls.len());
        for t in tls {
            match t {
                Timeline::Attachment { frames, names, .. } => {
                    o.u8(0);
                    o.count(frames.len());
                    for (f, n) in frames.iter().zip(names) {
                        o.f32(*f);
                        o.string_ref(n.as_deref());
                    }
                }
                Timeline::SlotColor { kind, curve, .. } => {
                    let (ty, n) = match kind {
                        SlotColorKind::Rgba => (1, 4),
                        SlotColorKind::Rgb => (2, 3),
                        SlotColorKind::Rgba2 => (3, 7),
                        SlotColorKind::Rgb2 => (4, 6),
                        SlotColorKind::Alpha => (5, 1),
                    };
                    o.u8(ty);
                    o.count(curve.frame_count());
                    o.count(bezier_count(curve));
                    write_color_curve(o, curve, n);
                }
                _ => unreachable!(),
            }
        }
    }

    // Bone timelines grouped by bone.
    let bone_tl: Vec<&Timeline> = a.timelines.iter().filter(|t| matches!(t, Timeline::Bone { .. } | Timeline::Inherit { .. })).collect();
    let mut bones: Vec<u16> = bone_tl
        .iter()
        .map(|t| match t {
            Timeline::Bone { bone, .. } | Timeline::Inherit { bone, .. } => *bone,
            _ => 0,
        })
        .collect();
    bones.sort_unstable();
    bones.dedup();
    o.count(bones.len());
    for b in bones {
        o.count(b as usize);
        let tls: Vec<&&Timeline> = bone_tl
            .iter()
            .filter(|t| matches!(t, Timeline::Bone { bone, .. } | Timeline::Inherit { bone, .. } if *bone == b))
            .collect();
        o.count(tls.len());
        for t in tls {
            match t {
                Timeline::Inherit { frames, values, .. } => {
                    o.u8(10);
                    o.count(frames.len());
                    for (f, v) in frames.iter().zip(values) {
                        o.f32(*f);
                        o.u8(*v as u8);
                    }
                }
                Timeline::Bone { property, curve, .. } => {
                    let (ty, scales): (u8, &[f32]) = match property {
                        BoneProperty::Rotate => (0, &[1.0]),
                        BoneProperty::Translate => (1, &[scale, scale]),
                        BoneProperty::TranslateX => (2, &[scale]),
                        BoneProperty::TranslateY => (3, &[scale]),
                        BoneProperty::Scale => (4, &[1.0, 1.0]),
                        BoneProperty::ScaleX => (5, &[1.0]),
                        BoneProperty::ScaleY => (6, &[1.0]),
                        BoneProperty::Shear => (7, &[1.0, 1.0]),
                        BoneProperty::ShearX => (8, &[1.0]),
                        BoneProperty::ShearY => (9, &[1.0]),
                    };
                    let mut s = [0f32; 2];
                    s[..scales.len()].copy_from_slice(scales);
                    o.u8(ty);
                    o.count(curve.frame_count());
                    o.count(bezier_count(curve));
                    write_curve_values(o, curve, &s[..scales.len()]);
                }
                _ => unreachable!(),
            }
        }
    }

    // IK timelines.
    let ik: Vec<&Timeline> = a.timelines.iter().filter(|t| matches!(t, Timeline::Ik { .. })).collect();
    o.count(ik.len());
    for t in ik {
        let Timeline::Ik { constraint, curve } = t else { unreachable!() };
        o.count(*constraint as usize);
        let n = curve.frame_count();
        o.count(n);
        o.count(bezier_count(curve));
        for frame in 0..n {
            let f = &curve.frames[frame * 6..frame * 6 + 6];
            let mut flags = 0u8;
            if f[1] != 0.0 {
                flags |= 1;
                if f[1] != 1.0 {
                    flags |= 2;
                }
            }
            if f[2] != 0.0 {
                flags |= 4;
            }
            if f[3] > 0.0 {
                flags |= 8;
            }
            if f[4] != 0.0 {
                flags |= 16;
            }
            if f[5] != 0.0 {
                flags |= 32;
            }
            if frame > 0 {
                let ct = curve.curves[frame - 1];
                if ct == CURVE_STEPPED {
                    flags |= 64;
                } else if ct != CURVE_LINEAR {
                    flags |= 128;
                }
            }
            o.u8(flags);
            o.f32(f[0]);
            if flags & 2 != 0 {
                o.f32(f[1]);
            }
            if flags & 4 != 0 {
                o.f32(f[2] / scale);
            }
            if frame > 0 && flags & 128 != 0 {
                for (k, sc) in [1.0, scale].iter().enumerate() {
                    let ctrl = curve.bezier_controls(frame - 1, k);
                    o.f32(ctrl[0]);
                    o.f32(ctrl[1] / sc);
                    o.f32(ctrl[2]);
                    o.f32(ctrl[3] / sc);
                }
            }
        }
    }

    let tc: Vec<&Timeline> = a.timelines.iter().filter(|t| matches!(t, Timeline::Transform { .. })).collect();
    o.count(tc.len());
    for t in tc {
        let Timeline::Transform { constraint, curve } = t else { unreachable!() };
        o.count(*constraint as usize);
        o.count(curve.frame_count());
        o.count(bezier_count(curve));
        write_curve_values(o, curve, &[1.0; 6]);
    }

    // Path timelines grouped by constraint.
    let pt: Vec<&Timeline> = a.timelines.iter().filter(|t| matches!(t, Timeline::Path { .. })).collect();
    let mut pcs: Vec<u16> = pt.iter().map(|t| if let Timeline::Path { constraint, .. } = t { *constraint } else { 0 }).collect();
    pcs.sort_unstable();
    pcs.dedup();
    o.count(pcs.len());
    for c in pcs {
        o.count(c as usize);
        let data = &d.path_constraints[c as usize];
        let tls: Vec<&&Timeline> = pt.iter().filter(|t| matches!(t, Timeline::Path { constraint, .. } if *constraint == c)).collect();
        o.count(tls.len());
        for t in tls {
            let Timeline::Path { property, curve, .. } = t else { unreachable!() };
            let (ty, s): (u8, [f32; 3]) = match property {
                PathProperty::Position => (0, [if data.position_mode == PositionMode::Fixed { scale } else { 1.0 }, 1.0, 1.0]),
                PathProperty::Spacing => (
                    1,
                    [if matches!(data.spacing_mode, SpacingMode::Length | SpacingMode::Fixed) { scale } else { 1.0 }, 1.0, 1.0],
                ),
                PathProperty::Mix => (2, [1.0, 1.0, 1.0]),
            };
            let n = if ty == 2 { 3 } else { 1 };
            o.u8(ty);
            o.count(curve.frame_count());
            o.count(bezier_count(curve));
            write_curve_values(o, curve, &s[..n]);
        }
    }

    // Physics timelines grouped by constraint (index + 1, 0 = all).
    let ph: Vec<&Timeline> = a.timelines.iter().filter(|t| matches!(t, Timeline::Physics { .. } | Timeline::PhysicsReset { .. })).collect();
    let key = |t: &Timeline| -> i32 {
        match t {
            Timeline::Physics { constraint, .. } | Timeline::PhysicsReset { constraint, .. } => constraint.map_or(0, |c| c as i32 + 1),
            _ => 0,
        }
    };
    let mut keys: Vec<i32> = ph.iter().map(|t| key(t)).collect();
    keys.sort_unstable();
    keys.dedup();
    o.count(keys.len());
    for k in keys {
        o.varint(k, true);
        let tls: Vec<&&Timeline> = ph.iter().filter(|t| key(t) == k).collect();
        o.count(tls.len());
        for t in tls {
            match t {
                Timeline::PhysicsReset { frames, .. } => {
                    o.u8(8);
                    o.count(frames.len());
                    for f in frames {
                        o.f32(*f);
                    }
                }
                Timeline::Physics { property, curve, .. } => {
                    let ty = match property {
                        PhysicsProperty::Inertia => 0,
                        PhysicsProperty::Strength => 1,
                        PhysicsProperty::Damping => 2,
                        PhysicsProperty::Mass => 4,
                        PhysicsProperty::Wind => 5,
                        PhysicsProperty::Gravity => 6,
                        PhysicsProperty::Mix => 7,
                    };
                    o.u8(ty);
                    o.count(curve.frame_count());
                    o.count(bezier_count(curve));
                    write_curve_values(o, curve, &[1.0]);
                }
                _ => unreachable!(),
            }
        }
    }

    // Attachment timelines: skin -> slot -> attachment key.
    let at: Vec<&Timeline> = a.timelines.iter().filter(|t| matches!(t, Timeline::Deform { .. } | Timeline::Sequence { .. })).collect();
    // Find the (skin, key) each timeline's attachment is stored under.
    let mut entries: Vec<(usize, u16, &str, &Timeline)> = Vec::new();
    for t in &at {
        let (slot, att) = match t {
            Timeline::Deform { slot, attachment, .. } | Timeline::Sequence { slot, attachment, .. } => (*slot, *attachment),
            _ => unreachable!(),
        };
        let (skin, k) = d
            .skins
            .iter()
            .enumerate()
            .find_map(|(si, s)| s.attachments.iter().find(|(sl, _, id)| *sl == slot && *id == att).map(|(_, k, _)| (si, k.as_str())))
            .expect("timeline attachment in a skin");
        entries.push((skin, slot, k, t));
    }
    let mut skins: Vec<usize> = entries.iter().map(|e| e.0).collect();
    skins.sort_unstable();
    skins.dedup();
    o.count(skins.len());
    for s in skins {
        o.count(s);
        let mut slots: Vec<u16> = entries.iter().filter(|e| e.0 == s).map(|e| e.1).collect();
        slots.sort_unstable();
        slots.dedup();
        o.count(slots.len());
        for sl in slots {
            o.count(sl as usize);
            let es: Vec<&(usize, u16, &str, &Timeline)> = entries.iter().filter(|e| e.0 == s && e.1 == sl).collect();
            o.count(es.len());
            for (_, _, k, t) in es {
                o.string_ref(Some(k));
                match t {
                    Timeline::Deform { attachment, curve, vertices, .. } => {
                        let verts = d.attachments[*attachment as usize].vertices().unwrap();
                        let weighted = verts.is_weighted();
                        o.u8(0);
                        let n = curve.frame_count();
                        o.count(n);
                        o.count(bezier_count(curve));
                        if n > 0 {
                            o.f32(curve.frames[0]);
                        }
                        for frame in 0..n {
                            let v = &vertices[frame];
                            // Offsets from setup (unweighted) or raw offsets (weighted), trimmed of zeros.
                            let off: Vec<f32> = if weighted {
                                v.clone()
                            } else {
                                v.iter().zip(verts.vertices.iter()).map(|(a, b)| a - b).collect()
                            };
                            let start = off.iter().position(|x| *x != 0.0);
                            match start {
                                None => o.count(0),
                                Some(st) => {
                                    let end = off.iter().rposition(|x| *x != 0.0).unwrap() + 1;
                                    o.count(end - st);
                                    o.count(st);
                                    for x in &off[st..end] {
                                        o.f32(*x / scale);
                                    }
                                }
                            }
                            if frame + 1 == n {
                                break;
                            }
                            o.f32(curve.frames[frame + 1]);
                            let ct = curve.curves[frame];
                            if ct == CURVE_LINEAR {
                                o.u8(0);
                            } else if ct == CURVE_STEPPED {
                                o.u8(1);
                            } else {
                                o.u8(2);
                                let ctrl = deform_controls(curve, frame);
                                for c in ctrl {
                                    o.f32(c);
                                }
                            }
                        }
                    }
                    Timeline::Sequence { frames, .. } => {
                        o.u8(1);
                        o.count(frames.len() / 3);
                        for f in frames.chunks_exact(3) {
                            o.f32(f[0]);
                            o.i32(f[1].to_bits() as i32);
                            o.f32(f[2]);
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    // Draw order.
    let dot = a.timelines.iter().find(|t| matches!(t, Timeline::DrawOrder { .. }));
    match dot {
        None => o.count(0),
        Some(Timeline::DrawOrder { frames, orders }) => {
            o.count(frames.len());
            for (f, ord) in frames.iter().zip(orders) {
                o.f32(*f);
                match ord {
                    None => o.count(0),
                    Some(order) => {
                        // order[i] = setup slot drawn at position i; offsets are per moved slot.
                        let mut pos = alloc::vec![0usize; order.len()];
                        for (i, s) in order.iter().enumerate() {
                            pos[*s as usize] = i;
                        }
                        let changed = encode_draw_order(order);
                        o.count(changed.len());
                        for (slot, offset) in changed {
                            o.count(slot);
                            o.varint(offset, true);
                        }
                        let _ = pos;
                    }
                }
            }
        }
        _ => unreachable!(),
    }

    let ev = a.timelines.iter().find(|t| matches!(t, Timeline::Event { .. }));
    match ev {
        None => o.count(0),
        Some(Timeline::Event { events, .. }) => {
            o.count(events.len());
            for e in events {
                let ed = &d.events[e.data as usize];
                o.f32(e.time);
                o.count(e.data as usize);
                o.varint(e.int_value, false);
                o.f32(e.float_value);
                if e.string_value == ed.string_value {
                    o.string(None);
                } else {
                    o.string(e.string_value.as_deref());
                }
                if ed.audio_path.as_deref().is_some_and(|p| !p.is_empty()) {
                    o.f32(e.volume);
                    o.f32(e.balance);
                }
            }
        }
        _ => unreachable!(),
    }
}

/// Deform curves store the percent Bezier (value 0 -> 1).
fn deform_controls(c: &Curve, frame: usize) -> [f32; 4] {
    c.bezier_controls(frame, 0)
}

/// Encodes a draw order (setup slot index per draw position) as Spine's (slot, offset) list,
/// where each listed slot moves from its setup index to setup index + offset and every other
/// slot keeps its relative order.
fn encode_draw_order(order: &[u16]) -> Vec<(usize, i32)> {
    let n = order.len();
    let mut pos = alloc::vec![0usize; n];
    for (i, s) in order.iter().enumerate() {
        pos[*s as usize] = i;
    }
    // Slots whose relative order is preserved form the longest increasing subsequence of
    // positions; the rest are written explicitly.
    let lis = longest_increasing(&pos);
    let mut keep = alloc::vec![false; n];
    for s in lis {
        keep[s] = true;
    }
    let mut out = Vec::new();
    for s in 0..n {
        if !keep[s] {
            out.push((s, pos[s] as i32 - s as i32));
        }
    }
    // The format requires unchanged slots to fill the remaining positions in order; verify by
    // decoding and fall back to listing every slot when the greedy choice does not round-trip.
    if decode_draw_order(n, &out) != order {
        out = (0..n).map(|s| (s, pos[s] as i32 - s as i32)).collect();
    }
    out
}

fn decode_draw_order(n: usize, changed: &[(usize, i32)]) -> Vec<u16> {
    let mut order = alloc::vec![-1i64; n];
    let mut unchanged = Vec::new();
    let mut original = 0usize;
    for &(slot, off) in changed {
        while original != slot {
            unchanged.push(original);
            original += 1;
        }
        let t = original as i64 + off as i64;
        if t < 0 || t as usize >= n {
            return Vec::new();
        }
        order[t as usize] = original as i64;
        original += 1;
    }
    while original < n {
        unchanged.push(original);
        original += 1;
    }
    for i in (0..n).rev() {
        if order[i] == -1 {
            match unchanged.pop() {
                Some(s) => order[i] = s as i64,
                None => return Vec::new(),
            }
        }
    }
    order.into_iter().map(|v| v as u16).collect()
}

fn longest_increasing(v: &[usize]) -> Vec<usize> {
    // Indices (slots) of one longest strictly increasing subsequence of v.
    let n = v.len();
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = alloc::vec![usize::MAX; n];
    for i in 0..n {
        let k = tails.partition_point(|&t| v[t] < v[i]);
        if k > 0 {
            prev[i] = tails[k - 1];
        }
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut out = Vec::new();
    let mut cur = tails.last().copied();
    while let Some(c) = cur {
        out.push(c);
        cur = if prev[c] == usize::MAX { None } else { Some(prev[c]) };
    }
    out.reverse();
    out
}

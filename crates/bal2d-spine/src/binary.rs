//! Reader for the Spine 4.2 binary skeleton format (`.skel`).
//!
//! Field order, flag bits and value scaling follow the published 4.2 format as written by the
//! Spine editor. Only 4.2.x files are accepted: every Blue Archive memorial lobby ships as 4.2.33.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::data::*;

#[derive(Debug, Clone, PartialEq)]
pub enum ReadError {
    Eof,
    Version(Option<String>),
    Invalid(&'static str),
}

impl core::fmt::Display for ReadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReadError::Eof => write!(f, "unexpected end of skeleton data"),
            ReadError::Version(v) => write!(f, "unsupported skeleton version {v:?} (need 4.2.x)"),
            ReadError::Invalid(what) => write!(f, "invalid skeleton data: {what}"),
        }
    }
}

type Result<T> = core::result::Result<T, ReadError>;

struct Input<'a> {
    data: &'a [u8],
    pos: usize,
    strings: Vec<String>,
}

impl<'a> Input<'a> {
    fn u8(&mut self) -> Result<u8> {
        let b = *self.data.get(self.pos).ok_or(ReadError::Eof)?;
        self.pos += 1;
        Ok(b)
    }

    fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }

    fn i32(&mut self) -> Result<i32> {
        let s = self.data.get(self.pos..self.pos + 4).ok_or(ReadError::Eof)?;
        self.pos += 4;
        Ok(i32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.i32()? as u32))
    }

    /// Variable-length int, 7 bits per byte, little-endian groups.
    fn varint(&mut self, optimize_positive: bool) -> Result<i32> {
        let mut result: u32 = 0;
        for shift in [0u32, 7, 14, 21, 28] {
            let b = self.u8()? as u32;
            result |= (b & 0x7f) << shift;
            if b & 0x80 == 0 {
                break;
            }
        }
        Ok(if optimize_positive {
            result as i32
        } else {
            ((result >> 1) as i32) ^ -((result & 1) as i32)
        })
    }

    fn count(&mut self) -> Result<usize> {
        let n = self.varint(true)?;
        if n < 0 || n as usize > self.data.len() {
            return Err(ReadError::Invalid("count"));
        }
        Ok(n as usize)
    }

    fn index(&mut self, len: usize, what: &'static str) -> Result<u16> {
        let i = self.varint(true)?;
        if i < 0 || i as usize >= len {
            return Err(ReadError::Invalid(what));
        }
        Ok(i as u16)
    }

    fn string(&mut self) -> Result<Option<String>> {
        let n = self.varint(true)?;
        match n {
            0 => return Ok(None),
            1 => return Ok(Some(String::new())),
            n if n < 0 => return Err(ReadError::Invalid("string length")),
            _ => {}
        }
        let len = (n - 1) as usize;
        let bytes = self.data.get(self.pos..self.pos + len).ok_or(ReadError::Eof)?;
        self.pos += len;
        Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
    }

    fn string_ref(&mut self) -> Result<Option<String>> {
        let i = self.varint(true)?;
        if i == 0 {
            return Ok(None);
        }
        self.strings
            .get((i - 1) as usize)
            .cloned()
            .map(Some)
            .ok_or(ReadError::Invalid("string reference"))
    }

    fn required_string(&mut self, what: &'static str) -> Result<String> {
        self.string()?.ok_or(ReadError::Invalid(what))
    }
}

const BONE_ROTATE: u8 = 0;
const BONE_TRANSLATE: u8 = 1;
const BONE_TRANSLATEX: u8 = 2;
const BONE_TRANSLATEY: u8 = 3;
const BONE_SCALE: u8 = 4;
const BONE_SCALEX: u8 = 5;
const BONE_SCALEY: u8 = 6;
const BONE_SHEAR: u8 = 7;
const BONE_SHEARX: u8 = 8;
const BONE_SHEARY: u8 = 9;
const BONE_INHERIT: u8 = 10;

const SLOT_ATTACHMENT: u8 = 0;
const SLOT_RGBA: u8 = 1;
const SLOT_RGB: u8 = 2;
const SLOT_RGBA2: u8 = 3;
const SLOT_RGB2: u8 = 4;
const SLOT_ALPHA: u8 = 5;

const ATTACHMENT_DEFORM: u8 = 0;
const ATTACHMENT_SEQUENCE: u8 = 1;

const PATH_POSITION: u8 = 0;
const PATH_SPACING: u8 = 1;
const PATH_MIX: u8 = 2;

const PHYSICS_INERTIA: u8 = 0;
const PHYSICS_STRENGTH: u8 = 1;
const PHYSICS_DAMPING: u8 = 2;
const PHYSICS_MASS: u8 = 4;
const PHYSICS_WIND: u8 = 5;
const PHYSICS_GRAVITY: u8 = 6;
const PHYSICS_MIX: u8 = 7;
const PHYSICS_RESET: u8 = 8;

const CURVE_TYPE_STEPPED: u8 = 1;
const CURVE_TYPE_BEZIER: u8 = 2;

struct LinkedMesh {
    attachment: u32,
    skin: usize,
    slot: u16,
    parent: String,
    inherit_timelines: bool,
}

/// Reads a 4.2 binary skeleton. `scale` multiplies positions, sizes and translations.
pub fn read_skeleton(bytes: &[u8], scale: f32) -> Result<SkeletonData> {
    let mut input = Input { data: bytes, pos: 0, strings: Vec::new() };
    let mut d = SkeletonData::default();

    let low = input.i32()?;
    let high = input.i32()?;
    d.hash = if low == 0 && high == 0 {
        None
    } else {
        Some(alloc::format!("{:x}{:x}", high as u32, low as u32))
    };
    d.version = input.string()?;
    if !d.version.as_deref().is_some_and(|v| v.starts_with("4.2")) {
        return Err(ReadError::Version(d.version));
    }
    d.x = input.f32()?;
    d.y = input.f32()?;
    d.width = input.f32()?;
    d.height = input.f32()?;
    d.reference_scale = input.f32()? * scale;
    let nonessential = input.bool()?;
    if nonessential {
        d.fps = input.f32()?;
        input.string()?; // images path
        input.string()?; // audio path
    }

    let n = input.count()?;
    for _ in 0..n {
        let s = input.required_string("string table entry")?;
        input.strings.push(s);
    }

    // Bones.
    let n = input.count()?;
    for i in 0..n {
        let name = input.required_string("bone name")?;
        let parent = if i == 0 { None } else { Some(input.index(d.bones.len(), "bone parent")?) };
        let rotation = input.f32()?;
        let x = input.f32()? * scale;
        let y = input.f32()? * scale;
        let scale_x = input.f32()?;
        let scale_y = input.f32()?;
        let shear_x = input.f32()?;
        let shear_y = input.f32()?;
        let length = input.f32()? * scale;
        let inherit = Inherit::from_u8(input.u8()?).ok_or(ReadError::Invalid("bone inherit"))?;
        let skin_required = input.bool()?;
        if nonessential {
            input.i32()?; // color
            input.string()?; // icon
            input.bool()?; // visible
        }
        d.bones.push(BoneData {
            name,
            parent,
            length,
            x,
            y,
            rotation,
            scale_x,
            scale_y,
            shear_x,
            shear_y,
            inherit,
            skin_required,
        });
    }

    // Slots.
    let n = input.count()?;
    for _ in 0..n {
        let name = input.required_string("slot name")?;
        let bone = input.index(d.bones.len(), "slot bone")?;
        let color = Color::from_rgba8888(input.i32()? as u32);
        let dark = input.i32()?;
        let dark = if dark == -1 { None } else { Some(Color::from_rgb888(dark as u32)) };
        let attachment = input.string_ref()?;
        let blend = BlendMode::from_u32(input.varint(true)? as u32).ok_or(ReadError::Invalid("blend mode"))?;
        if nonessential {
            input.bool()?; // visible
        }
        d.slots.push(SlotData { name, bone, color, dark, attachment, blend });
    }

    // IK constraints.
    let n = input.count()?;
    for _ in 0..n {
        let name = input.required_string("ik name")?;
        let order = input.varint(true)? as u32;
        let nn = input.count()?;
        let mut bones = Vec::with_capacity(nn);
        for _ in 0..nn {
            bones.push(input.index(d.bones.len(), "ik bone")?);
        }
        let target = input.index(d.bones.len(), "ik target")?;
        let flags = input.u8()?;
        // Without flag 32 the mix keeps the data default, which is 0.
        let mut mix = 0.0;
        let mut softness = 0.0;
        if flags & 32 != 0 {
            mix = if flags & 64 != 0 { input.f32()? } else { 1.0 };
        }
        if flags & 128 != 0 {
            softness = input.f32()? * scale;
        }
        d.ik_constraints.push(IkConstraintData {
            name,
            order,
            skin_required: flags & 1 != 0,
            bones,
            target,
            bend_direction: if flags & 2 != 0 { 1 } else { -1 },
            compress: flags & 4 != 0,
            stretch: flags & 8 != 0,
            uniform: flags & 16 != 0,
            mix,
            softness,
        });
    }

    // Transform constraints.
    let n = input.count()?;
    for _ in 0..n {
        let name = input.required_string("transform name")?;
        let order = input.varint(true)? as u32;
        let nn = input.count()?;
        let mut bones = Vec::with_capacity(nn);
        for _ in 0..nn {
            bones.push(input.index(d.bones.len(), "transform bone")?);
        }
        let target = input.index(d.bones.len(), "transform target")?;
        let mut c = TransformConstraintData {
            name,
            order,
            skin_required: false,
            bones,
            target,
            local: false,
            relative: false,
            offset_rotation: 0.0,
            offset_x: 0.0,
            offset_y: 0.0,
            offset_scale_x: 0.0,
            offset_scale_y: 0.0,
            offset_shear_y: 0.0,
            mix_rotate: 0.0,
            mix_x: 0.0,
            mix_y: 0.0,
            mix_scale_x: 0.0,
            mix_scale_y: 0.0,
            mix_shear_y: 0.0,
        };
        let flags = input.u8()?;
        c.skin_required = flags & 1 != 0;
        c.local = flags & 2 != 0;
        c.relative = flags & 4 != 0;
        if flags & 8 != 0 {
            c.offset_rotation = input.f32()?;
        }
        if flags & 16 != 0 {
            c.offset_x = input.f32()? * scale;
        }
        if flags & 32 != 0 {
            c.offset_y = input.f32()? * scale;
        }
        if flags & 64 != 0 {
            c.offset_scale_x = input.f32()?;
        }
        if flags & 128 != 0 {
            c.offset_scale_y = input.f32()?;
        }
        let flags = input.u8()?;
        if flags & 1 != 0 {
            c.offset_shear_y = input.f32()?;
        }
        if flags & 2 != 0 {
            c.mix_rotate = input.f32()?;
        }
        if flags & 4 != 0 {
            c.mix_x = input.f32()?;
        }
        if flags & 8 != 0 {
            c.mix_y = input.f32()?;
        }
        if flags & 16 != 0 {
            c.mix_scale_x = input.f32()?;
        }
        if flags & 32 != 0 {
            c.mix_scale_y = input.f32()?;
        }
        if flags & 64 != 0 {
            c.mix_shear_y = input.f32()?;
        }
        d.transform_constraints.push(c);
    }

    // Path constraints.
    let n = input.count()?;
    for _ in 0..n {
        let name = input.required_string("path name")?;
        let order = input.varint(true)? as u32;
        let skin_required = input.bool()?;
        let nn = input.count()?;
        let mut bones = Vec::with_capacity(nn);
        for _ in 0..nn {
            bones.push(input.index(d.bones.len(), "path bone")?);
        }
        let target = input.index(d.slots.len(), "path target")?;
        let flags = input.u8()?;
        let position_mode = if flags & 1 != 0 { PositionMode::Percent } else { PositionMode::Fixed };
        let spacing_mode = match (flags >> 1) & 3 {
            0 => SpacingMode::Length,
            1 => SpacingMode::Fixed,
            2 => SpacingMode::Percent,
            _ => SpacingMode::Proportional,
        };
        let rotate_mode = match (flags >> 3) & 3 {
            0 => RotateMode::Tangent,
            1 => RotateMode::Chain,
            _ => RotateMode::ChainScale,
        };
        let offset_rotation = if flags & 128 != 0 { input.f32()? } else { 0.0 };
        let mut position = input.f32()?;
        if position_mode == PositionMode::Fixed {
            position *= scale;
        }
        let mut spacing = input.f32()?;
        if matches!(spacing_mode, SpacingMode::Length | SpacingMode::Fixed) {
            spacing *= scale;
        }
        let mix_rotate = input.f32()?;
        let mix_x = input.f32()?;
        let mix_y = input.f32()?;
        d.path_constraints.push(PathConstraintData {
            name,
            order,
            skin_required,
            bones,
            target,
            position_mode,
            spacing_mode,
            rotate_mode,
            offset_rotation,
            position,
            spacing,
            mix_rotate,
            mix_x,
            mix_y,
        });
    }

    // Physics constraints.
    let n = input.count()?;
    for _ in 0..n {
        let name = input.required_string("physics name")?;
        let order = input.varint(true)? as u32;
        let bone = input.index(d.bones.len(), "physics bone")?;
        let flags = input.u8()?;
        let mut c = PhysicsConstraintData {
            name,
            order,
            skin_required: flags & 1 != 0,
            bone,
            x: 0.0,
            y: 0.0,
            rotate: 0.0,
            scale_x: 0.0,
            shear_x: 0.0,
            limit: 0.0,
            step: 0.0,
            inertia: 0.0,
            strength: 0.0,
            damping: 0.0,
            mass_inverse: 0.0,
            wind: 0.0,
            gravity: 0.0,
            mix: 0.0,
            inertia_global: false,
            strength_global: false,
            damping_global: false,
            mass_global: false,
            wind_global: false,
            gravity_global: false,
            mix_global: false,
        };
        if flags & 2 != 0 {
            c.x = input.f32()?;
        }
        if flags & 4 != 0 {
            c.y = input.f32()?;
        }
        if flags & 8 != 0 {
            c.rotate = input.f32()?;
        }
        if flags & 16 != 0 {
            c.scale_x = input.f32()?;
        }
        if flags & 32 != 0 {
            c.shear_x = input.f32()?;
        }
        c.limit = (if flags & 64 != 0 { input.f32()? } else { 5000.0 }) * scale;
        let fps = input.u8()?;
        c.step = 1.0 / fps as f32;
        c.inertia = input.f32()?;
        c.strength = input.f32()?;
        c.damping = input.f32()?;
        c.mass_inverse = if flags & 128 != 0 { input.f32()? } else { 1.0 };
        c.wind = input.f32()?;
        c.gravity = input.f32()?;
        let flags = input.u8()?;
        c.inertia_global = flags & 1 != 0;
        c.strength_global = flags & 2 != 0;
        c.damping_global = flags & 4 != 0;
        c.mass_global = flags & 8 != 0;
        c.wind_global = flags & 16 != 0;
        c.gravity_global = flags & 32 != 0;
        c.mix_global = flags & 64 != 0;
        c.mix = if flags & 128 != 0 { input.f32()? } else { 1.0 };
        d.physics_constraints.push(c);
    }

    // Skins: the default skin first (if it has any attachments), then the named ones.
    let mut linked = Vec::new();
    if let Some(skin) = read_skin(&mut input, &mut d, true, nonessential, scale, &mut linked)? {
        d.skins.push(skin);
        d.has_default_skin = true;
    }
    let n = input.count()?;
    for _ in 0..n {
        let skin = read_skin(&mut input, &mut d, false, nonessential, scale, &mut linked)?
            .ok_or(ReadError::Invalid("skin"))?;
        d.skins.push(skin);
    }

    // Linked meshes share their parent's geometry.
    for l in linked {
        let skin = d.skins.get(l.skin).ok_or(ReadError::Invalid("linked mesh skin"))?;
        let parent = skin.get(l.slot, &l.parent).ok_or(ReadError::Invalid("linked mesh parent"))?;
        let (vertices, hull, uvs, triangles) = match &d.attachments[parent as usize].kind {
            AttachmentKind::Mesh(p) => (p.vertices.clone(), p.hull_length, p.region_uvs.clone(), p.triangles.clone()),
            _ => return Err(ReadError::Invalid("linked mesh parent is not a mesh")),
        };
        let a = &mut d.attachments[l.attachment as usize];
        a.timeline_attachment = if l.inherit_timelines { parent } else { l.attachment };
        if let AttachmentKind::Mesh(m) = &mut a.kind {
            m.vertices = vertices;
            m.hull_length = hull;
            m.region_uvs = uvs;
            m.triangles = triangles;
            m.parent_mesh = Some(parent);
        }
    }

    // Weighted tables are checked once here, so skinning can index them without checks.
    let bone_count = d.bones.len();
    for a in d.attachments.iter_mut() {
        if let AttachmentKind::Mesh(m) = &mut a.kind {
            m.vertices.check(bone_count);
        }
    }

    // Events.
    let n = input.count()?;
    for _ in 0..n {
        let name = input.required_string("event name")?;
        let int_value = input.varint(false)?;
        let float_value = input.f32()?;
        let string_value = input.string()?;
        let audio_path = input.string()?;
        let (mut volume, mut balance) = (1.0, 0.0);
        if audio_path.as_deref().is_some_and(|p| !p.is_empty()) {
            volume = input.f32()?;
            balance = input.f32()?;
        }
        d.events.push(EventData { name, int_value, float_value, string_value, audio_path, volume, balance });
    }

    // Animations.
    let n = input.count()?;
    for _ in 0..n {
        let name = input.required_string("animation name")?;
        let a = read_animation(&mut input, name, &d, scale)?;
        d.animations.push(a);
    }
    if input.pos != bytes.len() {
        return Err(ReadError::Invalid("trailing bytes after animations"));
    }
    Ok(d)
}

fn read_sequence(input: &mut Input) -> Result<Sequence> {
    let count = input.varint(true)? as u32;
    let start = input.varint(true)? as u32;
    let digits = input.varint(true)? as u32;
    let setup_index = input.varint(true)? as u32;
    Ok(Sequence { count, start, digits, setup_index, regions: vec![None; count as usize] })
}

fn read_vertices(input: &mut Input, weighted: bool, scale: f32) -> Result<Vertices> {
    let count = input.count()?;
    let world_len = (count * 2) as u32;
    if !weighted {
        let mut v = Vec::with_capacity(count * 2);
        for _ in 0..count * 2 {
            v.push(input.f32()? * scale);
        }
        return Ok(Vertices { bones: Vec::new(), vertices: v, world_len, checked: false, bone_bound: 0 });
    }
    let mut bones = Vec::with_capacity(count * 3);
    let mut weights = Vec::with_capacity(count * 9);
    for _ in 0..count {
        let n = input.count()?;
        bones.push(n as u32);
        for _ in 0..n {
            bones.push(input.varint(true)? as u32);
            weights.push(input.f32()? * scale);
            weights.push(input.f32()? * scale);
            weights.push(input.f32()?);
        }
    }
    Ok(Vertices { bones, vertices: weights, world_len, checked: false, bone_bound: 0 })
}

fn read_skin(
    input: &mut Input,
    d: &mut SkeletonData,
    default: bool,
    nonessential: bool,
    scale: f32,
    linked: &mut Vec<LinkedMesh>,
) -> Result<Option<Skin>> {
    let mut skin = Skin::default();
    let slot_count;
    if default {
        slot_count = input.count()?;
        if slot_count == 0 {
            return Ok(None);
        }
        skin.name = String::from("default");
    } else {
        skin.name = input.required_string("skin name")?;
        if nonessential {
            input.i32()?; // color
        }
        let n = input.count()?;
        for _ in 0..n {
            skin.bones.push(input.index(d.bones.len(), "skin bone")?);
        }
        let n = input.count()?;
        for _ in 0..n {
            skin.constraints.push(ConstraintRef::Ik(input.index(d.ik_constraints.len(), "skin ik")?));
        }
        let n = input.count()?;
        for _ in 0..n {
            skin.constraints.push(ConstraintRef::Transform(input.index(d.transform_constraints.len(), "skin transform")?));
        }
        let n = input.count()?;
        for _ in 0..n {
            skin.constraints.push(ConstraintRef::Path(input.index(d.path_constraints.len(), "skin path")?));
        }
        let n = input.count()?;
        for _ in 0..n {
            skin.constraints.push(ConstraintRef::Physics(input.index(d.physics_constraints.len(), "skin physics")?));
        }
        slot_count = input.count()?;
    }
    // Index this skin will get once pushed.
    let skin_index = d.skins.len();
    for _ in 0..slot_count {
        let slot = input.index(d.slots.len(), "skin slot")?;
        let nn = input.count()?;
        for _ in 0..nn {
            let key = input.string_ref()?.ok_or(ReadError::Invalid("attachment key"))?;
            let id = d.attachments.len() as u32;
            if let Some(a) = read_attachment(input, d, slot, &key, nonessential, scale, id, skin_index, linked)? {
                d.attachments.push(a);
                skin.insert(slot, key, id);
            }
        }
    }
    Ok(Some(skin))
}

#[allow(clippy::too_many_arguments)]
fn read_attachment(
    input: &mut Input,
    d: &SkeletonData,
    slot: u16,
    key: &str,
    nonessential: bool,
    scale: f32,
    id: u32,
    _skin_index: usize,
    linked: &mut Vec<LinkedMesh>,
) -> Result<Option<Attachment>> {
    let flags = input.u8()?;
    let name = if flags & 8 != 0 {
        input.string_ref()?.ok_or(ReadError::Invalid("attachment name"))?
    } else {
        String::from(key)
    };
    let kind = match flags & 0b111 {
        0 => {
            let path = if flags & 16 != 0 { input.string_ref()? } else { None };
            let color = if flags & 32 != 0 { input.i32()? as u32 } else { 0xffff_ffff };
            let sequence = if flags & 64 != 0 { Some(read_sequence(input)?) } else { None };
            let rotation = if flags & 128 != 0 { input.f32()? } else { 0.0 };
            let x = input.f32()? * scale;
            let y = input.f32()? * scale;
            let scale_x = input.f32()?;
            let scale_y = input.f32()?;
            let width = input.f32()? * scale;
            let height = input.f32()? * scale;
            AttachmentKind::Region(RegionAttachment {
                path: path.unwrap_or_else(|| name.clone()),
                color: Color::from_rgba8888(color),
                sequence,
                x,
                y,
                rotation,
                scale_x,
                scale_y,
                width,
                height,
                region: None,
                offset: [0.0; 8],
                uvs: [0.0; 8],
            })
        }
        1 => {
            let v = read_vertices(input, flags & 16 != 0, scale)?;
            if nonessential {
                input.i32()?;
            }
            AttachmentKind::BoundingBox(v)
        }
        2 => {
            let path = if flags & 16 != 0 { input.string_ref()? } else { None };
            let color = if flags & 32 != 0 { input.i32()? as u32 } else { 0xffff_ffff };
            let sequence = if flags & 64 != 0 { Some(read_sequence(input)?) } else { None };
            let hull = input.count()?;
            let vertices = read_vertices(input, flags & 128 != 0, scale)?;
            let n = vertices.world_len as usize;
            let mut uvs = Vec::with_capacity(n);
            for _ in 0..n {
                uvs.push(input.f32()?);
            }
            let vc = n / 2;
            // A triangulated polygon with V vertices, h of them on the hull, has 2V - h - 2 triangles.
            if n < hull + 2 && vc != 0 {
                return Err(ReadError::Invalid("mesh hull"));
            }
            let tri_count = (n.saturating_sub(hull + 2)) * 3;
            let mut triangles = Vec::with_capacity(tri_count);
            for _ in 0..tri_count {
                let t = input.varint(true)?;
                if t < 0 || t as usize >= vc {
                    return Err(ReadError::Invalid("mesh triangle index"));
                }
                triangles.push(t as u16);
            }
            let (mut width, mut height) = (0.0, 0.0);
            if nonessential {
                let ne = input.count()?;
                for _ in 0..ne {
                    input.varint(true)?;
                }
                width = input.f32()? * scale;
                height = input.f32()? * scale;
            }
            AttachmentKind::Mesh(MeshAttachment {
                path: path.unwrap_or_else(|| name.clone()),
                color: Color::from_rgba8888(color),
                sequence,
                vertices,
                hull_length: (hull as u32) << 1,
                region_uvs: uvs,
                triangles,
                width,
                height,
                region: None,
                uvs: Vec::new(),
                parent_mesh: None,
            })
        }
        3 => {
            let path = if flags & 16 != 0 { input.string_ref()? } else { None };
            let color = if flags & 32 != 0 { input.i32()? as u32 } else { 0xffff_ffff };
            let sequence = if flags & 64 != 0 { Some(read_sequence(input)?) } else { None };
            let inherit_timelines = flags & 128 != 0;
            let skin = input.count()?;
            let parent = input.string_ref()?.ok_or(ReadError::Invalid("linked mesh parent name"))?;
            let (mut width, mut height) = (0.0, 0.0);
            if nonessential {
                width = input.f32()? * scale;
                height = input.f32()? * scale;
            }
            linked.push(LinkedMesh { attachment: id, skin, slot, parent, inherit_timelines });
            AttachmentKind::Mesh(MeshAttachment {
                path: path.unwrap_or_else(|| name.clone()),
                color: Color::from_rgba8888(color),
                sequence,
                vertices: Vertices::default(),
                hull_length: 0,
                region_uvs: Vec::new(),
                triangles: Vec::new(),
                width,
                height,
                region: None,
                uvs: Vec::new(),
                parent_mesh: None,
            })
        }
        4 => {
            let closed = flags & 16 != 0;
            let constant_speed = flags & 32 != 0;
            let vertices = read_vertices(input, flags & 64 != 0, scale)?;
            let n = vertices.world_len as usize / 6;
            let mut lengths = Vec::with_capacity(n);
            for _ in 0..n {
                lengths.push(input.f32()? * scale);
            }
            if nonessential {
                input.i32()?;
            }
            AttachmentKind::Path(PathAttachment { vertices, closed, constant_speed, lengths })
        }
        5 => {
            let rotation = input.f32()?;
            let x = input.f32()? * scale;
            let y = input.f32()? * scale;
            if nonessential {
                input.i32()?;
            }
            AttachmentKind::Point(PointAttachment { x, y, rotation })
        }
        6 => {
            let end_slot = input.index(d.slots.len(), "clipping end slot")?;
            let vertices = read_vertices(input, flags & 16 != 0, scale)?;
            if nonessential {
                input.i32()?;
            }
            AttachmentKind::Clipping(ClippingAttachment { vertices, end_slot })
        }
        _ => return Err(ReadError::Invalid("attachment type")),
    };
    Ok(Some(Attachment { name, kind, timeline_attachment: id }))
}

struct CurveBuilder {
    curve: Curve,
    frame_count: usize,
    bezier: usize,
}

impl CurveBuilder {
    fn new(frame_count: usize, bezier_count: usize, entries: usize) -> CurveBuilder {
        let mut curves = vec![0.0; frame_count + bezier_count * BEZIER_SIZE];
        if frame_count > 0 {
            curves[frame_count - 1] = CURVE_STEPPED;
        }
        CurveBuilder {
            curve: Curve { entries: entries as u8, frames: vec![0.0; frame_count * entries], curves, constant: false },
            frame_count,
            bezier: 0,
        }
    }

    fn set_frame(&mut self, frame: usize, values: &[f32]) {
        let e = self.curve.entries as usize;
        self.curve.frames[frame * e..frame * e + values.len()].copy_from_slice(values);
    }

    fn set_stepped(&mut self, frame: usize) {
        self.curve.curves[frame] = CURVE_STEPPED;
    }

    /// Reads one Bezier (cx1, cy1, cx2, cy2) into the curve table.
    #[allow(clippy::too_many_arguments)]
    fn read_bezier(
        &mut self,
        input: &mut Input,
        frame: usize,
        value: usize,
        time1: f32,
        time2: f32,
        value1: f32,
        value2: f32,
        scale: f32,
    ) -> Result<()> {
        let cx1 = input.f32()?;
        let cy1 = input.f32()? * scale;
        let cx2 = input.f32()?;
        let cy2 = input.f32()? * scale;
        let mut i = self.frame_count + self.bezier * BEZIER_SIZE;
        if i + BEZIER_SIZE > self.curve.curves.len() {
            return Err(ReadError::Invalid("bezier count"));
        }
        self.bezier += 1;
        if value == 0 {
            self.curve.curves[frame] = CURVE_BEZIER + i as f32;
        }
        // The endpoints are the frames' (time1, value1) and (time2, value2): only the controls
        // are kept; `Curve::samples` rebuilds the sampled curve from them when it is evaluated.
        let _ = (time1, time2, value1, value2);
        self.curve.curves[i..i + BEZIER_SIZE].copy_from_slice(&[cx1, cy1, cx2, cy2]);
        i += BEZIER_SIZE;
        let _ = i;
        Ok(())
    }

    fn finish(mut self) -> Curve {
        // Drop unused Bezier storage (the writer may over-count).
        let size = self.frame_count + self.bezier * BEZIER_SIZE;
        self.curve.curves.truncate(size);
        let c = &mut self.curve;
        let (e, n) = (c.entries as usize, self.frame_count);
        // Bit equality: a curve between equal keys with handles away from them still moves.
        c.constant = e >= 2
            && n > 0
            && (1..e).all(|v| {
                let x = c.frames[v].to_bits();
                (1..n).all(|k| c.frames[k * e + v].to_bits() == x)
                    && (0..n - 1).all(|k| {
                        let t = c.curves[k];
                        if t < CURVE_BEZIER {
                            return true;
                        }
                        let b = t as usize - 2 + (v - 1) * BEZIER_SIZE;
                        c.curves.get(b + 3).is_some_and(|_| c.curves[b + 1].to_bits() == x && c.curves[b + 3].to_bits() == x)
                    })
            });
        self.curve
    }
}

/// Samples a cubic Bezier into 9 (x, y) points with forward differencing.
#[allow(clippy::too_many_arguments)]
pub fn sample_bezier(out: &mut [f32], time1: f32, value1: f32, cx1: f32, cy1: f32, cx2: f32, cy2: f32, time2: f32, value2: f32) {
    let tmpx = (time1 - cx1 * 2.0 + cx2) * 0.03;
    let tmpy = (value1 - cy1 * 2.0 + cy2) * 0.03;
    let dddx = ((cx1 - cx2) * 3.0 - time1 + time2) * 0.006;
    let dddy = ((cy1 - cy2) * 3.0 - value1 + value2) * 0.006;
    let mut ddx = tmpx * 2.0 + dddx;
    let mut ddy = tmpy * 2.0 + dddy;
    let mut dx = (cx1 - time1) * 0.3 + tmpx + dddx * 0.166_666_67;
    let mut dy = (cy1 - value1) * 0.3 + tmpy + dddy * 0.166_666_67;
    let mut x = time1 + dx;
    let mut y = value1 + dy;
    for pair in out.chunks_exact_mut(2) {
        pair[0] = x;
        pair[1] = y;
        dx += ddx;
        dy += ddy;
        ddx += dddx;
        ddy += dddy;
        x += dx;
        y += dy;
    }
}

/// Reads a timeline whose frames are (time, value...) with `n` f32 values, each scaled by
/// `scales[i]`, followed per segment by a curve type and Beziers for every value.
fn read_curve_values(input: &mut Input, frame_count: usize, bezier_count: usize, scales: &[f32]) -> Result<Curve> {
    let n = scales.len();
    let mut b = CurveBuilder::new(frame_count, bezier_count, n + 1);
    if frame_count == 0 {
        return Ok(b.finish());
    }
    let mut cur = [0f32; 8];
    let mut next = [0f32; 8];
    cur[0] = input.f32()?;
    for i in 0..n {
        cur[i + 1] = input.f32()? * scales[i];
    }
    let last = frame_count - 1;
    let mut frame = 0;
    loop {
        b.set_frame(frame, &cur[..n + 1]);
        if frame == last {
            break;
        }
        next[0] = input.f32()?;
        for i in 0..n {
            next[i + 1] = input.f32()? * scales[i];
        }
        match input.u8()? {
            CURVE_TYPE_STEPPED => b.set_stepped(frame),
            CURVE_TYPE_BEZIER => {
                for i in 0..n {
                    b.read_bezier(input, frame, i, cur[0], next[0], cur[i + 1], next[i + 1], scales[i])?;
                }
            }
            _ => {}
        }
        cur = next;
        frame += 1;
    }
    Ok(b.finish())
}

/// Like `read_curve_values` but every value is an unsigned byte divided by 255 (slot colors).
fn read_color_curve(input: &mut Input, frame_count: usize, bezier_count: usize, n: usize) -> Result<Curve> {
    let mut b = CurveBuilder::new(frame_count, bezier_count, n + 1);
    if frame_count == 0 {
        return Ok(b.finish());
    }
    let mut cur = [0f32; 8];
    let mut next = [0f32; 8];
    cur[0] = input.f32()?;
    for i in 0..n {
        cur[i + 1] = input.u8()? as f32 / 255.0;
    }
    let last = frame_count - 1;
    let mut frame = 0;
    loop {
        b.set_frame(frame, &cur[..n + 1]);
        if frame == last {
            break;
        }
        next[0] = input.f32()?;
        for i in 0..n {
            next[i + 1] = input.u8()? as f32 / 255.0;
        }
        match input.u8()? {
            CURVE_TYPE_STEPPED => b.set_stepped(frame),
            CURVE_TYPE_BEZIER => {
                for i in 0..n {
                    b.read_bezier(input, frame, i, cur[0], next[0], cur[i + 1], next[i + 1], 1.0)?;
                }
            }
            _ => {}
        }
        cur = next;
        frame += 1;
    }
    Ok(b.finish())
}

fn read_animation(input: &mut Input, name: String, d: &SkeletonData, scale: f32) -> Result<Animation> {
    input.varint(true)?; // timeline count
    let mut timelines = Vec::new();

    // Slot timelines.
    let n = input.count()?;
    for _ in 0..n {
        let slot = input.index(d.slots.len(), "slot timeline slot")?;
        let nn = input.count()?;
        for _ in 0..nn {
            let ty = input.u8()?;
            let frame_count = input.count()?;
            match ty {
                SLOT_ATTACHMENT => {
                    let mut frames = Vec::with_capacity(frame_count);
                    let mut names = Vec::with_capacity(frame_count);
                    for _ in 0..frame_count {
                        frames.push(input.f32()?);
                        names.push(input.string_ref()?);
                    }
                    timelines.push(Timeline::Attachment { slot, frames, names });
                }
                SLOT_RGBA | SLOT_RGB | SLOT_RGBA2 | SLOT_RGB2 | SLOT_ALPHA => {
                    let bezier_count = input.count()?;
                    let (kind, values) = match ty {
                        SLOT_RGBA => (SlotColorKind::Rgba, 4),
                        SLOT_RGB => (SlotColorKind::Rgb, 3),
                        SLOT_RGBA2 => (SlotColorKind::Rgba2, 7),
                        SLOT_RGB2 => (SlotColorKind::Rgb2, 6),
                        _ => (SlotColorKind::Alpha, 1),
                    };
                    let curve = read_color_curve(input, frame_count, bezier_count, values)?;
                    timelines.push(Timeline::SlotColor { slot, kind, curve });
                }
                _ => return Err(ReadError::Invalid("slot timeline type")),
            }
        }
    }

    // Bone timelines.
    let n = input.count()?;
    for _ in 0..n {
        let bone = input.index(d.bones.len(), "bone timeline bone")?;
        let nn = input.count()?;
        for _ in 0..nn {
            let ty = input.u8()?;
            let frame_count = input.count()?;
            if ty == BONE_INHERIT {
                let mut frames = Vec::with_capacity(frame_count);
                let mut values = Vec::with_capacity(frame_count);
                for _ in 0..frame_count {
                    frames.push(input.f32()?);
                    values.push(Inherit::from_u8(input.u8()?).ok_or(ReadError::Invalid("inherit"))?);
                }
                timelines.push(Timeline::Inherit { bone, frames, values });
                continue;
            }
            let bezier_count = input.count()?;
            let (property, scales): (BoneProperty, &[f32]) = match ty {
                BONE_ROTATE => (BoneProperty::Rotate, &[1.0]),
                BONE_TRANSLATE => (BoneProperty::Translate, &[scale, scale]),
                BONE_TRANSLATEX => (BoneProperty::TranslateX, &[scale]),
                BONE_TRANSLATEY => (BoneProperty::TranslateY, &[scale]),
                BONE_SCALE => (BoneProperty::Scale, &[1.0, 1.0]),
                BONE_SCALEX => (BoneProperty::ScaleX, &[1.0]),
                BONE_SCALEY => (BoneProperty::ScaleY, &[1.0]),
                BONE_SHEAR => (BoneProperty::Shear, &[1.0, 1.0]),
                BONE_SHEARX => (BoneProperty::ShearX, &[1.0]),
                BONE_SHEARY => (BoneProperty::ShearY, &[1.0]),
                _ => return Err(ReadError::Invalid("bone timeline type")),
            };
            // `scales` borrows a temporary array; copy before the reader call.
            let mut s = [0f32; 2];
            s[..scales.len()].copy_from_slice(scales);
            let curve = read_curve_values(input, frame_count, bezier_count, &s[..scales.len()])?;
            timelines.push(Timeline::Bone { bone, property, curve });
        }
    }

    // IK constraint timelines: frames are (time, mix, softness, bend, compress, stretch).
    let n = input.count()?;
    for _ in 0..n {
        let constraint = input.index(d.ik_constraints.len(), "ik timeline")?;
        let frame_count = input.count()?;
        let bezier_count = input.count()?;
        let mut b = CurveBuilder::new(frame_count, bezier_count, 6);
        if frame_count > 0 {
            let mut flags = input.u8()?;
            let mut time = input.f32()?;
            let mut mix = if flags & 1 != 0 { if flags & 2 != 0 { input.f32()? } else { 1.0 } } else { 0.0 };
            let mut softness = if flags & 4 != 0 { input.f32()? * scale } else { 0.0 };
            let last = frame_count - 1;
            let mut frame = 0;
            loop {
                b.set_frame(
                    frame,
                    &[
                        time,
                        mix,
                        softness,
                        if flags & 8 != 0 { 1.0 } else { -1.0 },
                        if flags & 16 != 0 { 1.0 } else { 0.0 },
                        if flags & 32 != 0 { 1.0 } else { 0.0 },
                    ],
                );
                if frame == last {
                    break;
                }
                flags = input.u8()?;
                let time2 = input.f32()?;
                let mix2 = if flags & 1 != 0 { if flags & 2 != 0 { input.f32()? } else { 1.0 } } else { 0.0 };
                let softness2 = if flags & 4 != 0 { input.f32()? * scale } else { 0.0 };
                if flags & 64 != 0 {
                    b.set_stepped(frame);
                } else if flags & 128 != 0 {
                    b.read_bezier(input, frame, 0, time, time2, mix, mix2, 1.0)?;
                    b.read_bezier(input, frame, 1, time, time2, softness, softness2, scale)?;
                }
                time = time2;
                mix = mix2;
                softness = softness2;
                frame += 1;
            }
        }
        timelines.push(Timeline::Ik { constraint, curve: b.finish() });
    }

    // Transform constraint timelines.
    let n = input.count()?;
    for _ in 0..n {
        let constraint = input.index(d.transform_constraints.len(), "transform timeline")?;
        let frame_count = input.count()?;
        let bezier_count = input.count()?;
        let curve = read_curve_values(input, frame_count, bezier_count, &[1.0; 6])?;
        timelines.push(Timeline::Transform { constraint, curve });
    }

    // Path constraint timelines.
    let n = input.count()?;
    for _ in 0..n {
        let constraint = input.index(d.path_constraints.len(), "path timeline")?;
        let data = &d.path_constraints[constraint as usize];
        let nn = input.count()?;
        for _ in 0..nn {
            let ty = input.u8()?;
            let frame_count = input.count()?;
            let bezier_count = input.count()?;
            let (property, s): (PathProperty, &[f32]) = match ty {
                PATH_POSITION => (
                    PathProperty::Position,
                    if data.position_mode == PositionMode::Fixed { &[scale][..] } else { &[1.0][..] },
                ),
                PATH_SPACING => (
                    PathProperty::Spacing,
                    if matches!(data.spacing_mode, SpacingMode::Length | SpacingMode::Fixed) { &[scale][..] } else { &[1.0][..] },
                ),
                PATH_MIX => (PathProperty::Mix, &[1.0, 1.0, 1.0][..]),
                _ => return Err(ReadError::Invalid("path timeline type")),
            };
            let mut sc = [0f32; 3];
            sc[..s.len()].copy_from_slice(s);
            let curve = read_curve_values(input, frame_count, bezier_count, &sc[..s.len()])?;
            timelines.push(Timeline::Path { constraint, property, curve });
        }
    }

    // Physics timelines; index 0 means "all constraints".
    let n = input.count()?;
    for _ in 0..n {
        let raw = input.varint(true)?;
        let constraint = if raw == 0 {
            None
        } else {
            let i = raw - 1;
            if i < 0 || i as usize >= d.physics_constraints.len() {
                return Err(ReadError::Invalid("physics timeline"));
            }
            Some(i as u16)
        };
        let nn = input.count()?;
        for _ in 0..nn {
            let ty = input.u8()?;
            let frame_count = input.count()?;
            if ty == PHYSICS_RESET {
                let mut frames = Vec::with_capacity(frame_count);
                for _ in 0..frame_count {
                    frames.push(input.f32()?);
                }
                timelines.push(Timeline::PhysicsReset { constraint, frames });
                continue;
            }
            let bezier_count = input.count()?;
            let property = match ty {
                PHYSICS_INERTIA => PhysicsProperty::Inertia,
                PHYSICS_STRENGTH => PhysicsProperty::Strength,
                PHYSICS_DAMPING => PhysicsProperty::Damping,
                PHYSICS_MASS => PhysicsProperty::Mass,
                PHYSICS_WIND => PhysicsProperty::Wind,
                PHYSICS_GRAVITY => PhysicsProperty::Gravity,
                PHYSICS_MIX => PhysicsProperty::Mix,
                _ => return Err(ReadError::Invalid("physics timeline type")),
            };
            let curve = read_curve_values(input, frame_count, bezier_count, &[1.0])?;
            timelines.push(Timeline::Physics { constraint, property, curve });
        }
    }

    // Attachment (deform / sequence) timelines.
    let n = input.count()?;
    for _ in 0..n {
        let skin = input.index(d.skins.len(), "deform skin")?;
        let nn = input.count()?;
        for _ in 0..nn {
            let slot = input.index(d.slots.len(), "deform slot")?;
            let nnn = input.count()?;
            for _ in 0..nnn {
                let key = input.string_ref()?.ok_or(ReadError::Invalid("deform attachment"))?;
                let attachment = d.skins[skin as usize]
                    .get(slot, &key)
                    .ok_or(ReadError::Invalid("deform attachment not in skin"))?;
                let ty = input.u8()?;
                let frame_count = input.count()?;
                match ty {
                    ATTACHMENT_DEFORM => {
                        let verts = d.attachments[attachment as usize]
                            .vertices()
                            .ok_or(ReadError::Invalid("deform on non-vertex attachment"))?;
                        let weighted = verts.is_weighted();
                        let deform_len = verts.deform_len();
                        let bezier_count = input.count()?;
                        let mut b = CurveBuilder::new(frame_count, bezier_count, 1);
                        let mut vertices = Vec::with_capacity(frame_count);
                        if frame_count > 0 {
                            let mut time = input.f32()?;
                            let last = frame_count - 1;
                            let mut frame = 0;
                            loop {
                                let end = input.count()?;
                                let deform = if end == 0 {
                                    if weighted { vec![0.0; deform_len] } else { verts.vertices.clone() }
                                } else {
                                    let mut deform = vec![0.0; deform_len];
                                    let start = input.count()?;
                                    let end = end + start;
                                    if end > deform_len {
                                        return Err(ReadError::Invalid("deform range"));
                                    }
                                    for v in deform.iter_mut().take(end).skip(start) {
                                        *v = input.f32()? * scale;
                                    }
                                    if !weighted {
                                        for (v, s) in deform.iter_mut().zip(verts.vertices.iter()) {
                                            *v += *s;
                                        }
                                    }
                                    deform
                                };
                                b.set_frame(frame, &[time]);
                                vertices.push(deform);
                                if frame == last {
                                    break;
                                }
                                let time2 = input.f32()?;
                                match input.u8()? {
                                    CURVE_TYPE_STEPPED => b.set_stepped(frame),
                                    CURVE_TYPE_BEZIER => b.read_bezier(input, frame, 0, time, time2, 0.0, 1.0, 1.0)?,
                                    _ => {}
                                }
                                time = time2;
                                frame += 1;
                            }
                        }
                        let constant = vertices.windows(2).all(|w| w[0].iter().zip(&w[1]).all(|(a, b)| a.to_bits() == b.to_bits()));
                        timelines.push(Timeline::Deform { slot, attachment, curve: b.finish(), vertices, constant });
                    }
                    ATTACHMENT_SEQUENCE => {
                        let mut frames = Vec::with_capacity(frame_count * 3);
                        for _ in 0..frame_count {
                            let time = input.f32()?;
                            let mode_and_index = input.i32()?;
                            let delay = input.f32()?;
                            frames.push(time);
                            frames.push(f32::from_bits(mode_and_index as u32));
                            frames.push(delay);
                        }
                        timelines.push(Timeline::Sequence { slot, attachment, frames });
                    }
                    _ => return Err(ReadError::Invalid("attachment timeline type")),
                }
            }
        }
    }

    // Draw order timeline.
    let draw_order_count = input.count()?;
    if draw_order_count > 0 {
        let slot_count = d.slots.len();
        let mut frames = Vec::with_capacity(draw_order_count);
        let mut orders = Vec::with_capacity(draw_order_count);
        for _ in 0..draw_order_count {
            frames.push(input.f32()?);
            let offset_count = input.count()?;
            if offset_count == 0 {
                orders.push(None);
                continue;
            }
            if offset_count > slot_count {
                return Err(ReadError::Invalid("draw order offsets"));
            }
            let mut order = vec![-1i32; slot_count];
            let mut unchanged = vec![0i32; slot_count - offset_count];
            let (mut original, mut unchanged_index) = (0usize, 0usize);
            for _ in 0..offset_count {
                let slot = input.index(slot_count, "draw order slot")? as usize;
                while original != slot {
                    *unchanged.get_mut(unchanged_index).ok_or(ReadError::Invalid("draw order"))? = original as i32;
                    unchanged_index += 1;
                    original += 1;
                }
                let target = original as i64 + input.varint(true)? as i64;
                if target < 0 || target as usize >= slot_count {
                    return Err(ReadError::Invalid("draw order offset"));
                }
                order[target as usize] = original as i32;
                original += 1;
            }
            while original < slot_count {
                *unchanged.get_mut(unchanged_index).ok_or(ReadError::Invalid("draw order"))? = original as i32;
                unchanged_index += 1;
                original += 1;
            }
            for i in (0..slot_count).rev() {
                if order[i] == -1 {
                    unchanged_index = unchanged_index.checked_sub(1).ok_or(ReadError::Invalid("draw order"))?;
                    order[i] = unchanged[unchanged_index];
                }
            }
            orders.push(Some(order.into_iter().map(|v| v as u16).collect()));
        }
        timelines.push(Timeline::DrawOrder { frames, orders });
    }

    // Event timeline.
    let event_count = input.count()?;
    if event_count > 0 {
        let mut frames = Vec::with_capacity(event_count);
        let mut events = Vec::with_capacity(event_count);
        for _ in 0..event_count {
            let time = input.f32()?;
            let data = input.index(d.events.len(), "event")?;
            let ed = &d.events[data as usize];
            let int_value = input.varint(false)?;
            let float_value = input.f32()?;
            let string_value = input.string()?.or_else(|| ed.string_value.clone());
            let (mut volume, mut balance) = (ed.volume, ed.balance);
            if ed.audio_path.as_deref().is_some_and(|p| !p.is_empty()) {
                volume = input.f32()?;
                balance = input.f32()?;
            }
            frames.push(time);
            events.push(EventKey { time, data, int_value, float_value, string_value, volume, balance });
        }
        timelines.push(Timeline::Event { frames, events });
    }

    let duration = timelines.iter().map(|t| t.duration()).fold(0.0f32, f32::max);
    Ok(Animation { name, timelines, duration })
}

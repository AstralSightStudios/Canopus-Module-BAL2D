//! Immutable skeleton data: the setup pose, skins, attachments and animations.
//!
//! The layout follows the Spine 4.2 data model closely enough that the binary reader, the pack
//! reader and the runtime share one representation. Attachments live in one arena
//! (`SkeletonData::attachments`) and are referenced by index everywhere (skins, timelines, the
//! runtime slots), which keeps the data `Clone`-free and makes deform timelines cheap to match.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };

    pub fn from_rgba8888(v: u32) -> Color {
        Color {
            r: ((v >> 24) & 0xff) as f32 / 255.0,
            g: ((v >> 16) & 0xff) as f32 / 255.0,
            b: ((v >> 8) & 0xff) as f32 / 255.0,
            a: (v & 0xff) as f32 / 255.0,
        }
    }

    pub fn from_rgb888(v: u32) -> Color {
        Color {
            r: ((v >> 16) & 0xff) as f32 / 255.0,
            g: ((v >> 8) & 0xff) as f32 / 255.0,
            b: (v & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    pub fn clamp(&mut self) {
        self.r = crate::math::clamp(self.r, 0.0, 1.0);
        self.g = crate::math::clamp(self.g, 0.0, 1.0);
        self.b = crate::math::clamp(self.b, 0.0, 1.0);
        self.a = crate::math::clamp(self.a, 0.0, 1.0);
    }
}

/// How a bone inherits its parent's transform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inherit {
    Normal = 0,
    OnlyTranslation = 1,
    NoRotationOrReflection = 2,
    NoScale = 3,
    NoScaleOrReflection = 4,
}

impl Inherit {
    pub fn from_u8(v: u8) -> Option<Inherit> {
        Some(match v {
            0 => Inherit::Normal,
            1 => Inherit::OnlyTranslation,
            2 => Inherit::NoRotationOrReflection,
            3 => Inherit::NoScale,
            4 => Inherit::NoScaleOrReflection,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Normal = 0,
    Additive = 1,
    Multiply = 2,
    Screen = 3,
}

impl BlendMode {
    pub fn from_u32(v: u32) -> Option<BlendMode> {
        Some(match v {
            0 => BlendMode::Normal,
            1 => BlendMode::Additive,
            2 => BlendMode::Multiply,
            3 => BlendMode::Screen,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct BoneData {
    pub name: String,
    pub parent: Option<u16>,
    pub length: f32,
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub shear_x: f32,
    pub shear_y: f32,
    pub inherit: Inherit,
    pub skin_required: bool,
}

#[derive(Clone, Debug)]
pub struct SlotData {
    pub name: String,
    pub bone: u16,
    pub color: Color,
    /// Two-color tint ("dark" color); `None` when the slot is not tinted.
    pub dark: Option<Color>,
    pub attachment: Option<String>,
    pub blend: BlendMode,
}

#[derive(Clone, Debug)]
pub struct IkConstraintData {
    pub name: String,
    pub order: u32,
    pub skin_required: bool,
    pub bones: Vec<u16>,
    pub target: u16,
    pub bend_direction: i8,
    pub compress: bool,
    pub stretch: bool,
    pub uniform: bool,
    pub mix: f32,
    pub softness: f32,
}

#[derive(Clone, Debug)]
pub struct TransformConstraintData {
    pub name: String,
    pub order: u32,
    pub skin_required: bool,
    pub bones: Vec<u16>,
    pub target: u16,
    pub local: bool,
    pub relative: bool,
    pub offset_rotation: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub offset_scale_x: f32,
    pub offset_scale_y: f32,
    pub offset_shear_y: f32,
    pub mix_rotate: f32,
    pub mix_x: f32,
    pub mix_y: f32,
    pub mix_scale_x: f32,
    pub mix_scale_y: f32,
    pub mix_shear_y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionMode {
    Fixed = 0,
    Percent = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpacingMode {
    Length = 0,
    Fixed = 1,
    Percent = 2,
    Proportional = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotateMode {
    Tangent = 0,
    Chain = 1,
    ChainScale = 2,
}

#[derive(Clone, Debug)]
pub struct PathConstraintData {
    pub name: String,
    pub order: u32,
    pub skin_required: bool,
    pub bones: Vec<u16>,
    /// The slot whose path attachment the bones follow.
    pub target: u16,
    pub position_mode: PositionMode,
    pub spacing_mode: SpacingMode,
    pub rotate_mode: RotateMode,
    pub offset_rotation: f32,
    pub position: f32,
    pub spacing: f32,
    pub mix_rotate: f32,
    pub mix_x: f32,
    pub mix_y: f32,
}

#[derive(Clone, Debug)]
pub struct PhysicsConstraintData {
    pub name: String,
    pub order: u32,
    pub skin_required: bool,
    pub bone: u16,
    pub x: f32,
    pub y: f32,
    pub rotate: f32,
    pub scale_x: f32,
    pub shear_x: f32,
    pub limit: f32,
    pub step: f32,
    pub inertia: f32,
    pub strength: f32,
    pub damping: f32,
    pub mass_inverse: f32,
    pub wind: f32,
    pub gravity: f32,
    pub mix: f32,
    pub inertia_global: bool,
    pub strength_global: bool,
    pub damping_global: bool,
    pub mass_global: bool,
    pub wind_global: bool,
    pub gravity_global: bool,
    pub mix_global: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstraintRef {
    Ik(u16),
    Transform(u16),
    Path(u16),
    Physics(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceMode {
    Hold = 0,
    Once = 1,
    Loop = 2,
    Pingpong = 3,
    OnceReverse = 4,
    LoopReverse = 5,
    PingpongReverse = 6,
}

impl SequenceMode {
    pub fn from_u32(v: u32) -> SequenceMode {
        match v {
            1 => SequenceMode::Once,
            2 => SequenceMode::Loop,
            3 => SequenceMode::Pingpong,
            4 => SequenceMode::OnceReverse,
            5 => SequenceMode::LoopReverse,
            6 => SequenceMode::PingpongReverse,
            _ => SequenceMode::Hold,
        }
    }
}

/// An image sequence on a region or mesh: `count` atlas regions named `path + index`.
#[derive(Clone, Debug)]
pub struct Sequence {
    pub count: u32,
    pub start: u32,
    pub digits: u32,
    pub setup_index: u32,
    /// Atlas region per frame, resolved by whoever binds the atlas.
    pub regions: Vec<Option<u32>>,
}

impl Sequence {
    pub fn path(&self, base: &str, index: u32) -> String {
        use core::fmt::Write;
        let mut s = String::from(base);
        let number = self.start + index;
        let mut digits = alloc::format!("{number}");
        while (digits.len() as u32) < self.digits {
            digits.insert(0, '0');
        }
        let _ = write!(s, "{digits}");
        s
    }
}

/// Vertices of a vertex attachment, either local (unweighted: x, y pairs) or weighted
/// (per vertex: bone count, then per bone index + (x, y, weight)).
#[derive(Clone, Debug, Default)]
pub struct Vertices {
    /// Spine's packed bone table: for each vertex, `count` followed by `count` bone indices.
    /// Empty when the attachment is not weighted.
    pub bones: Vec<u32>,
    /// Unweighted: x, y per vertex. Weighted: x, y, weight per bone influence.
    pub vertices: Vec<f32>,
    /// Number of floats of world output (2 per vertex).
    pub world_len: u32,
    /// [`Vertices::check`] found the bone table consistent with `vertices` and `world_len`
    /// and every bone index below `bone_bound`: skinning may then index without checks.
    pub checked: bool,
    pub bone_bound: u32,
}

impl Vertices {
    /// Validates a weighted table (counts, lengths, bone indices below `bone_count`) and marks
    /// it [`Vertices::checked`]. Anything that edits the table afterwards must clear the mark.
    pub fn check(&mut self, bone_count: usize) {
        self.checked = false;
        if self.bones.is_empty() {
            return;
        }
        let (t, w) = (&self.bones, &self.vertices);
        let (mut v, mut b, mut verts, mut bound) = (0usize, 0usize, 0usize, 0u32);
        while v < t.len() {
            let n = t[v] as usize;
            let Some(ids) = t.get(v + 1..v + 1 + n) else { return };
            for &id in ids {
                if id as usize >= bone_count {
                    return;
                }
                bound = bound.max(id + 1);
            }
            v += 1 + n;
            b += 3 * n;
            verts += 1;
        }
        if b == w.len() && verts * 2 == self.world_len as usize {
            self.checked = true;
            self.bone_bound = bound;
        }
    }

    pub fn is_weighted(&self) -> bool {
        !self.bones.is_empty()
    }

    pub fn vertex_count(&self) -> usize {
        (self.world_len / 2) as usize
    }

    /// Length of a deform array for this attachment (per-vertex x, y offsets for weighted
    /// attachments, absolute local positions otherwise).
    pub fn deform_len(&self) -> usize {
        if self.is_weighted() { self.vertices.len() / 3 * 2 } else { self.vertices.len() }
    }
}

/// Texture region an attachment samples from, resolved against an atlas.
/// UVs are normalized to the atlas page; `degrees` is 0 or 90 (atlas packing rotation).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextureRegion {
    pub page: u32,
    pub u: f32,
    pub v: f32,
    pub u2: f32,
    pub v2: f32,
    pub degrees: i32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub width: f32,
    pub height: f32,
    pub original_width: f32,
    pub original_height: f32,
    /// Packed size in pixels (rotated regions are stored rotated).
    pub packed_width: f32,
    pub packed_height: f32,
    pub page_width: f32,
    pub page_height: f32,
}

#[derive(Clone, Debug)]
pub struct RegionAttachment {
    pub path: String,
    pub color: Color,
    pub sequence: Option<Sequence>,
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub width: f32,
    pub height: f32,
    pub region: Option<TextureRegion>,
    /// Local corner positions (BL, TL, TR, BR as x, y), computed by `update_region`.
    pub offset: [f32; 8],
    /// UVs in the same corner order.
    pub uvs: [f32; 8],
}

#[derive(Clone, Debug)]
pub struct MeshAttachment {
    pub path: String,
    pub color: Color,
    pub sequence: Option<Sequence>,
    pub vertices: Vertices,
    /// Number of hull vertices times two (Spine convention).
    pub hull_length: u32,
    /// Region-relative UVs (0..1 within the region).
    pub region_uvs: Vec<f32>,
    pub triangles: Vec<u16>,
    pub width: f32,
    pub height: f32,
    pub region: Option<TextureRegion>,
    /// Page UVs computed by `update_region`.
    pub uvs: Vec<f32>,
    /// For linked meshes: the parent mesh this one shares geometry with.
    pub parent_mesh: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct PathAttachment {
    pub vertices: Vertices,
    pub closed: bool,
    pub constant_speed: bool,
    pub lengths: Vec<f32>,
}

#[derive(Clone, Debug)]
pub struct PointAttachment {
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
}

#[derive(Clone, Debug)]
pub struct ClippingAttachment {
    pub vertices: Vertices,
    pub end_slot: u16,
}

#[derive(Clone, Debug)]
pub enum AttachmentKind {
    Region(RegionAttachment),
    Mesh(MeshAttachment),
    BoundingBox(Vertices),
    Path(PathAttachment),
    Point(PointAttachment),
    Clipping(ClippingAttachment),
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub name: String,
    pub kind: AttachmentKind,
    /// Deform and sequence timelines targeting this attachment id also apply to this attachment
    /// (a linked mesh with inherited timelines points at its parent).
    pub timeline_attachment: u32,
}

impl Attachment {
    pub fn vertices(&self) -> Option<&Vertices> {
        match &self.kind {
            AttachmentKind::Mesh(m) => Some(&m.vertices),
            AttachmentKind::BoundingBox(v) => Some(v),
            AttachmentKind::Path(p) => Some(&p.vertices),
            AttachmentKind::Clipping(c) => Some(&c.vertices),
            _ => None,
        }
    }

    pub fn sequence(&self) -> Option<&Sequence> {
        match &self.kind {
            AttachmentKind::Region(r) => r.sequence.as_ref(),
            AttachmentKind::Mesh(m) => m.sequence.as_ref(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Skin {
    pub name: String,
    /// (slot, attachment name, attachment id), sorted by (slot, name) for binary search.
    pub attachments: Vec<(u16, String, u32)>,
    pub bones: Vec<u16>,
    pub constraints: Vec<ConstraintRef>,
}

impl Skin {
    pub fn get(&self, slot: u16, name: &str) -> Option<u32> {
        self.attachments
            .binary_search_by(|(s, n, _)| (*s, n.as_str()).cmp(&(slot, name)))
            .ok()
            .map(|i| self.attachments[i].2)
    }

    pub fn insert(&mut self, slot: u16, name: String, id: u32) {
        match self.attachments.binary_search_by(|(s, n, _)| (*s, n.as_str()).cmp(&(slot, name.as_str()))) {
            Ok(i) => self.attachments[i].2 = id,
            Err(i) => self.attachments.insert(i, (slot, name, id)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct EventData {
    pub name: String,
    pub int_value: i32,
    pub float_value: f32,
    pub string_value: Option<String>,
    pub audio_path: Option<String>,
    pub volume: f32,
    pub balance: f32,
}

#[derive(Clone, Debug)]
pub struct EventKey {
    pub time: f32,
    pub data: u16,
    pub int_value: i32,
    pub float_value: f32,
    pub string_value: Option<String>,
    pub volume: f32,
    pub balance: f32,
}

/// Floats stored per Bezier segment: its control values (cx1, cy1, cx2, cy2). The endpoints are
/// the frames' own times and values, so the curve is sampled when evaluated (Spine's runtimes
/// keep [`BEZIER_SAMPLES`] precomputed floats instead; the samples are the same numbers, at 4.5
/// times the memory, which on a watch is most of a lobby's heap).
pub const BEZIER_SIZE: usize = 4;
/// Floats of a sampled Bezier: 9 (x, y) points.
pub const BEZIER_SAMPLES: usize = 18;
/// How far (relative to their magnitude) control values rebuilt from a shared easing may be from
/// the stored ones: a few float roundings.
pub const SHARE_TOLERANCE: f32 = 4e-6;
pub const CURVE_LINEAR: f32 = 0.0;
pub const CURVE_STEPPED: f32 = 1.0;
pub const CURVE_BEZIER: f32 = 2.0;

/// Key frames with per-segment interpolation: `frames` holds `entries` floats per frame
/// (time first), `curves` holds one curve type per frame followed by the Bezier segments'
/// control values ([`BEZIER_SIZE`] floats each).
///
/// A Bezier frame's type is `CURVE_BEZIER + i` when its `channels` Beziers are stored one after
/// another from `curves[i]`, or `-(CURVE_BEZIER + i)` when the channels share one easing (Spine's
/// linked curves) stored once from `curves[i]`, normalised: (x1, y1, x2, y2) with x from the
/// frame's time to the next's and y from each channel's value to the next's (see
/// [`Curve::share_easings`]). [`Curve::controls`] gives a channel's control values either way.
#[derive(Clone, Debug, Default)]
pub struct Curve {
    pub entries: u8,
    /// Bezier channels of a Bezier frame (the values with curves: all of them, but two for IK).
    pub channels: u8,
    pub frames: Vec<f32>,
    pub curves: Vec<f32>,
    /// Every value holds the same value at every key and along every Bezier between keys (its
    /// handles are at that value too): evaluating it gives the first key's values exactly, so
    /// no search or interpolation is needed. Set when the curve is read.
    pub constant: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoneProperty {
    Rotate,
    Translate,
    TranslateX,
    TranslateY,
    Scale,
    ScaleX,
    ScaleY,
    Shear,
    ShearX,
    ShearY,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotColorKind {
    Rgba,
    Rgb,
    Rgba2,
    Rgb2,
    Alpha,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathProperty {
    Position,
    Spacing,
    Mix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicsProperty {
    Inertia,
    Strength,
    Damping,
    Mass,
    Wind,
    Gravity,
    Mix,
}

#[derive(Clone, Debug)]
pub enum Timeline {
    Attachment { slot: u16, frames: Vec<f32>, names: Vec<Option<String>> },
    SlotColor { slot: u16, kind: SlotColorKind, curve: Curve },
    Bone { bone: u16, property: BoneProperty, curve: Curve },
    Inherit { bone: u16, frames: Vec<f32>, values: Vec<Inherit> },
    Ik { constraint: u16, curve: Curve },
    Transform { constraint: u16, curve: Curve },
    Path { constraint: u16, property: PathProperty, curve: Curve },
    /// `constraint` is `None` for timelines that apply to every physics constraint.
    Physics { constraint: Option<u16>, property: PhysicsProperty, curve: Curve },
    PhysicsReset { constraint: Option<u16>, frames: Vec<f32> },
    /// `constant`: every key holds the same values (bit for bit), set when the timeline is read.
    Deform { slot: u16, attachment: u32, curve: Curve, vertices: Vec<Vec<f32>>, constant: bool },
    Sequence { slot: u16, attachment: u32, frames: Vec<f32> },
    DrawOrder { frames: Vec<f32>, orders: Vec<Option<Vec<u16>>> },
    Event { frames: Vec<f32>, events: Vec<EventKey> },
}

impl Timeline {
    pub fn duration(&self) -> f32 {
        fn last(frames: &[f32], entries: usize) -> f32 {
            if frames.is_empty() { 0.0 } else { frames[frames.len() - entries] }
        }
        match self {
            Timeline::Attachment { frames, .. }
            | Timeline::Inherit { frames, .. }
            | Timeline::PhysicsReset { frames, .. }
            | Timeline::DrawOrder { frames, .. }
            | Timeline::Event { frames, .. } => last(frames, 1),
            Timeline::Sequence { frames, .. } => last(frames, 3),
            Timeline::SlotColor { curve, .. }
            | Timeline::Bone { curve, .. }
            | Timeline::Ik { curve, .. }
            | Timeline::Transform { curve, .. }
            | Timeline::Path { curve, .. }
            | Timeline::Physics { curve, .. }
            | Timeline::Deform { curve, .. } => last(&curve.frames, curve.entries as usize),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Animation {
    pub name: String,
    pub timelines: Vec<Timeline>,
    pub duration: f32,
}

#[derive(Clone, Debug, Default)]
pub struct SkeletonData {
    pub hash: Option<String>,
    pub version: Option<String>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub reference_scale: f32,
    pub fps: f32,
    pub bones: Vec<BoneData>,
    pub slots: Vec<SlotData>,
    pub ik_constraints: Vec<IkConstraintData>,
    pub transform_constraints: Vec<TransformConstraintData>,
    pub path_constraints: Vec<PathConstraintData>,
    pub physics_constraints: Vec<PhysicsConstraintData>,
    /// Skin 0 is the default skin when `has_default_skin`.
    pub skins: Vec<Skin>,
    pub has_default_skin: bool,
    pub attachments: Vec<Attachment>,
    pub events: Vec<EventData>,
    pub animations: Vec<Animation>,
}

impl SkeletonData {
    pub fn find_bone(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.name == name)
    }

    pub fn find_slot(&self, name: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.name == name)
    }

    pub fn find_animation(&self, name: &str) -> Option<usize> {
        self.animations.iter().position(|a| a.name == name)
    }

    pub fn find_skin(&self, name: &str) -> Option<usize> {
        self.skins.iter().position(|s| s.name == name)
    }

    pub fn default_skin(&self) -> Option<&Skin> {
        if self.has_default_skin { self.skins.first() } else { None }
    }
}

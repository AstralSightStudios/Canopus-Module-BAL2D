//! Prints what a skeleton uses: counts, constraint kinds, attachment kinds, blend modes,
//! timeline kinds and animation names. Usage: `stats <file.skel> [<file.atlas>]...`
use std::collections::BTreeMap;

use bal2d_spine::*;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let skel = &args[i];
        i += 1;
        let atlas_path = args.get(i).filter(|a| a.ends_with(".atlas")).cloned();
        if atlas_path.is_some() {
            i += 1;
        }
        report(skel, atlas_path.as_deref());
    }
}

fn report(skel: &str, atlas_path: Option<&str>) {
    let bytes = std::fs::read(skel).expect("read skel");
    let mut d = match read_skeleton(&bytes, 1.0) {
        Ok(d) => d,
        Err(e) => {
            println!("{skel}: ERROR {e}");
            return;
        }
    };
    println!("== {skel}");
    println!(
        "version {:?} size {:.0}x{:.0} at ({:.0},{:.0}) refscale {}",
        d.version, d.width, d.height, d.x, d.y, d.reference_scale
    );
    println!(
        "bones {} slots {} ik {} transform {} path {} physics {} skins {} attachments {} events {} animations {}",
        d.bones.len(),
        d.slots.len(),
        d.ik_constraints.len(),
        d.transform_constraints.len(),
        d.path_constraints.len(),
        d.physics_constraints.len(),
        d.skins.len(),
        d.attachments.len(),
        d.events.len(),
        d.animations.len()
    );
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut verts, mut tris, mut weighted, mut infl) = (0usize, 0usize, 0usize, 0usize);
    let mut seq = 0;
    for a in &d.attachments {
        let k = match &a.kind {
            AttachmentKind::Region(r) => {
                seq += r.sequence.is_some() as usize;
                "region"
            }
            AttachmentKind::Mesh(m) => {
                seq += m.sequence.is_some() as usize;
                verts += m.vertices.vertex_count();
                tris += m.triangles.len() / 3;
                if m.vertices.is_weighted() {
                    weighted += 1;
                    infl += m.vertices.vertices.len() / 3;
                }
                if m.parent_mesh.is_some() { "linked-mesh" } else { "mesh" }
            }
            AttachmentKind::BoundingBox(_) => "bbox",
            AttachmentKind::Path(_) => "path",
            AttachmentKind::Point(_) => "point",
            AttachmentKind::Clipping(_) => "clipping",
        };
        *kinds.entry(k).or_default() += 1;
    }
    println!(
        "attachment kinds {kinds:?} sequences {seq}; mesh vertices {verts} triangles {tris} weighted meshes {weighted} influences {infl}"
    );
    let mut blends: BTreeMap<String, usize> = BTreeMap::new();
    let mut dark = 0;
    for s in &d.slots {
        *blends.entry(format!("{:?}", s.blend)).or_default() += 1;
        dark += s.dark.is_some() as usize;
    }
    println!("slot blends {blends:?} two-color slots {dark}");
    let inh: BTreeMap<String, usize> = d.bones.iter().fold(BTreeMap::new(), |mut m, b| {
        *m.entry(format!("{:?}", b.inherit)).or_default() += 1;
        m
    });
    println!("bone inherit {inh:?}");
    let mut tl: BTreeMap<String, usize> = BTreeMap::new();
    let mut deform_floats = 0usize;
    for a in &d.animations {
        for t in &a.timelines {
            let k = match t {
                Timeline::Attachment { .. } => "attachment".into(),
                Timeline::SlotColor { kind, .. } => format!("color-{kind:?}"),
                Timeline::Bone { property, .. } => format!("bone-{property:?}"),
                Timeline::Inherit { .. } => "inherit".into(),
                Timeline::Ik { .. } => "ik".into(),
                Timeline::Transform { .. } => "transform".into(),
                Timeline::Path { property, .. } => format!("path-{property:?}"),
                Timeline::Physics { property, .. } => format!("physics-{property:?}"),
                Timeline::PhysicsReset { .. } => "physics-reset".into(),
                Timeline::Deform { vertices, .. } => {
                    deform_floats += vertices.iter().map(|v| v.len()).sum::<usize>();
                    "deform".into()
                }
                Timeline::Sequence { .. } => "sequence".into(),
                Timeline::DrawOrder { .. } => "draw-order".into(),
                Timeline::Event { .. } => "event".into(),
            };
            *tl.entry(k).or_default() += 1;
        }
    }
    println!("timelines {tl:?} deform floats {deform_floats}");
    let names: Vec<String> = d.animations.iter().map(|a| format!("{}({:.1}s)", a.name, a.duration)).collect();
    println!("animations: {}", names.join(" "));
    println!("events: {}", d.events.iter().map(|e| e.name.as_str()).collect::<Vec<_>>().join(" "));
    let touch: Vec<&str> = d.bones.iter().map(|b| b.name.as_str()).filter(|n| n.contains("Touch")).collect();
    println!("touch bones: {touch:?}");
    if let Some(ap) = atlas_path {
        let text = std::fs::read_to_string(ap).expect("read atlas");
        let atlas = Atlas::parse(&text);
        let missing = bind_atlas(&mut d, &atlas);
        let pages: Vec<String> = atlas.pages.iter().map(|p| format!("{}({}x{} pma={})", p.name, p.width, p.height, p.pma)).collect();
        let area: i64 = atlas.regions.iter().map(|r| r.width as i64 * r.height as i64).sum();
        println!(
            "atlas pages {pages:?} regions {} packed area {:.2} Mpx; missing {}",
            atlas.regions.len(),
            area as f64 / 1e6,
            missing.len()
        );
        let rot = atlas.regions.iter().filter(|r| r.degrees != 0).count();
        println!("rotated regions {rot}");
    }
}

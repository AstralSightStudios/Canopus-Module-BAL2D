//! Reads a skeleton, writes it back, reads it again and compares the two data sets.
//! Usage: roundtrip <skel> [out.skel]
use bal2d_spine::*;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&a[0]).unwrap();
    let d1 = read_skeleton(&bytes, 1.0).unwrap();
    let out = writer::write_skeleton(&d1, 1.0);
    if let Some(p) = a.get(1) {
        std::fs::write(p, &out).unwrap();
    }
    let d2 = match read_skeleton(&out, 1.0) {
        Ok(d) => d,
        Err(e) => {
            println!("{}: re-read failed: {e}", a[0]);
            std::process::exit(1);
        }
    };
    let mut diffs = 0;
    let mut check = |what: String, ok: bool| {
        if !ok {
            diffs += 1;
            if diffs < 20 {
                println!("  diff: {what}");
            }
        }
    };
    check("bones".into(), format!("{:?}", d1.bones) == format!("{:?}", d2.bones));
    check("slots".into(), format!("{:?}", d1.slots) == format!("{:?}", d2.slots));
    check("ik".into(), format!("{:?}", d1.ik_constraints) == format!("{:?}", d2.ik_constraints));
    check("transform".into(), format!("{:?}", d1.transform_constraints) == format!("{:?}", d2.transform_constraints));
    check("path".into(), format!("{:?}", d1.path_constraints) == format!("{:?}", d2.path_constraints));
    check("physics".into(), format!("{:?}", d1.physics_constraints) == format!("{:?}", d2.physics_constraints));
    check("skins".into(), format!("{:?}", d1.skins) == format!("{:?}", d2.skins));
    check("attachments".into(), format!("{:?}", d1.attachments) == format!("{:?}", d2.attachments));
    check("events".into(), format!("{:?}", d1.events) == format!("{:?}", d2.events));
    // Animations: compare per timeline with a tolerance on curves (Beziers are re-sampled).
    let mut max_curve = 0f32;
    for (x, y) in d1.animations.iter().zip(&d2.animations) {
        check(format!("anim {} count", x.name), x.timelines.len() == y.timelines.len() && x.name == y.name && x.duration == y.duration);
        let key = |t: &Timeline| format!("{:?}", std::mem::discriminant(t)) + &match t {
            Timeline::Bone { bone, property, .. } => format!("{bone}{property:?}"),
            Timeline::SlotColor { slot, kind, .. } => format!("{slot}{kind:?}"),
            Timeline::Attachment { slot, .. } => format!("{slot}"),
            Timeline::Deform { slot, attachment, .. } => format!("{slot}/{attachment}"),
            Timeline::Ik { constraint, .. } | Timeline::Transform { constraint, .. } => format!("{constraint}"),
            Timeline::Path { constraint, property, .. } => format!("{constraint}{property:?}"),
            Timeline::Physics { constraint, property, .. } => format!("{constraint:?}{property:?}"),
            Timeline::PhysicsReset { constraint, .. } => format!("{constraint:?}"),
            Timeline::Inherit { bone, .. } => format!("{bone}"),
            _ => String::new(),
        };
        for t1 in &x.timelines {
            let Some(t2) = y.timelines.iter().find(|t| key(t) == key(t1)) else {
                check(format!("anim {} missing {}", x.name, key(t1)), false);
                continue;
            };
            let curves = |t: &Timeline| -> Option<Curve> {
                match t {
                    Timeline::Bone { curve, .. } | Timeline::SlotColor { curve, .. } | Timeline::Ik { curve, .. } | Timeline::Transform { curve, .. }
                    | Timeline::Path { curve, .. } | Timeline::Physics { curve, .. } | Timeline::Deform { curve, .. } => Some(curve.clone()),
                    _ => None,
                }
            };
            match (curves(t1), curves(t2)) {
                (Some(c1), Some(c2)) => {
                    let same_shape = c1.frames.len() == c2.frames.len() && c1.curves.len() == c2.curves.len();
                    check(format!("anim {} {} shape", x.name, key(t1)), same_shape);
                    if same_shape {
                        for (p, q) in c1.frames.iter().zip(&c2.frames) {
                            max_curve = max_curve.max((p - q).abs());
                        }
                        for (p, q) in c1.curves.iter().zip(&c2.curves) {
                            max_curve = max_curve.max((p - q).abs());
                        }
                    }
                    if let (Timeline::Deform { vertices: v1, .. }, Timeline::Deform { vertices: v2, .. }) = (t1, t2) {
                        let m = v1.iter().flatten().zip(v2.iter().flatten()).map(|(p, q)| (p - q).abs()).fold(0f32, f32::max);
                        max_curve = max_curve.max(m);
                    }
                }
                _ => check(format!("anim {} {} {:?}", x.name, key(t1), std::mem::discriminant(t1)), format!("{t1:?}") == format!("{t2:?}")),
            }
        }
    }
    println!("{}: {} -> {} bytes, {} diffs, max curve delta {:.6}", a[0], bytes.len(), out.len(), diffs, max_curve);
}

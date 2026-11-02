//! Lists constraints touching the named bones. Usage: constraints <skel> <bone>...
use bal2d_spine::*;
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let d = read_skeleton(&std::fs::read(&a[0]).unwrap(), 1.0).unwrap();
    for name in &a[1..] {
        let bi = d.find_bone(name).unwrap() as u16;
        let b = &d.bones[bi as usize];
        println!("bone {name} parent {:?} inherit {:?} len {}", b.parent.map(|p| d.bones[p as usize].name.clone()), b.inherit, b.length);
        for (i, c) in d.ik_constraints.iter().enumerate() {
            if c.bones.contains(&bi) || c.target == bi { println!("  ik#{i} {} order {} bones {:?} target {}", c.name, c.order, c.bones.iter().map(|x| &d.bones[*x as usize].name).collect::<Vec<_>>(), d.bones[c.target as usize].name); }
        }
        for (i, c) in d.transform_constraints.iter().enumerate() {
            if c.bones.contains(&bi) || c.target == bi { println!("  tc#{i} {} order {} local {} rel {} bones {:?} target {} mix r{} x{} y{} sx{} sy{} shy{}", c.name, c.order, c.local, c.relative, c.bones.iter().map(|x| &d.bones[*x as usize].name).collect::<Vec<_>>(), d.bones[c.target as usize].name, c.mix_rotate, c.mix_x, c.mix_y, c.mix_scale_x, c.mix_scale_y, c.mix_shear_y); }
        }
        for (i, c) in d.physics_constraints.iter().enumerate() {
            if c.bone == bi { println!("  ph#{i} {} order {} x{} y{} r{} sx{} shx{} inertia {} strength {} damping {} mass {} wind {} grav {} mix {} step {} limit {}", c.name, c.order, c.x, c.y, c.rotate, c.scale_x, c.shear_x, c.inertia, c.strength, c.damping, c.mass_inverse, c.wind, c.gravity, c.mix, c.step, c.limit); }
        }
    }
}

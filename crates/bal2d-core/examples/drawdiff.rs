//! Which slots draw differently between an original skeleton and a compiled pack at the same
//! moment: `drawdiff ORIG.skel ORIG.atlas PACK ANIM SECONDS`.
use bal2d_spine::state::{AnimationState, AnimationStateData};
use bal2d_spine::*;

fn pose(d: &SkeletonData, anim: &str, secs: f32, cam: bal2d_core::Camera) -> Vec<(String, String, [f32; 4], usize, [f32; 4])> {
    let mut sk = Skeleton::new(d);
    sk.update_world_transform(d, Physics::Update);
    let mut st = AnimationState::new(d, AnimationStateData { default_mix: 0.25, mixes: vec![] });
    st.set_animation(d, 0, d.find_animation(anim), true);
    let mut t = 0.0;
    while t < secs {
        st.update(d, 1.0 / 30.0);
        st.apply(d, &mut sk);
        sk.update(1.0 / 30.0);
        sk.update_world_transform(d, Physics::Update);
        st.output.clear();
        t += 1.0 / 30.0;
    }
    let mut list = bal2d_render::DrawList::default();
    bal2d_render::build(d, &sk, &mut list);
    list.batches
        .iter()
        .map(|b| {
            let v0 = b.first_vertex as usize * 2;
            let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for q in list.positions[v0..v0 + b.vertex_count as usize * 2].chunks_exact(2) {
                let (x, y) = ((q[0] - cam.cx) * cam.scale + 168.0, 240.0 - (q[1] - cam.cy) * cam.scale);
                bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x), bb[3].max(y)];
            }
            (d.slots[b.slot as usize].name.clone(), d.attachments[b.attachment as usize].name.clone(), b.color, b.index_count as usize / 3, bb)
        })
        .collect()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut orig = read_skeleton(&std::fs::read(&a[0]).unwrap(), 1.0).unwrap();
    bind_atlas(&mut orig, &Atlas::parse(&std::fs::read_to_string(&a[1]).unwrap()));
    let (lobby, _) = bal2d_core::Lobby::load(&std::fs::read(&a[2]).unwrap()).unwrap();
    let packed = &lobby.layers[0].data;
    let secs: f32 = a[4].parse().unwrap();
    let x = pose(&orig, &a[3], secs, lobby.camera);
    let y = pose(packed, &a[3], secs, lobby.camera);
    println!("orig draws {} slots, pack {}", x.len(), y.len());
    for e in &x {
        match y.iter().find(|f| f.0 == e.0) {
            None => println!("  only orig: {} / {} a={:.2} tris {} screen bbox {:.0?}", e.0, e.1, e.2[3], e.3, e.4),
            Some(f) if f.1 != e.1 || (f.2[3] - e.2[3]).abs() > 0.01 || f.3 != e.3 => {
                println!("  differs: {} orig {} a={:.2} tris {} / pack {} a={:.2} tris {}", e.0, e.1, e.2[3], e.3, f.1, f.2[3], f.3)
            }
            _ => {}
        }
    }
    for f in &y {
        if !x.iter().any(|e| e.0 == f.0) {
            println!("  only pack: {} / {} a={:.2} tris {}", f.0, f.1, f.2[3], f.3);
        }
    }
    // Draw order of the shared slots.
    let xo: Vec<&String> = x.iter().map(|e| &e.0).filter(|n| y.iter().any(|f| &f.0 == *n)).collect();
    let yo: Vec<&String> = y.iter().map(|e| &e.0).filter(|n| x.iter().any(|f| &f.0 == *n)).collect();
    if xo != yo {
        println!("  draw order differs");
    }
}

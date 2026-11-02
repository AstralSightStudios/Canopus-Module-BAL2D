//! Host time of the animation update's parts: `updsplit PACK...` (state update, timeline apply,
//! world transforms with physics), 600 frames after the intro, plus counts of what the skeleton
//! holds (bones, slots, constraints, timelines of the playing animations).
use std::time::Instant;

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (mut lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        for _ in 0..450 {
            lobby.update(1.0 / 30.0);
            lobby.notices.clear();
        }
        let (mut ts, mut ta, mut tw) = (0f64, 0f64, 0f64);
        let frames = 600;
        for _ in 0..frames {
            for l in lobby.layers.iter_mut() {
                let t0 = Instant::now();
                l.state.update(&l.data, 1.0 / 30.0);
                let t1 = Instant::now();
                l.state.apply(&l.data, &mut l.skeleton);
                let t2 = Instant::now();
                l.skeleton.update(1.0 / 30.0);
                l.skeleton.update_world_transform(&l.data, bal2d_spine::Physics::Update);
                let t3 = Instant::now();
                l.state.output.clear();
                ts += (t1 - t0).as_secs_f64();
                ta += (t2 - t1).as_secs_f64();
                tw += (t3 - t2).as_secs_f64();
            }
        }
        let us = |t: f64| t * 1e6 / frames as f64;
        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
        let d = &lobby.layers[lobby.layers.len() - 1].data;
        println!(
            "{name:14} state {:6.1} us  apply {:6.1} us  world {:6.1} us | bones {} slots {} physics {} ik {} transform {} path {}",
            us(ts),
            us(ta),
            us(tw),
            d.bones.len(),
            d.slots.len(),
            d.physics_constraints.len(),
            d.ik_constraints.len(),
            d.transform_constraints.len(),
            d.path_constraints.len()
        );
    }
}

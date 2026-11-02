//! Apply time by timeline kind: `applykinds PACK KIND...` times the update's timeline apply with
//! and without the timelines of the named kinds (Debug names: SlotColor, Bone, Deform, ...).
use std::time::Instant;

fn apply_us(lobby: &mut bal2d_core::Lobby) -> f64 {
    let mut t = 0.0;
    for _ in 0..600 {
        for l in lobby.layers.iter_mut() {
            l.state.update(&l.data, 1.0 / 30.0);
            let t0 = Instant::now();
            l.state.apply(&l.data, &mut l.skeleton);
            t += t0.elapsed().as_secs_f64();
            l.skeleton.update(1.0 / 30.0);
            l.skeleton.update_world_transform(&l.data, bal2d_spine::Physics::Update);
            l.state.output.clear();
        }
    }
    t * 1e6 / 600.0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).unwrap();
    let load = || {
        let (mut l, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        for _ in 0..450 {
            l.update(1.0 / 30.0);
            l.notices.clear();
        }
        l
    };
    let mut all = load();
    println!("all timelines: {:.1} us", apply_us(&mut all));
    let mut cut = load();
    for l in cut.layers.iter_mut() {
        for a in l.data.animations.iter_mut() {
            a.timelines.retain(|t| {
                let k = format!("{t:?}");
                !args[1..].iter().any(|n| k.starts_with(&format!("{n} ")) || k.starts_with(&format!("{n} {{")))
            });
        }
    }
    println!("without {:?}: {:.1} us", &args[1..], apply_us(&mut cut));
}

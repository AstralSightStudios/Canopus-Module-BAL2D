//! Prints where a pack's drag areas are on screen after its intro: `drags PACK [SECONDS]`.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).expect("read pack");
    let secs: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8.0);
    let (mut lobby, _) = bal2d_core::Lobby::load(&bytes).expect("load");
    lobby.start();
    let mut t = 0.0;
    while t < secs {
        lobby.update(1.0 / 30.0);
        t += 1.0 / 30.0;
    }
    println!("screen {}x{} camera {:?} idle {}", lobby.width, lobby.height, lobby.camera, lobby.is_idle());
    let l = &lobby.layers[0];
    for d in &lobby.interactions.drags {
        let Some(b) = l.data.find_bone(&d.bone) else { continue };
        let bone = &l.skeleton.bones[b];
        let (sx, sy) = lobby.world_to_screen(bone.world_x, bone.world_y);
        let (w, h) = (d.box_w * lobby.camera.scale, d.box_h * lobby.camera.scale);
        println!("drag {} at screen ({sx:.0}, {sy:.0}) box {w:.0}x{h:.0} px; grabs centre {}", d.bone, lobby.grabs(sx, sy));
    }
}

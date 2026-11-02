//! Per-phase framing end to end: `phasecam SKEL [OUT_DIR]` compiles with a talk camera 1.5x
//! closer than the idle one, plays to the idle loop, taps, and prints the camera as it blends
//! (and writes frames before and during the talk to OUT_DIR).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let p = std::path::Path::new(&args[0]);
    let dir = p.parent().unwrap();
    let stem = p.file_stem().unwrap().to_string_lossy().to_string();
    let atlas = std::fs::read_to_string(dir.join(format!("{stem}.atlas"))).unwrap();
    let pages = atlas
        .lines()
        .filter(|l| l.trim().ends_with(".png"))
        .map(|l| (l.trim().to_string(), std::fs::read(dir.join(l.trim())).unwrap().into()))
        .collect();
    let layers = vec![bal2d_compiler::LayerInput { name: stem, skel: std::fs::read(p).unwrap(), atlas, pages }];
    let opts0 = bal2d_compiler::Options { budget: 800_000, ..Default::default() };
    let data = bal2d_spine::binary::read_skeleton(&layers[0].skel, 1.0).unwrap();
    let base = bal2d_compiler::auto_camera(&data, 336, 480, 1.0);
    let talk = bal2d_compiler::Camera { scale: base.scale * 1.5, ..base };
    let opts = bal2d_compiler::Options { camera: Some(base), phase_cameras: vec![(bal2d_pack::phase::TALK, talk)], ..opts0 };
    let (bytes, _) = bal2d_compiler::compile(&layers, &opts, None).unwrap();
    let mut pv = bal2d_preview::PackPreview::load(&bytes).unwrap();
    pv.advance(12.0);
    let save = |pv: &mut bal2d_preview::PackPreview, name: &str| {
        if let Some(out) = args.get(1) {
            let (w, h) = pv.size();
            let png = bal2d_compiler::image::encode_png(&bal2d_compiler::image::Rgba { width: w as u32, height: h as u32, data: pv.render() });
            std::fs::write(format!("{out}/{name}.png"), png).unwrap();
        }
    };
    save(&mut pv, "idle");
    println!("idle: phase {} camera {:?}", pv.lobby.phase(), pv.lobby.camera);
    println!("idle {} free {} talks {}", pv.lobby.is_idle(), pv.lobby.is_free(), pv.lobby.interactions.talks.len());
    pv.lobby.tap();
    println!("after tap: phase {} free {}", pv.lobby.phase(), pv.lobby.is_free());
    for i in 0..8 {
        pv.advance(0.15);
        println!("+{:.2}s: phase {} scale {:.4}", (i + 1) as f32 * 0.15, pv.lobby.phase(), pv.lobby.camera.scale);
    }
    save(&mut pv, "talk");
    for _ in 0..40 {
        pv.advance(0.5);
    }
    println!("later: phase {} scale {:.4} (base {:.4})", pv.lobby.phase(), pv.lobby.camera.scale, base.scale);
}

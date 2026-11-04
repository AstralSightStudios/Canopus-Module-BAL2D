//! The baked calls that cover a pixel: `pixcalls PACK X Y [FRAMES]`, played into the idle loops
//! as `idlediff` does and stepped FRAMES times 0.37 s.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut pv = bal2d_preview::PackPreview::load(&std::fs::read(&args[0]).unwrap()).unwrap();
    let (x, y): (usize, usize) = (args[1].parse().unwrap(), args[2].parse().unwrap());
    let steps: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut t = 0.0;
    while !pv.lobby.idle_ready() && t < 120.0 {
        pv.advance(0.5);
        t += 0.5;
    }
    for _ in 0..steps {
        pv.advance(0.37);
    }
    let (w, h) = pv.size();
    for li in 0..pv.lobby.layers.len() {
        let names: Vec<String> = pv.lobby.layers[li].data.slots.iter().map(|s| s.name.clone()).collect();
        let shapes: Vec<(u16, u8)> = pv.lobby.idle[li].as_ref().map(|l| l.bake.shapes.iter().map(|s| (s.slot, s.clip)).collect()).unwrap_or_default();
        pv.idle_calls(li, |call, _| {
            let mut hit = false;
            bal2d_preview::raster::fill_polygons(&bal2d_preview::raster::polygons(call.path), (w, h), |px, py| hit |= px == x && py == y);
            if hit {
                let sh = (call.tag & 0xFFFF) as usize;
                let (slot, clip) = shapes.get(sh).copied().unwrap_or((u16::MAX, 0));
                println!("layer {li} shape {sh} slot {} clip {clip} blend {:?} color {:?} path {} bounds {:?}", names.get(slot as usize).map_or("?", |s| s.as_str()), call.blend, call.color, call.path.len(), call.bounds);
                if std::env::var("LOOPS").is_ok() {
                    for p in bal2d_preview::raster::polygons(call.path) {
                        let area: f32 = (0..p.len()).map(|i| { let (a, b) = (p[i], p[(i + 1) % p.len()]); a[0] * b[1] - b[0] * a[1] }).sum::<f32>() * 0.5;
                        println!("   loop {} pts area {area:.0}: {:?}", p.len(), p.iter().map(|q| (q[0] as i32, q[1] as i32)).collect::<Vec<_>>());
                    }
                }
            }
        });
    }
}

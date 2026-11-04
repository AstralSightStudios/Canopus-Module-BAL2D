//! Whether a talk looks the same whatever moment of the idle loop it starts at (so it could be
//! baked like the idle): `talkphase PACK [SHIFT_S]`. Two lobbies, one SHIFT_S further into its
//! idle loop, start the same talks; prints per talk the largest and mean distance (screen
//! pixels) between their main layer's vertices, over the talk and after its first 0.5 s.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pack = std::fs::read(&args[0]).unwrap();
    let shift: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1.37);
    let dt = 1.0 / 30.0;
    let load = || {
        let (mut l, _) = bal2d_core::Lobby::load(&pack).unwrap();
        l.start();
        let mut t = 0.0;
        while !l.idle_ready() && t < 120.0 {
            l.update(dt);
            l.notices.clear();
            t += dt;
        }
        l
    };
    let (mut a, mut b) = (load(), load());
    let mut s = 0.0;
    while s < shift {
        b.update(dt);
        b.notices.clear();
        s += dt;
    }
    let main = a.interactions.layer as usize;
    let scale = a.camera.scale;
    let talks = a.interactions.talks.len();
    let (mut la, mut lb) = (bal2d_render::DrawList::default(), bal2d_render::DrawList::default());
    for ti in 0..talks {
        a.tap();
        b.tap();
        let (mut max_all, mut max_late, mut sum, mut n, mut frames) = (0f32, 0f32, 0f64, 0usize, 0);
        let mut t = 0.0;
        while t < 30.0 {
            a.update(dt);
            b.update(dt);
            a.notices.clear();
            b.notices.clear();
            t += dt;
            frames += 1;
            a.draw_list(main, &mut la);
            b.draw_list(main, &mut lb);
            // Batches of the same attachments, vertex by vertex.
            for ba in &la.batches {
                let Some(bb) = lb.batches.iter().find(|x| x.slot == ba.slot && x.attachment == ba.attachment && x.vertex_count == ba.vertex_count) else { continue };
                for k in 0..ba.vertex_count as usize {
                    let (i, j) = ((ba.first_vertex as usize + k) * 2, (bb.first_vertex as usize + k) * 2);
                    let d = ((la.positions[i] - lb.positions[j]).powi(2) + (la.positions[i + 1] - lb.positions[j + 1]).powi(2)).sqrt() * scale;
                    max_all = max_all.max(d);
                    if t > 0.5 {
                        max_late = max_late.max(d);
                        sum += d as f64;
                        n += 1;
                    }
                }
            }
            if a.is_idle() && b.is_idle() && t > 1.0 {
                break;
            }
        }
        println!("talk {ti} {:?}: {frames} frames, largest {max_all:.1} px, after 0.5 s largest {max_late:.1} px, mean {:.2} px", a.interactions.talks[ti], sum / n.max(1) as f64);
        // Back into the idle loop, keeping the shift.
        let mut t = 0.0;
        while t < 2.0 {
            a.update(dt);
            b.update(dt);
            a.notices.clear();
            b.notices.clear();
            t += dt;
        }
    }
}

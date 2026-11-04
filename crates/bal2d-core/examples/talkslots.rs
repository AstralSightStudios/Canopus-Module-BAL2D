//! Which slots a lobby's talks leave where the idle loop has them (they could stay on the bake
//! during a talk): `talkslots PACK`. Plays the main layer with and without each talk from the
//! same state (two idle phases) and marks a slot moved when any vertex differs by more than a
//! quarter pixel, or its colour or attachment differs. Prints the share of the main layer's
//! triangles and batches in unmoved slots.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pack = std::fs::read(&args[0]).unwrap();
    let dt = 1.0 / 30.0;
    let (base, _) = bal2d_core::Lobby::load(&pack).unwrap();
    let main = base.interactions.layer as usize;
    let nslots = base.layers[main].data.slots.len();
    let mut moved = vec![false; nslots];
    let mut worst = vec![0f32; nslots];
    let talks = base.interactions.talks.len();
    let scale = base.camera.scale;
    for phase in [0.0f32, 2.3] {
        for ti in 0..talks {
            let start = || {
                let (mut l, _) = bal2d_core::Lobby::load(&pack).unwrap();
                l.start();
                let mut t = 0.0;
                while !l.idle_ready() && t < 120.0 {
                    l.update(dt);
                    l.notices.clear();
                    t += dt;
                }
                let mut s = 0.0;
                while s < phase {
                    l.update(dt);
                    l.notices.clear();
                    s += dt;
                }
                l
            };
            let (mut a, mut b) = (start(), start());
            for _ in 0..ti {
                b.tap();
                let mut t = 0.0;
                while t < 15.0 && !(b.is_idle() && b.is_free() && t > 1.0) {
                    b.update(dt);
                    b.notices.clear();
                    t += dt;
                }
            }
            // a: idle only, kept at b's moment.
            let tb = b.time;
            while a.time < tb {
                a.update(dt);
                a.notices.clear();
            }
            b.tap();
            let (mut la, mut lb) = (bal2d_render::DrawList::default(), bal2d_render::DrawList::default());
            let mut t = 0.0;
            while t < 15.0 {
                a.update(dt);
                b.update(dt);
                a.notices.clear();
                b.notices.clear();
                t += dt;
                a.draw_list(main, &mut la);
                b.draw_list(main, &mut lb);
                for bb in &lb.batches {
                    let s = bb.slot as usize;
                    if moved[s] {
                        continue;
                    }
                    let Some(ba) = la.batches.iter().find(|x| x.slot == bb.slot) else {
                        moved[s] = true;
                        continue;
                    };
                    if ba.attachment != bb.attachment || ba.vertex_count != bb.vertex_count || ba.color != bb.color {
                        moved[s] = true;
                        continue;
                    }
                    for k in 0..bb.vertex_count as usize {
                        let (i, j) = ((ba.first_vertex as usize + k) * 2, (bb.first_vertex as usize + k) * 2);
                        let d = ((la.positions[i] - lb.positions[j]).powi(2) + (la.positions[i + 1] - lb.positions[j + 1]).powi(2)).sqrt() * scale;
                        worst[s] = worst[s].max(d);
                        if d > 0.25 {
                            moved[s] = true;
                            break;
                        }
                    }
                }
                for ba in &la.batches {
                    if !lb.batches.iter().any(|x| x.slot == ba.slot) {
                        moved[ba.slot as usize] = true;
                    }
                }
                if t > 1.0 && b.is_idle() && b.is_free() {
                    break;
                }
            }
        }
    }
    // Share of the idle frame's work in unmoved slots.
    let (mut l, _) = bal2d_core::Lobby::load(&pack).unwrap();
    l.start();
    let mut t = 0.0;
    while !l.idle_ready() && t < 120.0 {
        l.update(dt);
        l.notices.clear();
        t += dt;
    }
    let mut list = bal2d_render::DrawList::default();
    l.draw_list(main, &mut list);
    let (mut tri_all, mut tri_kept, mut b_all, mut b_kept) = (0u32, 0u32, 0u32, 0u32);
    for b in &list.batches {
        let n = b.index_count / 3;
        tri_all += n;
        b_all += 1;
        if !moved[b.slot as usize] {
            tri_kept += n;
            b_kept += 1;
        }
    }
    if std::env::var("SLOTS").is_ok() {
        let mut v: Vec<(f32, &str)> = worst.iter().zip(&base.layers[main].data.slots).map(|(w, s)| (*w, s.name.as_str())).collect();
        v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for (w, n) in v.iter().filter(|x| x.0 > 0.0).take(12) {
            println!("  {n}: {w:.3} px");
        }
    }
    println!(
        "{}: {} talks; slots the talks leave alone: {} of {} drawn; {:.0}% of triangles, {:.0}% of batches",
        args[0],
        talks,
        b_kept,
        b_all,
        100.0 * tri_kept as f32 / tri_all.max(1) as f32,
        100.0 * b_kept as f32 / b_all.max(1) as f32
    );
}

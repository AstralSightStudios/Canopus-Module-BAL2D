//! Compares this runtime against a reference dump from `tools/oracle/oracle.mjs`.
//! Usage: golden <skel> <atlas> <scenario.json> <reference.json>
use bal2d_spine::skeleton::{compute_region_world, compute_vertex_world};
use bal2d_spine::state::{AnimationState, AnimationStateData, StateEventKind};
use bal2d_spine::*;
use serde_json::Value;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut data = read_skeleton(&std::fs::read(&a[0]).unwrap(), 1.0).unwrap();
    let atlas = Atlas::parse(&std::fs::read_to_string(&a[1]).unwrap());
    bind_atlas(&mut data, &atlas);
    let scenario: Value = serde_json::from_str(&std::fs::read_to_string(&a[2]).unwrap()).unwrap();
    let reference: Value = serde_json::from_str(&std::fs::read_to_string(&a[3]).unwrap()).unwrap();
    let ref_frames = reference["frames"].as_array().unwrap();

    let mut sk = Skeleton::new(&data);
    sk.update_world_transform(&data, Physics::Update);
    let mut st = AnimationState::new(
        &data,
        AnimationStateData { default_mix: scenario["defaultMix"].as_f64().unwrap_or(0.25) as f32, mixes: vec![] },
    );
    let dt = scenario["dt"].as_f64().unwrap() as f32;
    let nframes = scenario["frames"].as_u64().unwrap() as usize;
    let dump_every = scenario["dumpEvery"].as_u64().unwrap() as usize;
    let actions = scenario["actions"].as_array().unwrap();
    let mut poses: Vec<(usize, f32, f32, usize)> = vec![];
    let mut fired: Vec<String> = vec![];
    let mut ri = 0;
    let (mut max_err, mut max_at) = (0f32, String::new());
    let mut mismatches = 0;
    let mut worst_color = 0f32;
    for f in 0..nframes {
        for act in actions.iter().filter(|x| x["frame"].as_u64() == Some(f as u64)) {
            let track = act["track"].as_u64().unwrap_or(0) as usize;
            let anim = act["anim"].as_str().and_then(|n| data.find_animation(n));
            match act["op"].as_str().unwrap() {
                "set" => {
                    if anim.is_some() {
                        st.set_animation(&data, track, anim, act["loop"].as_bool().unwrap_or(false));
                    }
                }
                "add" => {
                    if anim.is_some() {
                        st.add_animation(&data, track, anim, act["loop"].as_bool().unwrap_or(false), act["delay"].as_f64().unwrap_or(0.0) as f32);
                    }
                }
                "setEmpty" => {
                    st.set_empty_animation(&data, track, act["mix"].as_f64().unwrap_or(0.0) as f32);
                }
                "addEmpty" => {
                    st.add_empty_animation(&data, track, act["mix"].as_f64().unwrap_or(0.0) as f32, act["delay"].as_f64().unwrap_or(0.0) as f32);
                }
                "bone" => {
                    if let Some(b) = data.find_bone(act["name"].as_str().unwrap()) {
                        poses.push((b, act["dx"].as_f64().unwrap() as f32, act["dy"].as_f64().unwrap() as f32, act["until"].as_u64().unwrap() as usize));
                    }
                }
                _ => {}
            }
        }
        st.update(&data, dt);
        st.apply(&data, &mut sk);
        for &(b, dx, dy, until) in &poses {
            if f < until {
                sk.bones[b].x = data.bones[b].x + dx;
                sk.bones[b].y = data.bones[b].y + dy;
            }
        }
        sk.update(dt);
        sk.update_world_transform(&data, Physics::Update);
        for ev in st.output.drain(..) {
            match ev.kind {
                StateEventKind::Event { animation, key } => {
                    for t in &data.animations[animation as usize].timelines {
                        if let Timeline::Event { events, .. } = t {
                            fired.push(format!("{}:{}", ev.track, data.events[events[key as usize].data as usize].name));
                        }
                    }
                }
                StateEventKind::Complete => fired.push(format!("{}:complete:{}", ev.track, ev.animation.map_or("<empty>".into(), |a| data.animations[a].name.clone()))),
                _ => {}
            }
        }
        if f % dump_every != 0 && f != nframes - 1 {
            continue;
        }
        let rf = &ref_frames[ri];
        ri += 1;
        assert_eq!(rf["frame"].as_u64().unwrap() as usize, f);
        let ref_events: Vec<String> = rf["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| match e.get("complete") {
                Some(c) => format!("{}:complete:{}", e["track"], c.as_str().unwrap()),
                None => format!("{}:{}", e["track"], e["name"].as_str().unwrap()),
            })
            .collect();
        if ref_events != fired {
            println!("frame {f}: events differ\n  ref {ref_events:?}\n  got {fired:?}");
            mismatches += 1;
        }
        fired.clear();
        // Our visible slots in draw order.
        let mut ours = vec![];
        for &si in &sk.draw_order {
            let slot = &sk.slots[si as usize];
            let Some(id) = slot.attachment else { continue };
            let att = &data.attachments[id as usize];
            let v = match &att.kind {
                AttachmentKind::Region(r) => {
                    let mut out = vec![0f32; 8];
                    compute_region_world(&sk.bones[slot.bone as usize], &r.offset, &mut out, 0, 2);
                    out
                }
                AttachmentKind::Mesh(m) => {
                    let n = m.vertices.world_len as usize;
                    let mut out = vec![0f32; n];
                    compute_vertex_world(&sk.bones, slot.bone as usize, &slot.deform, &m.vertices, 0, n, &mut out, 0, 2);
                    out
                }
                _ => continue,
            };
            ours.push((si, att.name.clone(), slot.color, v));
        }
        if let Some(rb) = rf.get("bones").and_then(|b| b.as_array()) {
            let mut worst: Vec<(f32, usize)> = vec![];
            for (bi, b) in rb.iter().enumerate() {
                let r: Vec<f32> = b.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
                let o = &sk.bones[bi];
                let mine = [o.a, o.b, o.c, o.d, o.world_x, o.world_y, o.x, o.y, o.rotation, o.scale_x, o.scale_y, o.shear_x, o.shear_y];
                let e = if std::env::var("ABCD").is_ok() {
                    r[..4].iter().zip(mine[..4].iter()).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max)
                } else {
                    r.iter().zip(mine.iter()).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max)
                };
                worst.push((e, bi));
            }
            if let Ok(list) = std::env::var("BONES") {
                if std::env::var("AT").ok().and_then(|x| x.parse::<usize>().ok()) == Some(f) {
                    for name in list.split(',') {
                        let bi = data.find_bone(name).unwrap();
                        let r: Vec<f64> = rb[bi].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
                        let o = &sk.bones[bi];
                        println!("  {name:16} ref w({:.3},{:.3}) abcd({:.4},{:.4},{:.4},{:.4})  got w({:.3},{:.3}) abcd({:.4},{:.4},{:.4},{:.4})", r[4], r[5], r[0], r[1], r[2], r[3], o.world_x, o.world_y, o.a, o.b, o.c, o.d);
                    }
                }
            }
            let first_bad = worst.iter().find(|(e, _)| *e > std::env::var("THR").ok().and_then(|t| t.parse().ok()).unwrap_or(0.05f32)).map(|(e, bi)| format!("{}({:.3})", data.bones[*bi].name, e));
            worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            if worst[0].0 > std::env::var("THR").ok().and_then(|t| t.parse().ok()).unwrap_or(0.05f32) {
                let names: Vec<String> = worst.iter().take(5).map(|(e, bi)| format!("{}({:.3})", data.bones[*bi].name, e)).collect();
                println!("frame {f}: first bad bone {:?}; worst {names:?}", first_bad);
                let bi = worst[0].1;
                let r: Vec<f64> = rb[bi].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
                let o = &sk.bones[bi];
                println!("   ref local x {:.3} y {:.3} r {:.3} sx {:.3} sy {:.3} shx {:.3} shy {:.3}", r[6], r[7], r[8], r[9], r[10], r[11], r[12]);
                println!("   got local x {:.3} y {:.3} r {:.3} sx {:.3} sy {:.3} shx {:.3} shy {:.3}", o.x, o.y, o.rotation, o.scale_x, o.scale_y, o.shear_x, o.shear_y);
            }
            if let Some(ri) = rf.get("ik").and_then(|b| b.as_array()) {
                for (ci, c) in ri.iter().enumerate() {
                    let r: Vec<f32> = c.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
                    let o = &sk.ik[ci];
                    let mine = [o.mix, o.softness, o.bend_direction as f32, o.compress as u8 as f32, o.stretch as u8 as f32, o.active as u8 as f32];
                    let e = r.iter().zip(mine.iter()).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
                    if e > 1e-4 {
                        println!("   ik {} ({}) ref {:?} got {:?}", ci, data.ik_constraints[ci].name, r, mine);
                    }
                }
            }
            if let Some(rp) = rf.get("physics").and_then(|b| b.as_array()) {
                for (ci, c) in rp.iter().enumerate() {
                    let r: Vec<f32> = c.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
                    let o = &sk.physics[ci];
                    let mine = [o.mix, o.inertia, o.strength, o.damping, o.mass_inverse, o.wind, o.gravity, o.active as u8 as f32];
                    let e = r.iter().zip(mine.iter()).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
                    if e > 1e-3 {
                        println!("   physics {} ({}) ref {:?} got {:?}", ci, data.physics_constraints[ci].name, r, mine);
                    }
                }
            }
        }
        let rslots = rf["slots"].as_array().unwrap();
        let mut frame_err = 0f32;
        let mut frame_at = String::new();
        let verbose = std::env::var("GOLDEN_VERBOSE").is_ok();
        if rslots.len() != ours.len() {
            println!("frame {f}: slot count ref {} got {}", rslots.len(), ours.len());
            mismatches += 1;
        }
        for (rs, (si, name, color, v)) in rslots.iter().zip(ours.iter()) {
            let _ = verbose;
            if rs["slot"].as_u64().unwrap() != *si as u64 || rs["att"].as_str().unwrap() != name {
                println!("frame {f}: slot/attachment ref {}:{} got {}:{}", rs["slot"], rs["att"], si, name);
                mismatches += 1;
                break;
            }
            let rc: Vec<f32> = rs["color"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
            for (a, b) in rc.iter().zip([color.r, color.g, color.b, color.a]) {
                worst_color = worst_color.max((a - b).abs());
            }
            let rv = rs["v"].as_array().unwrap();
            for (k, x) in rv.iter().enumerate().take(v.len()) {
                let e = (x.as_f64().unwrap() as f32 - v[k]).abs();
                if e > frame_err {
                    frame_err = e;
                    frame_at = name.clone();
                }
                if e > max_err {
                    max_err = e;
                    max_at = format!("frame {f} slot {si} {name} v{k}");
                }
            }
        }
        if std::env::var("GOLDEN_VERBOSE").is_ok() {
            println!("frame {f}: max {frame_err:.4} ({frame_at})");
        }
    }
    println!("compared {ri} frames: max vertex error {max_err:.4} at {max_at}; max color error {worst_color:.5}; mismatches {mismatches}");
}

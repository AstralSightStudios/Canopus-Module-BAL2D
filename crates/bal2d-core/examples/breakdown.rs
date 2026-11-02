//! Where a lobby's heap goes: `breakdown PACK` (skeleton data parts, skeleton instance, state).
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static COUNT: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        COUNT.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        COUNT.fetch_sub(1, Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn live() -> (usize, usize) {
    (LIVE.load(Ordering::Relaxed), COUNT.load(Ordering::Relaxed))
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let (lobby, _pages) = bal2d_core::Lobby::load(&bytes).unwrap();
    for (i, l) in lobby.layers.iter().enumerate() {
        let d = &l.data;
        let measure = |f: &dyn Fn() -> Box<dyn std::any::Any>| {
            let (b0, c0) = live();
            let v = f();
            let (b1, c1) = live();
            drop(v);
            (b1 - b0, c1 - c0)
        };
        let (all, all_n) = measure(&|| Box::new(d.clone()));
        let (anim, anim_n) = measure(&|| Box::new(d.animations.clone()));
        let (att, att_n) = measure(&|| Box::new(d.attachments.clone()));
        let (bones, _) = measure(&|| Box::new(d.bones.clone()));
        let (slots, _) = measure(&|| Box::new(d.slots.clone()));
        let (skins, _) = measure(&|| Box::new(d.skins.clone()));
        let (sk, _) = measure(&|| Box::new(l.skeleton.clone()));
        println!(
            "layer {i}: data {:.0} KB in {all_n} blocks: animations {:.0} KB ({anim_n} blocks), attachments {:.0} KB ({att_n} blocks), bones {:.0} KB, slots {:.0} KB, skins {:.0} KB; skeleton instance {:.0} KB",
            all as f64 / 1024.0,
            anim as f64 / 1024.0,
            att as f64 / 1024.0,
            bones as f64 / 1024.0,
            slots as f64 / 1024.0,
            skins as f64 / 1024.0,
            sk as f64 / 1024.0
        );
        // Floats in key frames vs sampled Bezier curves.
        let (mut f, mut c, mut beziers, mut timelines) = (0usize, 0usize, 0usize, 0usize);
        for a in &d.animations {
            for t in &a.timelines {
                let curve = match t {
                    bal2d_spine::Timeline::SlotColor { curve, .. } | bal2d_spine::Timeline::Bone { curve, .. } => Some(curve),
                    _ => None,
                };
                if let Some(curve) = curve {
                    timelines += 1;
                    f += curve.frames.len();
                    c += curve.curves.len();
                    let frames = curve.frames.len() / curve.entries.max(1) as usize;
                    beziers += (curve.curves.len() - frames) / bal2d_spine::BEZIER_SIZE;
                }
            }
        }
        println!("    curve timelines {timelines}: frame floats {f} ({:.0} KB), curve floats {c} ({:.0} KB), {beziers} Bezier segments", f as f64 * 4.0 / 1024.0, c as f64 * 4.0 / 1024.0);
        // Bezier segments that are flat or straight, and timelines that never change.
        let (mut flat, mut straight, mut constant, mut constant_frames) = (0usize, 0usize, 0usize, 0usize);
        for a in &d.animations {
            for t in &a.timelines {
                let curve = match t {
                    bal2d_spine::Timeline::SlotColor { curve, .. } | bal2d_spine::Timeline::Bone { curve, .. } => curve,
                    _ => continue,
                };
                let e = curve.entries as usize;
                let n = curve.frames.len() / e.max(1);
                let same = (1..n).all(|k| (1..e).all(|v| curve.frames[k * e + v] == curve.frames[v]));
                if same {
                    constant += 1;
                    constant_frames += n;
                }
                for k in 0..n.saturating_sub(1) {
                    let ty = curve.curves[k];
                    if ty < 2.0 {
                        continue;
                    }
                    let base = ty as usize - 2;
                    for v in 1..e {
                        let c = &curve.curves[base + (v - 1) * 4..base + (v - 1) * 4 + 4];
                        let (t1, v1) = (curve.frames[k * e], curve.frames[k * e + v]);
                        let (t2, v2) = (curve.frames[(k + 1) * e], curve.frames[(k + 1) * e + v]);
                        if v1 == v2 && c[1] == v1 && c[3] == v1 {
                            flat += 1;
                        } else {
                            // Controls on the chord: y(t) is then exactly linear in x(t).
                            let on = |cx: f32, cy: f32| {
                                let u = (cx - t1) / (t2 - t1);
                                (v1 + (v2 - v1) * u - cy).abs() <= 1e-4 * (1.0 + v1.abs().max(v2.abs()))
                            };
                            if on(c[0], c[1]) && on(c[2], c[3]) {
                                straight += 1;
                            }
                        }
                    }
                }
            }
        }
        println!("    flat Bezier segments {flat}, straight {straight}; constant timelines {constant} ({constant_frames} frames)");
        // Whole frames whose every channel's Bezier is flat or straight (-> linear), and RGBA
        // timelines whose RGB never changes (-> alpha only).
        let (mut linear_frames, mut bezier_frames, mut alpha_only, mut rgba) = (0usize, 0usize, 0usize, 0usize);
        for a in &d.animations {
            for t in &a.timelines {
                let (curve, is_rgba) = match t {
                    bal2d_spine::Timeline::SlotColor { curve, kind, .. } => (curve, *kind == bal2d_spine::SlotColorKind::Rgba),
                    bal2d_spine::Timeline::Bone { curve, .. } => (curve, false),
                    _ => continue,
                };
                let e = curve.entries as usize;
                let n = curve.frames.len() / e.max(1);
                if is_rgba {
                    rgba += 1;
                    if (1..n).all(|k| (1..4).all(|v| curve.frames[k * e + v] == curve.frames[v])) {
                        alpha_only += 1;
                    }
                }
                for k in 0..n.saturating_sub(1) {
                    let ty = curve.curves[k];
                    if ty < 2.0 {
                        continue;
                    }
                    bezier_frames += 1;
                    let base = ty as usize - 2;
                    let all = (1..e).all(|v| {
                        let c = &curve.curves[base + (v - 1) * 4..base + (v - 1) * 4 + 4];
                        let (t1, v1) = (curve.frames[k * e], curve.frames[k * e + v]);
                        let (t2, v2) = (curve.frames[(k + 1) * e], curve.frames[(k + 1) * e + v]);
                        let on = |cx: f32, cy: f32| {
                            let u = (cx - t1) / (t2 - t1);
                            (v1 + (v2 - v1) * u - cy).abs() <= 1e-4 * (1.0 + v1.abs().max(v2.abs()))
                        };
                        (v1 == v2 && c[1] == v1 && c[3] == v1) || (on(c[0], c[1]) && on(c[2], c[3]))
                    });
                    if all {
                        linear_frames += 1;
                    }
                }
            }
        }
        println!("    Bezier frames {bezier_frames}, of them all-flat/straight {linear_frames}; RGBA timelines {rgba}, RGB constant {alpha_only}");
        // The biggest animations.
        let mut sizes: Vec<(usize, String, usize)> = d
            .animations
            .iter()
            .map(|a| {
                let (b, _) = measure(&|| Box::new(a.clone()));
                (b, a.name.clone(), a.timelines.len())
            })
            .collect();
        sizes.sort_by(|a, b| b.0.cmp(&a.0));
        for (b, n, t) in sizes.iter().take(5) {
            println!("    {n}: {:.0} KB, {t} timelines", *b as f64 / 1024.0);
        }
    }
}

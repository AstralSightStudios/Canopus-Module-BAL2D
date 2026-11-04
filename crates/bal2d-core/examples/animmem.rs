//! Where a pack's animation floats go and what could shrink them: `animmem PACK...`. Per
//! animation: duration, keys, Bezier frames; multi-channel Bezier frames whose channels share one
//! easing once normalised (stored once instead of per channel); keys a neighbour pair already
//! gives (removable without changing the curve).
use bal2d_spine::{Curve, Timeline, BEZIER_SIZE, CURVE_BEZIER, CURVE_LINEAR};

fn curve_of(t: &Timeline) -> Option<&Curve> {
    match t {
        Timeline::SlotColor { curve, .. }
        | Timeline::Bone { curve, .. }
        | Timeline::Ik { curve, .. }
        | Timeline::Transform { curve, .. }
        | Timeline::Path { curve, .. }
        | Timeline::Physics { curve, .. }
        | Timeline::Deform { curve, .. } => Some(curve),
        _ => None,
    }
}

/// Normalised easing of channel `v` (1-based) of the Bezier frame `k`: (x1, y1, x2, y2), or None
/// when the channel does not move (any easing gives the same values).
fn eased(c: &Curve, k: usize, v: usize) -> Option<[f32; 4]> {
    let e = c.entries as usize;
    let (t1, t2) = (c.frames[k * e], c.frames[(k + 1) * e]);
    let (v1, v2) = if e == 1 { (0.0, 1.0) } else { (c.frames[k * e + v], c.frames[(k + 1) * e + v]) };
    let q = c.controls(c.curves[k], k * e, if e == 1 { 0 } else { v });
    if v1 == v2 && q[1] == v1 && q[3] == v1 {
        return None;
    }
    let dv = if v2 == v1 { 1.0 } else { v2 - v1 };
    Some([(q[0] - t1) / (t2 - t1), (q[1] - v1) / dv, (q[2] - t1) / (t2 - t1), (q[3] - v1) / dv])
}

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        println!("{path}");
        for (li, l) in lobby.layers.iter().enumerate() {
            for a in &l.data.animations {
                let (mut frames_f, mut curve_f, mut keys, mut bez, mut lin, mut step) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
                let (mut multi, mut shared, mut shared_save) = (0usize, 0usize, 0usize);
                let (mut deform_f, mut tl) = (0usize, 0usize);
                // Keys whose linear neighbours are collinear with them (linear in, linear out).
                let mut collinear = 0usize;
                for t in &a.timelines {
                    tl += 1;
                    if let Timeline::Deform { vertices, .. } = t {
                        deform_f += vertices.iter().map(|v| v.len()).sum::<usize>();
                    }
                    let Some(c) = curve_of(t) else { continue };
                    let e = c.entries as usize;
                    let n = c.frame_count();
                    frames_f += c.frames.len();
                    curve_f += c.curves.len();
                    keys += n;
                    // IK keys hold six floats but only mix and softness have curves.
                    let ch = if matches!(t, Timeline::Ik { .. }) { 2 } else { (e - 1).max(1) };
                    for k in 0..n.saturating_sub(1) {
                        let ty = c.curves[k];
                        if ty >= CURVE_BEZIER || ty <= -CURVE_BEZIER {
                            bez += 1;
                            if ch > 1 {
                                multi += 1;
                                let es: Vec<Option<[f32; 4]>> = (1..=ch).map(|v| eased(c, k, v)).collect();
                                let first = es.iter().flatten().next().copied();
                                let same = first.is_none_or(|f| es.iter().flatten().all(|g| (0..4).all(|i| (g[i] - f[i]).abs() < 1e-3)));
                                // Already stored once (negative type) or could be.
                                if ty <= -CURVE_BEZIER {
                                    shared += 1;
                                } else if same {
                                    shared += 1;
                                    shared_save += (ch - 1) * BEZIER_SIZE;
                                }
                            }
                        } else if ty == CURVE_LINEAR {
                            lin += 1;
                        } else {
                            step += 1;
                        }
                    }
                    if e >= 2 {
                        for k in 1..n.saturating_sub(1) {
                            if c.curves[k - 1] != CURVE_LINEAR || c.curves[k] != CURVE_LINEAR {
                                continue;
                            }
                            let (t0, t1, t2) = (c.frames[(k - 1) * e], c.frames[k * e], c.frames[(k + 1) * e]);
                            let ok = (1..e).all(|v| {
                                let (a0, a1, a2) = (c.frames[(k - 1) * e + v], c.frames[k * e + v], c.frames[(k + 1) * e + v]);
                                let mid = a0 + (a2 - a0) * (t1 - t0) / (t2 - t0);
                                (mid - a1).abs() <= 1e-4 * (1.0 + a1.abs())
                            });
                            if ok {
                                collinear += 1;
                            }
                        }
                    }
                }
                let kb = |f: usize| f as f64 * 4.0 / 1024.0;
                println!(
                    "  L{li} {:<24} {:5.1}s {:4} tl, {:6} keys ({:.0}/s/tl), seg bez {} lin {} step {}; frames {:.0} KB curves {:.0} KB deform {:.0} KB; multi-ch bez {} shared {} (-{:.0} KB); collinear keys {}",
                    a.name,
                    a.duration,
                    tl,
                    keys,
                    keys as f32 / a.duration.max(0.01) / tl.max(1) as f32,
                    bez,
                    lin,
                    step,
                    kb(frames_f),
                    kb(curve_f),
                    kb(deform_f),
                    multi,
                    shared,
                    kb(shared_save),
                    collinear
                );
            }
        }
    }
}

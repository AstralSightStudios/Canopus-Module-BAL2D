//! Slots a lobby's talks leave alone.
//!
//! A talk plays over the idle loop (tracks 1 and 2 over track 0), and many lobbies keep much of
//! the scene still while the character talks (Neru's city: 667 of 728 slots). Those slots stay
//! on the baked idle during a talk; only the rest is skinned and grouped live. The pack is
//! played with and without each talk from the same moment, at a few moments of the idle loop
//! (a talk's effect can depend on where the idle is), and a slot counts as left alone when none
//! of its vertices ever lands more than `MAX_PX` from where the idle alone puts it, and its
//! attachment and colour never differ.

use bal2d_core::Lobby;
use bal2d_render::DrawList;

/// Screen pixels a vertex may differ by.
const MAX_PX: f32 = 0.25;
/// Moments of the idle loop the talks start at (seconds after it is reached), and the longest a
/// talk is played.
const PHASES: [f32; 3] = [0.0, 2.3, 4.7];
const LONGEST_S: f32 = 20.0;

/// Per slot of the main layer: left alone by every talk. None when there is nothing to gain or
/// the main layer's animations change the draw order (the watch interleaves by it).
pub fn steady_slots(pack: &[u8]) -> Option<(usize, Vec<bool>)> {
    let dt = 1.0 / 30.0;
    let (probe, _) = Lobby::load(pack).ok()?;
    if probe.interactions.talks.is_empty() {
        return None;
    }
    let main = probe.interactions.layer as usize;
    let data = &probe.layers.get(main)?.data;
    if data.animations.iter().any(|a| a.timelines.iter().any(|t| matches!(t, bal2d_spine::Timeline::DrawOrder { .. }))) {
        return None;
    }
    let scale = probe.camera.scale;
    let n = data.slots.len();
    let mut moved = vec![false; n];
    let into_idle = |phase: f32| -> Option<Lobby> {
        let (mut l, _) = Lobby::load(pack).ok()?;
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
        Some(l)
    };
    let (mut la, mut lb) = (DrawList::default(), DrawList::default());
    for phase in PHASES {
        // One lobby plays the talks one after another; the other only the idle, kept at the
        // same moment.
        let mut b = into_idle(phase)?;
        let mut a = into_idle(phase)?;
        for _ in 0..probe.interactions.talks.len() {
            while a.time < b.time - dt * 0.5 {
                a.update(dt);
                a.notices.clear();
            }
            if !b.is_idle() || !b.is_free() {
                return None;
            }
            b.tap();
            let mut t = 0.0;
            while t < LONGEST_S {
                a.update(dt);
                b.update(dt);
                a.notices.clear();
                b.notices.clear();
                t += dt;
                a.draw_list(main, &mut la);
                b.draw_list(main, &mut lb);
                compare(&la, &lb, scale, &mut moved);
                if t > 1.0 && b.is_idle() && b.is_free() {
                    break;
                }
            }
            // Once the talk is over the two can differ in physics for a while: give it time to
            // settle back before the next talk is compared.
            let mut s = 0.0;
            while s < 1.0 {
                a.update(dt);
                b.update(dt);
                a.notices.clear();
                b.notices.clear();
                s += dt;
            }
        }
    }
    moved.iter().any(|&m| !m).then(|| (main, moved.into_iter().map(|m| !m).collect()))
}

/// Marks the slots whose batches differ between the two lists.
fn compare(la: &DrawList, lb: &DrawList, scale: f32, moved: &mut [bool]) {
    for bb in &lb.batches {
        let s = bb.slot as usize;
        if moved[s] {
            continue;
        }
        let Some(ba) = la.batches.iter().find(|x| x.slot == bb.slot) else {
            moved[s] = true;
            continue;
        };
        if ba.attachment != bb.attachment || ba.vertex_count != bb.vertex_count || ba.color != bb.color || ba.blend != bb.blend {
            moved[s] = true;
            continue;
        }
        for k in 0..bb.vertex_count as usize {
            let (i, j) = ((ba.first_vertex as usize + k) * 2, (bb.first_vertex as usize + k) * 2);
            let (dx, dy) = (la.positions[i] - lb.positions[j], la.positions[i + 1] - lb.positions[j + 1]);
            if (dx * dx + dy * dy).sqrt() * scale > MAX_PX {
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
}

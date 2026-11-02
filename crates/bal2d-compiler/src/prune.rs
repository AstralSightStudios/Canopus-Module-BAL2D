//! Drops what the watch never uses: animations the lobby never plays, and timelines that cannot
//! change a pixel (slots with nothing left to draw, bones nothing visible hangs from). Both cost
//! heap (most of a lobby's) and per-frame time on the watch, and change nothing on screen.

use bal2d_pack::Interactions;
use bal2d_spine::*;

/// The animation a non-main layer loops (as `bal2d_core::Lobby::start` picks it).
pub fn layer_idle(d: &SkeletonData) -> Option<usize> {
    d.animations
        .iter()
        .position(|a| a.name.starts_with("Idle"))
        .or_else(|| d.animations.iter().position(|a| a.name != "Dummy" && a.duration > 0.0))
}

/// Keeps only the animations the player can start: on the main layer the intro, the idle loop,
/// the talk pairs and the drag clips; on other layers their idle loop. Returns how many went.
pub fn played_only(d: &mut SkeletonData, main: bool, inter: &Interactions) -> usize {
    let keep: Vec<bool> = if main {
        let mut names: Vec<&str> = vec![&inter.intro, &inter.idle];
        for (m, a) in &inter.talks {
            names.push(m);
            names.push(a);
        }
        for dr in &inter.drags {
            names.extend([dr.ing_m.as_str(), dr.ing_a.as_str(), dr.end_m.as_str(), dr.end_a.as_str()]);
        }
        d.animations.iter().map(|a| names.contains(&a.name.as_str())).collect()
    } else {
        let idle = layer_idle(d);
        (0..d.animations.len()).map(|i| Some(i) == idle).collect()
    };
    let before = d.animations.len();
    let mut i = 0;
    d.animations.retain(|_| {
        i += 1;
        keep[i - 1]
    });
    before - d.animations.len()
}

/// Removes timelines whose target cannot affect a drawn pixel. `keep_bones` names bones the
/// player reads (drag bones). Returns how many timelines went.
pub fn dead_timelines(d: &mut SkeletonData, keep_bones: &[&str]) -> usize {
    let nb = d.bones.len();
    // Slots that can still show something.
    let mut live_slot = vec![false; d.slots.len()];
    for skin in &d.skins {
        for (slot, _, _) in &skin.attachments {
            live_slot[*slot as usize] = true;
        }
    }
    // Bones a visible thing depends on: slot bones and mesh weights, closed over ancestors and
    // the constraints that move them.
    let mut need = vec![false; nb];
    for (s, live) in live_slot.iter().enumerate() {
        if *live {
            need[d.slots[s].bone as usize] = true;
        }
    }
    for skin in &d.skins {
        for (_, _, id) in &skin.attachments {
            if let Some(v) = d.attachments[*id as usize].vertices() {
                let mut i = 0;
                while i < v.bones.len() {
                    let n = v.bones[i] as usize;
                    for b in &v.bones[i + 1..(i + 1 + n).min(v.bones.len())] {
                        need[*b as usize] = true;
                    }
                    i += n + 1;
                }
            }
        }
    }
    for name in keep_bones {
        if let Some(b) = d.find_bone(name) {
            need[b] = true;
        }
    }
    loop {
        let before = need.iter().filter(|x| **x).count();
        for b in 0..nb {
            if need[b] {
                let mut p = d.bones[b].parent;
                while let Some(q) = p {
                    need[q as usize] = true;
                    p = d.bones[q as usize].parent;
                }
            }
        }
        for c in &d.ik_constraints {
            if c.bones.iter().any(|b| need[*b as usize]) {
                need[c.target as usize] = true;
            }
        }
        for c in &d.transform_constraints {
            if c.bones.iter().any(|b| need[*b as usize]) {
                need[c.target as usize] = true;
            }
        }
        for c in &d.path_constraints {
            if c.bones.iter().any(|b| need[*b as usize]) {
                let slot = c.target as usize;
                live_slot[slot] = true;
                need[d.slots[slot].bone as usize] = true;
                for skin in &d.skins {
                    for (s, _, id) in &skin.attachments {
                        if *s as usize == slot {
                            if let Some(v) = d.attachments[*id as usize].vertices() {
                                let mut i = 0;
                                while i < v.bones.len() {
                                    let n = v.bones[i] as usize;
                                    for b in &v.bones[i + 1..(i + 1 + n).min(v.bones.len())] {
                                        need[*b as usize] = true;
                                    }
                                    i += n + 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        if need.iter().filter(|x| **x).count() == before {
            break;
        }
    }
    let ik_live: Vec<bool> = d.ik_constraints.iter().map(|c| c.bones.iter().any(|b| need[*b as usize])).collect();
    let tf_live: Vec<bool> = d.transform_constraints.iter().map(|c| c.bones.iter().any(|b| need[*b as usize])).collect();
    let path_live: Vec<bool> = d.path_constraints.iter().map(|c| c.bones.iter().any(|b| need[*b as usize])).collect();
    let phys_live: Vec<bool> = d.physics_constraints.iter().map(|c| need[c.bone as usize]).collect();
    let mut gone = 0;
    for a in &mut d.animations {
        let before = a.timelines.len();
        a.timelines.retain(|t| match t {
            Timeline::Attachment { slot, .. } | Timeline::SlotColor { slot, .. } => live_slot[*slot as usize],
            Timeline::Bone { bone, .. } | Timeline::Inherit { bone, .. } => need[*bone as usize],
            Timeline::Ik { constraint, .. } => ik_live[*constraint as usize],
            Timeline::Transform { constraint, .. } => tf_live[*constraint as usize],
            Timeline::Path { constraint, .. } => path_live[*constraint as usize],
            Timeline::Physics { constraint: Some(c), .. } | Timeline::PhysicsReset { constraint: Some(c), .. } => phys_live[*c as usize],
            _ => true,
        });
        gone += before - a.timelines.len();
    }
    gone
}

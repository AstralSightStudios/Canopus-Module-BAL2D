//! Mesh trimming: triangles that never reach the viewport in the simulation (with the same
//! margin as texture cropping, whose texels they would not have anyway) are removed, and so are
//! the vertices only they used. The watch then skins, transforms and culls fewer vertices every
//! frame (a lobby is framed for a wide screen: much of a body mesh lies below a band's).

use bal2d_spine::*;

/// Which triangles of each attachment the simulation saw on screen.
#[derive(Default)]
pub struct TriSeen {
    /// Per attachment: one flag per mesh triangle (empty until the mesh is drawn).
    pub seen: Vec<Vec<bool>>,
    /// Attachments whose batches did not map 1:1 onto their triangles (clipped): kept whole.
    pub unknown: Vec<bool>,
}

impl TriSeen {
    pub fn new(attachments: usize) -> TriSeen {
        TriSeen { seen: vec![Vec::new(); attachments], unknown: vec![false; attachments] }
    }
}

/// Trims every eligible mesh; returns (triangles, vertices) removed.
pub fn trim_meshes(d: &mut SkeletonData, tri: &TriSeen) -> (usize, usize) {
    // Meshes sharing geometry (linked meshes and their parents) are left alone.
    let mut shared = vec![false; d.attachments.len()];
    for (i, a) in d.attachments.iter().enumerate() {
        if let AttachmentKind::Mesh(m) = &a.kind {
            if let Some(p) = m.parent_mesh {
                shared[i] = true;
                shared[p as usize] = true;
            }
        }
    }
    let (mut tris_removed, mut verts_removed) = (0, 0);
    for i in 0..d.attachments.len() {
        if shared[i] || tri.unknown[i] {
            continue;
        }
        let AttachmentKind::Mesh(m) = &d.attachments[i].kind else { continue };
        let seen = &tri.seen[i];
        let n = m.triangles.len() / 3;
        if seen.len() != n || !seen.iter().any(|s| *s) || seen.iter().all(|s| *s) {
            continue;
        }
        let Some((map, kept)) = trim_mesh(d, i as u32, seen) else { continue };
        tris_removed += n - kept;
        verts_removed += map.iter().filter(|m| m.is_none()).count();
    }
    (tris_removed, verts_removed)
}

/// Removes attachment `id`'s unseen triangles and unused vertices, and remaps its deform
/// timelines. Returns the old -> new vertex map and the triangles kept.
fn trim_mesh(d: &mut SkeletonData, id: u32, seen: &[bool]) -> Option<(Vec<Option<u16>>, usize)> {
    let AttachmentKind::Mesh(m) = &mut d.attachments[id as usize].kind else { return None };
    let nv = m.vertices.vertex_count();
    let mut used = vec![false; nv];
    for (t, s) in m.triangles.chunks_exact(3).zip(seen) {
        if *s {
            for &v in t {
                used[v as usize] = true;
            }
        }
    }
    let mut map: Vec<Option<u16>> = vec![None; nv];
    let mut next = 0u16;
    for v in 0..nv {
        if used[v] {
            map[v] = Some(next);
            next += 1;
        }
    }
    // Triangles.
    let mut triangles = Vec::with_capacity(m.triangles.len());
    for (t, s) in m.triangles.chunks_exact(3).zip(seen) {
        if *s {
            triangles.extend(t.iter().map(|&v| map[v as usize].unwrap()));
        }
    }
    let kept = triangles.len() / 3;
    // The binary format derives the triangle count from the vertex and hull counts
    // (T = 2V - h - 2): the hull, editor-only data, is set to whatever makes that hold.
    let hull = (2 * next as usize).checked_sub(2 + kept)?;
    // Per-vertex ranges: (bones start, influence count, first influence) for weighted meshes.
    let weighted = m.vertices.is_weighted();
    let mut ranges = Vec::with_capacity(nv);
    if weighted {
        let (mut b, mut f) = (0usize, 0usize);
        for _ in 0..nv {
            let c = m.vertices.bones[b] as usize;
            ranges.push((b, c, f));
            b += 1 + c;
            f += c;
        }
    }
    let keep_pairs = |src: &[f32]| -> Vec<f32> {
        // One (x, y) per vertex.
        let mut out = Vec::with_capacity(next as usize * 2);
        for v in 0..nv {
            if used[v] {
                out.extend_from_slice(&src[v * 2..v * 2 + 2]);
            }
        }
        out
    };
    let keep_influences = |src: &[f32]| -> Vec<f32> {
        // One (x, y) per bone influence (weighted deform offsets).
        let mut out = Vec::new();
        for v in 0..nv {
            if used[v] {
                let (_, c, f) = ranges[v];
                out.extend_from_slice(&src[f * 2..(f + c) * 2]);
            }
        }
        out
    };
    let vertices = if weighted {
        let (mut bones, mut verts) = (Vec::new(), Vec::new());
        for v in 0..nv {
            if used[v] {
                let (b, c, f) = ranges[v];
                bones.extend_from_slice(&m.vertices.bones[b..b + 1 + c]);
                verts.extend_from_slice(&m.vertices.vertices[f * 3..(f + c) * 3]);
            }
        }
        Vertices { bones, vertices: verts, world_len: next as u32 * 2, checked: false, bone_bound: 0 }
    } else {
        Vertices { bones: Vec::new(), vertices: keep_pairs(&m.vertices.vertices), world_len: next as u32 * 2, checked: false, bone_bound: 0 }
    };
    let region_uvs = keep_pairs(&m.region_uvs);
    let uvs = if m.uvs.len() == nv * 2 { keep_pairs(&m.uvs) } else { m.uvs.clone() };
    // Deform timelines keyed to this attachment.
    for a in &mut d.animations {
        for t in &mut a.timelines {
            if let Timeline::Deform { attachment, vertices: frames, .. } = t {
                if *attachment == id {
                    for f in frames.iter_mut() {
                        *f = if weighted { keep_influences(f) } else { keep_pairs(f) };
                    }
                }
            }
        }
    }
    let AttachmentKind::Mesh(m) = &mut d.attachments[id as usize].kind else { return None };
    m.vertices = vertices;
    m.triangles = triangles;
    m.region_uvs = region_uvs;
    m.uvs = uvs;
    m.hull_length = (hull as u32) << 1;
    Some((map, kept))
}

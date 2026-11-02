//! Attachment geometry: region corner offsets, mesh page UVs.

use crate::data::*;
use crate::math::{cos_deg, sin_deg};

/// Computes a region attachment's local corner offsets and page UVs from its texture region.
/// Corner order is BL, TL, TR, BR (the order Spine renders a quad in).
pub fn update_region_attachment(r: &mut RegionAttachment) {
    let Some(region) = r.region else {
        r.uvs = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0];
        return;
    };
    let region_scale_x = r.width / region.original_width * r.scale_x;
    let region_scale_y = r.height / region.original_height * r.scale_y;
    let local_x = -r.width / 2.0 * r.scale_x + region.offset_x * region_scale_x;
    let local_y = -r.height / 2.0 * r.scale_y + region.offset_y * region_scale_y;
    let local_x2 = local_x + region.width * region_scale_x;
    let local_y2 = local_y + region.height * region_scale_y;
    let cos = cos_deg(r.rotation);
    let sin = sin_deg(r.rotation);
    let (x, y) = (r.x, r.y);
    let lxc = local_x * cos + x;
    let lxs = local_x * sin;
    let lyc = local_y * cos + y;
    let lys = local_y * sin;
    let lx2c = local_x2 * cos + x;
    let lx2s = local_x2 * sin;
    let ly2c = local_y2 * cos + y;
    let ly2s = local_y2 * sin;
    r.offset = [
        lxc - lys,
        lyc + lxs,
        lxc - ly2s,
        ly2c + lxs,
        lx2c - ly2s,
        ly2c + lx2s,
        lx2c - lys,
        lyc + lx2s,
    ];
    r.uvs = if region.degrees == 90 {
        [region.u2, region.v2, region.u, region.v2, region.u, region.v, region.u2, region.v]
    } else {
        [region.u, region.v2, region.u, region.v, region.u2, region.v, region.u2, region.v2]
    };
}

/// Maps a mesh's region-relative UVs into page UVs, undoing whitespace stripping and
/// packing rotation.
pub fn update_mesh_uvs(m: &mut MeshAttachment) {
    let n = m.region_uvs.len();
    m.uvs.clear();
    m.uvs.resize(n, 0.0);
    let ruv = &m.region_uvs;
    let uvs = &mut m.uvs;
    let Some(region) = m.region else {
        uvs.copy_from_slice(ruv);
        return;
    };
    let (tw, th) = (region.page_width, region.page_height);
    let (mut u, mut v) = (region.u, region.v);
    match region.degrees {
        90 => {
            u -= (region.original_height - region.offset_y - region.height) / tw;
            v -= (region.original_width - region.offset_x - region.width) / th;
            let width = region.original_height / tw;
            let height = region.original_width / th;
            for i in (0..n).step_by(2) {
                uvs[i] = u + ruv[i + 1] * width;
                uvs[i + 1] = v + (1.0 - ruv[i]) * height;
            }
        }
        180 => {
            u -= (region.original_width - region.offset_x - region.width) / tw;
            v -= region.offset_y / th;
            let width = region.original_width / tw;
            let height = region.original_height / th;
            for i in (0..n).step_by(2) {
                uvs[i] = u + (1.0 - ruv[i]) * width;
                uvs[i + 1] = v + (1.0 - ruv[i + 1]) * height;
            }
        }
        270 => {
            u -= region.offset_y / tw;
            v -= region.offset_x / th;
            let width = region.original_height / tw;
            let height = region.original_width / th;
            for i in (0..n).step_by(2) {
                uvs[i] = u + (1.0 - ruv[i + 1]) * width;
                uvs[i + 1] = v + ruv[i] * height;
            }
        }
        _ => {
            u -= region.offset_x / tw;
            v -= (region.original_height - region.offset_y - region.height) / th;
            let width = region.original_width / tw;
            let height = region.original_height / th;
            for i in (0..n).step_by(2) {
                uvs[i] = u + ruv[i] * width;
                uvs[i + 1] = v + ruv[i + 1] * height;
            }
        }
    }
}

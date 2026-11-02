//! Adding the BAL2D layer to a finished watchface (BIN or MWZ) at a chosen z position.

use crate::state::{self, FaceState};

fn describe(uid: u32) -> &'static str {
    match uid >> 24 & 0x0f {
        0 => "布局",
        2 => "图片",
        3 => "图片组",
        4 => "视频",
        5 => "数字",
        6 => "插槽",
        7 => "进度",
        8 => "组件",
        _ => "其他",
    }
}

/// Reads a picked watchface and lists its layers (first theme, bottom first).
pub fn load(name: &str, data: Vec<u8>) -> Result<(), String> {
    let bin = if data.starts_with(b"PK") {
        let entries = bal2d_face::zip::read(&data).map_err(|e| format!("MWZ 读取失败：{e}"))?;
        entries.into_iter().find(|e| e.0 == "resource.bin").map(|e| e.1).ok_or("MWZ 里没有 resource.bin。")?
    } else {
        data.clone()
    };
    let face = bal2d_face::Face::parse(&bin).map_err(|e| format!("不是可识别的表盘：{e}"))?;
    let theme = face.themes.iter().position(|t| !t.aod).unwrap_or(0);
    let layers: Vec<String> = face
        .layouts(&bin, theme)
        .iter()
        .map(|l| format!("{} 0x{:08x} · 位置 ({}, {})", describe(l.resource_uid), l.resource_uid, l.x, l.y))
        .collect();
    let has_marker = face.has_marker(&bin);
    // Default: right above the background image.
    let z = if layers.is_empty() { 0 } else { 1 };
    state::with_state(|s| {
        s.face = FaceState {
            name: if face.name.is_empty() { name.to_string() } else { face.name.clone() },
            data,
            layers,
            z,
            has_marker,
            patched: None,
        }
    });
    Ok(())
}

/// Patches the loaded face for a screen of `size`; the result replaces `patched`.
pub fn patch(size: (u32, u32)) -> Result<(), String> {
    let snap = state::snapshot().face;
    if snap.data.is_empty() {
        return Err("请先选择表盘文件。".into());
    }
    let place = bal2d_face::Placement {
        z: if snap.z >= snap.layers.len() { usize::MAX } else { snap.z },
        x: 0,
        y: 0,
        width: size.0 as u16,
        height: size.1 as u16,
    };
    // AstroBox installs the BIN; an MWZ is unpacked to it.
    let bin = if snap.data.starts_with(b"PK") {
        bal2d_face::patch_mwz(&snap.data, place).map_err(|e| e.to_string())?.1
    } else {
        bal2d_face::patch_bin(&snap.data, place).map_err(|e| e.to_string())?
    };
    let file = format!("{}-bal2d.bin", snap.name.replace(['/', '\\', ' '], "_"));
    state::with_state(|s| s.face.patched = Some((file, bin)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_face_is_listed_and_patched() {
        let Ok(mwz) = std::fs::read("/Volumes/EXT0/tmp/bal2d-ext/faces/compass-10p.mwz") else { return };
        load("compass.mwz", mwz).unwrap();
        let s = state::snapshot().face;
        assert!(!s.has_marker);
        assert_eq!(s.z, 1);
        assert!(s.layers.len() > 10, "{}", s.layers.len());
        patch((336, 480)).unwrap();
        let (file, bin) = state::snapshot().face.patched.unwrap();
        assert!(file.ends_with("-bal2d.bin"));
        let face = bal2d_face::Face::parse(&bin).unwrap();
        assert!(face.has_marker(&bin));
        assert_eq!(face.layouts(&bin, 0)[1].resource_uid & !0x0078_0000, bal2d_face::MARKER_UID);
    }
}

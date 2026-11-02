//! The framing editor: an overview of the whole scene (a small compile of everything, rendered
//! once per lobby phase) under a box the shape of the watch's screen, moved and resized by hand,
//! one camera per phase (`bal2d_pack::phase`; phases without their own use the idle one).

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use bal2d_compiler::{Camera, Options};
use bal2d_core::Touch;
use bal2d_pack::{PageFormat, phase};
use bal2d_spine::read_skeleton;

use crate::state::Asset;

/// Largest overview, CSS pixels: it is shown at this size.
const MAX_W: f32 = 300.0;
const MAX_H: f32 = 400.0;
/// Texels the overview pack may use (small: it is only looked at in the plugin).
const OVERVIEW_BUDGET: u32 = 250_000;

#[derive(Clone, Debug)]
pub struct Overview {
    /// Skeleton -> overview pixels (y down), as a pack camera at `size`.
    pub cam: Camera,
    pub size: (u32, u32),
    /// The watch screen the boxes have the shape of.
    pub screen: (u32, u32),
    /// A moment of each phase (PNG data URL); `None` where the lobby has no such phase.
    pub frames: [Option<String>; phase::COUNT],
    /// The framing the compiler would pick by itself (idle).
    pub default: Camera,
}

/// One camera per phase; the idle one is always set.
pub type Framing = [Option<Camera>; phase::COUNT];

pub fn phase_name(p: u8) -> &'static str {
    match p {
        phase::INTRO => "开场",
        phase::TALK => "对话",
        phase::TOUCH => "摸头",
        _ => "待机",
    }
}

/// Compiles the whole scene small and renders each phase once.
pub fn overview(assets: &[Asset], screen: (u32, u32), progress: &mut dyn FnMut(&str, f32)) -> Result<(Overview, Framing), String> {
    let layers = crate::assets::layers(assets)?;
    let mut main = None;
    let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for l in &layers {
        let d = read_skeleton(&l.skel, 1.0).map_err(|e| format!("{}：无法读取骨骼（{e:?}）", l.name))?;
        if d.width > 0.0 && d.height > 0.0 {
            bounds = [bounds[0].min(d.x), bounds[1].min(d.y), bounds[2].max(d.x + d.width), bounds[3].max(d.y + d.height)];
        }
        if main.is_none() || d.find_bone("Touch_Point").is_some() {
            main = Some(d);
        }
    }
    let main = main.ok_or("没有骨骼")?;
    let default = bal2d_compiler::auto_camera(&main, screen.0, screen.1, 1.0);
    // The scene: the skeletons' bounds and at least twice the default view around it.
    let (vw, vh) = (screen.0 as f32 / default.scale, screen.1 as f32 / default.scale);
    let view = [default.cx - vw, default.cy - vh, default.cx + vw, default.cy + vh];
    bounds = if bounds[0] < bounds[2] {
        [bounds[0].min(view[0] + vw * 0.5), bounds[1].min(view[1] + vh * 0.5), bounds[2].max(view[2] - vw * 0.5), bounds[3].max(view[3] - vh * 0.5)]
    } else {
        view
    };
    let (bw, bh) = (bounds[2] - bounds[0], bounds[3] - bounds[1]);
    let k = (MAX_W / bw).min(MAX_H / bh);
    let size = (((bw * k) as u32).max(16) & !1, ((bh * k) as u32).max(16) & !1);
    let cam = Camera { cx: (bounds[0] + bounds[2]) * 0.5, cy: (bounds[1] + bounds[3]) * 0.5, scale: k };
    let opts = Options {
        width: size.0,
        height: size.1,
        camera: Some(cam),
        format: PageFormat::Bgra8888,
        budget: OVERVIEW_BUDGET,
        bake: false,
        keep_layers: true,
        name: "overview".into(),
        ..Default::default()
    };
    let inter = crate::assets::interactions(assets, &layers);
    let (bytes, _) = bal2d_compiler::compile_progress(&layers, &opts, inter, &mut |_, f| progress("生成场景预览", f * 0.8))
        .map_err(|e| format!("场景预览失败：{e}"))?;
    drop(layers);
    let mut pv = bal2d_preview::PackPreview::load(&bytes).map_err(|e| format!("场景预览失败：{e:?}"))?;
    drop(bytes);
    let shot = |pv: &mut bal2d_preview::PackPreview| -> String {
        let (w, h) = pv.size();
        let png = bal2d_compiler::image::encode_png(&bal2d_compiler::image::Rgba { width: w as u32, height: h as u32, data: pv.render() });
        format!("data:image/png;base64,{}", BASE64.encode(png))
    };
    let mut frames: [Option<String>; phase::COUNT] = Default::default();
    // Intro: half way through it.
    let intro = {
        let l = &pv.lobby;
        let m = &l.layers[(l.interactions.layer as usize).min(l.layers.len() - 1)];
        m.data.find_animation(&l.interactions.intro).map(|i| m.data.animations[i].duration)
    };
    progress("渲染各阶段画面", 0.82);
    if let Some(d) = intro.filter(|d| *d > 0.1) {
        pv.advance(d * 0.5);
        frames[phase::INTRO as usize] = Some(shot(&mut pv));
        pv.advance(d * 0.5);
    }
    // Idle: once the idle loop has settled.
    pv.advance(1.5);
    frames[phase::IDLE as usize] = Some(shot(&mut pv));
    progress("渲染各阶段画面", 0.88);
    // Talk: a second into the first one.
    if !pv.lobby.interactions.talks.is_empty() {
        pv.lobby.tap();
        pv.advance(1.0);
        if pv.lobby.phase() == phase::TALK {
            frames[phase::TALK as usize] = Some(shot(&mut pv));
        }
        for _ in 0..40 {
            if pv.lobby.phase() == phase::IDLE && pv.lobby.is_free() {
                break;
            }
            pv.advance(0.5);
        }
    }
    progress("渲染各阶段画面", 0.94);
    // Touch: a pat in progress (press on the drag area, hold past its delay, move a little).
    if let Some((x, y, w, h)) = pv.lobby.grab_box() {
        let (cx, cy) = (x + w * 0.5, y + h * 0.5);
        let delay = pv.lobby.interactions.drags.first().map_or(0.3, |d| d.delay);
        pv.touch(Touch::Down, cx, cy);
        pv.advance(delay + 0.2);
        pv.touch(Touch::Move, cx + w * 0.3, cy + h * 0.1);
        pv.advance(0.5);
        if pv.lobby.phase() == phase::TOUCH {
            frames[phase::TOUCH as usize] = Some(shot(&mut pv));
        }
        pv.touch(Touch::Up, cx + w * 0.3, cy + h * 0.1);
    }
    progress("完成", 1.0);
    let mut framing: Framing = [None; phase::COUNT];
    framing[phase::IDLE as usize] = Some(default);
    Ok((Overview { cam, size, screen, frames, default }, framing))
}

/// The box of `c` on the overview, CSS pixels: (x, y, w, h), y down.
pub fn frame_rect(o: &Overview, c: Camera) -> (f32, f32, f32, f32) {
    let (ow, oh) = (o.size.0 as f32, o.size.1 as f32);
    let w = o.screen.0 as f32 / c.scale * o.cam.scale;
    let h = o.screen.1 as f32 / c.scale * o.cam.scale;
    let x = (c.cx - o.cam.cx) * o.cam.scale + ow * 0.5 - w * 0.5;
    let y = oh * 0.5 - (c.cy - o.cam.cy) * o.cam.scale - h * 0.5;
    (x, y, w, h)
}

/// The camera whose box is (x, y, w) on the overview (the height follows the screen's shape).
pub fn camera_of(o: &Overview, x: f32, y: f32, w: f32) -> Camera {
    let (ow, oh) = (o.size.0 as f32, o.size.1 as f32);
    let w = w.max(8.0);
    let h = w * o.screen.1 as f32 / o.screen.0 as f32;
    let scale = o.screen.0 as f32 * o.cam.scale / w;
    Camera { cx: o.cam.cx + (x + w * 0.5 - ow * 0.5) / o.cam.scale, cy: o.cam.cy - (y + h * 0.5 - oh * 0.5) / o.cam.scale, scale }
}

/// `c` moved by (dx, dy) overview pixels.
pub fn moved(o: &Overview, c: Camera, dx: f32, dy: f32) -> Camera {
    let (x, y, w, _) = frame_rect(o, c);
    camera_of(o, x + dx, y + dy, w)
}

/// `c` with its bottom-right corner dragged by (dx, dy) (top-left fixed, shape kept).
pub fn resized(o: &Overview, c: Camera, dx: f32, dy: f32) -> Camera {
    let (x, y, w, _) = frame_rect(o, c);
    let aspect = o.screen.0 as f32 / o.screen.1 as f32;
    // The larger of the two moves, in width terms.
    let grow = if dx.abs() >= (dy * aspect).abs() { dx } else { dy * aspect };
    camera_of(o, x, y, (w + grow).clamp(o.size.0 as f32 * 0.08, o.size.0 as f32 * 3.0))
}

/// `c` zoomed to `percent` of the default framing's scale, about its centre.
pub fn zoomed(o: &Overview, c: Camera, percent: f32) -> Camera {
    Camera { scale: o.default.scale * (percent / 100.0).clamp(0.2, 6.0), ..c }
}

/// `c` with its box centred at (x, y) overview pixels.
pub fn centred(o: &Overview, c: Camera, x: f32, y: f32) -> Camera {
    let (_, _, w, h) = frame_rect(o, c);
    camera_of(o, x - w * 0.5, y - h * 0.5, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "/Volumes/EXT0/tmp/bal2d-ext/assets/Shiroko_home";

    fn assets() -> Option<Vec<Asset>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(DIR).ok()?.flatten() {
            crate::assets::recognise(&e.file_name().to_string_lossy(), std::fs::read(e.path()).ok()?, &mut out);
        }
        Some(out)
    }

    #[test]
    fn the_overview_shows_every_phase_and_boxes_map_back_to_cameras() {
        let Some(assets) = assets() else { return };
        let t0 = std::time::Instant::now();
        let (o, f) = overview(&assets, (336, 480), &mut |_, _| {}).unwrap();
        eprintln!("overview {}x{} in {:?}", o.size.0, o.size.1, t0.elapsed());
        if let Ok(dir) = std::env::var("FRAMING_OUT") {
            for (p, frame) in o.frames.iter().enumerate() {
                if let Some(url) = frame {
                    let png = BASE64.decode(url.trim_start_matches("data:image/png;base64,")).unwrap();
                    std::fs::write(format!("{dir}/phase{p}.png"), png).unwrap();
                }
            }
        }
        for p in [phase::IDLE, phase::INTRO, phase::TALK, phase::TOUCH] {
            assert!(o.frames[p as usize].is_some(), "phase {p} has a frame");
        }
        let c = f[phase::IDLE as usize].unwrap();
        // The default box lies on the overview and maps back to its camera.
        let (x, y, w, h) = frame_rect(&o, c);
        assert!(x >= -1.0 && y >= -1.0 && x + w <= o.size.0 as f32 + 1.0 && y + h <= o.size.1 as f32 + 1.0, "{:?}", (x, y, w, h));
        let back = camera_of(&o, x, y, w);
        assert!((back.cx - c.cx).abs() < 0.01 && (back.cy - c.cy).abs() < 0.01 && (back.scale / c.scale - 1.0).abs() < 1e-4);
        // Moving and resizing do what they say on the overview.
        let m = frame_rect(&o, moved(&o, c, 10.0, -5.0));
        assert!((m.0 - x - 10.0).abs() < 0.01 && (m.1 - y + 5.0).abs() < 0.01 && (m.2 - w).abs() < 0.01);
        let r = frame_rect(&o, resized(&o, c, 20.0, 0.0));
        assert!((r.0 - x).abs() < 0.01 && (r.1 - y).abs() < 0.01 && (r.2 - w - 20.0).abs() < 0.01);
        assert!((r.3 / r.2 - 480.0 / 336.0).abs() < 1e-3);
    }
}

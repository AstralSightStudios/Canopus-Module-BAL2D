//! The plugin page: import (pick files, compile, preview, install), the packs on the watch, and
//! the watchface layer tool.

use serde::Deserialize;

use crate::astrobox::psys_host_v4::{
    dialog::{self, DialogButton, DialogInfo, DialogStyle, DialogType, FilterConfig, PickConfig},
    queue, ui,
};
use crate::state::{self, Tab, UiState};
use crate::view::{Kind, On, View};
use crate::{assets, face, library, online, watch};

const EV_TAB: &str = "action:tab.";
const EV_DEVICE: &str = "input:device";
const EV_CONNECT: &str = "action:connect";
const EV_REFRESH: &str = "action:refresh";
const EV_ADD: &str = "action:assets.add";
const EV_CLEAR: &str = "action:assets.clear";
const EV_NAME: &str = "input:name";
const EV_ZOOM: &str = "input:zoom";
const EV_QUALITY: &str = "input:quality";
const EV_IDLE: &str = "input:idle";
const EV_BUILD: &str = "action:build";
const EV_INSTALL: &str = "action:install";
const EV_CANCEL: &str = "action:cancel";
const EV_SELECT: &str = "action:pack.select.";
const EV_DELETE: &str = "action:pack.delete.";
const EV_FLUENT: &str = "action:fluent";
const EV_DIRECT: &str = "action:direct";
const EV_LOG: &str = "action:export-log";
const EV_SINGLE: &str = "action:single";
const EV_FACE_PICK: &str = "action:face.pick";
const EV_FACE_Z: &str = "input:face.z";
const EV_FACE_PATCH: &str = "action:face.patch";
const EV_FACE_QUEUE: &str = "action:face.queue";
const EV_FACE_SAVE: &str = "action:face.save";
const EV_FRAME_OPEN: &str = "action:frame.open";
const EV_FRAME_PHASE: &str = "action:frame.phase.";
const EV_FRAME_OWN: &str = "action:frame.own";
const EV_FRAME_RESET: &str = "action:frame.reset";
const EV_FRAME_MOVE: &str = "pointer:frame.move";
const EV_FRAME_RESIZE: &str = "pointer:frame.resize";
const EV_FRAME_DRAG: &str = "pointer:frame.drag";
const EV_FRAME_UP: &str = "pointer:frame.up";
const EV_FRAME_ZOOM: &str = "input:frame.zoom";
const EV_FRAME_X: &str = "input:frame.x";
const EV_FRAME_Y: &str = "input:frame.y";
const EV_LANG: &str = "input:lang";
const EV_LIB_LOAD: &str = "action:lib.load";
const EV_LIB_QUERY: &str = "input:lib.query";
const EV_LIB_GET: &str = "action:lib.get.";
const EV_LIB_FRAME: &str = "action:lib.frame.";
const EV_LIB_ROUTE: &str = "input:lib.route";
const EV_LIB_PREFIX: &str = "input:lib.prefix";
const EV_LIB_AUTO: &str = "action:lib.auto";
/// Rows of the online library shown at once (the rest are reached by searching).
const LIB_ROWS: usize = 50;
/// Download progress redraws the page at most this often (ms).
const PROGRESS_REDRAW_MS: u128 = 150;
/// The box is redrawn at most this often while dragged (ms): each redraw sends the whole page.
const FRAME_REDRAW_MS: u128 = 40;

mod tone {
    pub const TEXT: &str = "var(--gray-12)";
    pub const MUTED: &str = "var(--gray-11)";
    pub const WELL: &str = "var(--gray-a3)";
    pub const RED_WELL: &str = "var(--red-a3)";
    pub const RED: &str = "var(--red-11)";
    pub const GREEN_WELL: &str = "var(--green-a3)";
    pub const GREEN: &str = "var(--green-11)";
}

#[derive(Default, Deserialize)]
struct Payload {
    #[serde(default)]
    value: Option<String>,
    #[serde(default, rename = "clientX")]
    client_x: Option<f64>,
    #[serde(default, rename = "clientY")]
    client_y: Option<f64>,
}

static FRAME_DRAWN: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// The camera of the phase shown (its own, else the idle one).
fn frame_camera(s: &UiState) -> Option<bal2d_compiler::Camera> {
    let f = s.framing?;
    f[s.frame_phase as usize].or(f[bal2d_pack::phase::IDLE as usize])
}

/// Sets the shown phase's camera (giving the phase its own framing).
fn set_frame_camera(s: &mut UiState, c: bal2d_compiler::Camera) {
    if let Some(f) = &mut s.framing {
        f[s.frame_phase as usize] = Some(c);
        s.built = None;
    }
}

/// Pointer events of the framing box. Returns whether to redraw now.
fn frame_pointer(event: &str, p: &Payload) -> bool {
    let (Some(x), Some(y)) = (p.client_x, p.client_y) else { return event == EV_FRAME_UP };
    let (x, y) = (x as f32, y as f32);
    match event {
        // The handle sits inside the box: its press arrives first, then the box's (same point).
        EV_FRAME_RESIZE => state::with_state(|s| s.frame_drag = Some((true, x, y))),
        EV_FRAME_MOVE => state::with_state(|s| {
            if !matches!(s.frame_drag, Some((true, px, py)) if px == x && py == y) {
                s.frame_drag = Some((false, x, y));
            }
        }),
        EV_FRAME_DRAG => {
            let moved = state::with_state(|s| {
                let (Some((resize, lx, ly)), Some(o), Some(c)) = (s.frame_drag, s.overview.clone(), frame_camera(s)) else { return false };
                let next = if resize { crate::framing::resized(&o, c, x - lx, y - ly) } else { crate::framing::moved(&o, c, x - lx, y - ly) };
                set_frame_camera(s, next);
                s.frame_drag = Some((resize, x, y));
                true
            });
            if !moved {
                return false;
            }
            let mut last = FRAME_DRAWN.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|t| t.elapsed().as_millis() < FRAME_REDRAW_MS) {
                return false;
            }
            *last = Some(std::time::Instant::now());
            return true;
        }
        _ => state::with_state(|s| s.frame_drag = None),
    }
    true
}

pub fn render_main_ui(root: &str) {
    state::with_state(|s| s.root = Some(root.to_string()));
    rerender();
}

pub fn rerender() {
    let s = state::snapshot();
    if let Some(root) = &s.root {
        ui::render(root, page(&s).into_element());
    }
}

/// The screen packs are compiled for: the connected watch's, else the device chosen by name.
fn screen(s: &UiState) -> Option<(u32, u32)> {
    s.hello.as_ref().map(|h| (h.width as u32, h.height as u32))
}

pub async fn on_event(event: &str, payload: &str) {
    let parsed = serde_json::from_str::<Payload>(payload).unwrap_or_default();
    if event.starts_with("pointer:frame.") {
        if frame_pointer(event, &parsed) {
            rerender();
        }
        return;
    }
    let value = parsed.value.clone().unwrap_or_default();
    match event {
        EV_FRAME_OPEN => open_framing().await,
        e if e.starts_with(EV_FRAME_PHASE) => state::with_state(|s| s.frame_phase = e[EV_FRAME_PHASE.len()..].parse().unwrap_or(0)),
        EV_FRAME_OWN => state::with_state(|s| {
            let p = s.frame_phase as usize;
            if p != bal2d_pack::phase::IDLE as usize {
                if let Some(f) = &mut s.framing {
                    f[p] = if f[p].is_some() { None } else { f[bal2d_pack::phase::IDLE as usize] };
                    s.built = None;
                }
            }
        }),
        EV_FRAME_RESET => state::with_state(|s| {
            let p = s.frame_phase as usize;
            let default = s.overview.as_ref().map(|o| o.default);
            if let Some(f) = &mut s.framing {
                f[p] = if p == bal2d_pack::phase::IDLE as usize { default } else { None };
                s.built = None;
            }
        }),
        EV_FRAME_ZOOM | EV_FRAME_X | EV_FRAME_Y => state::with_state(|s| {
            let (Some(o), Some(c)) = (s.overview.clone(), frame_camera(s)) else { return };
            let Ok(v) = value.parse::<f32>() else { return };
            let (x, y, w, h) = crate::framing::frame_rect(&o, c);
            let next = match event {
                EV_FRAME_ZOOM => crate::framing::zoomed(&o, c, v),
                EV_FRAME_X => crate::framing::centred(&o, c, v, y + h * 0.5),
                _ => crate::framing::centred(&o, c, x + w * 0.5, v),
            };
            set_frame_camera(s, next);
        }),
        EV_NAME => return state::with_state(|s| s.name = value),
        EV_LANG => state::with_state(|s| {
            s.bubble_lang = value;
            s.built = None;
        }),
        EV_LIB_LOAD => load_library().await,
        EV_LIB_QUERY => state::with_state(|s| s.lib.query = value),
        EV_LIB_ROUTE => {
            online::forget_source();
            state::with_state(|s| s.lib.route = value);
        }
        EV_LIB_PREFIX => {
            online::forget_source();
            return state::with_state(|s| s.lib.custom_prefix = value.trim().to_string());
        }
        EV_LIB_AUTO => state::with_state(|s| s.lib.auto_install = !s.lib.auto_install),
        e if e.starts_with(EV_LIB_GET) => download_lobby(&e[EV_LIB_GET.len()..]).await,
        e if e.starts_with(EV_LIB_FRAME) => download_for_framing(&e[EV_LIB_FRAME.len()..]).await,
        EV_ZOOM => {
            state::with_state(|s| {
                s.zoom = value.parse().unwrap_or(100);
                s.built = None;
            });
        }
        EV_QUALITY => {
            state::with_state(|s| {
                s.high_quality = value == "high";
                s.built = None;
            });
        }
        EV_IDLE => {
            state::with_state(|s| {
                s.idle_mode = value.to_string();
                s.built = None;
            });
        }
        EV_DEVICE => {
            state::with_state(|s| {
                if s.selected_addr != value {
                    s.hello = None;
                    s.packs.clear();
                }
                s.selected_addr = value;
            });
            watch::hello().await;
        }
        // The device list is read at load and on AstroBox device events: a watch connected
        // after the plugin opened may only show up when asked again.
        EV_CONNECT | EV_REFRESH => {
            crate::connect::refresh_devices().await;
            watch::hello().await;
        }
        e if e.starts_with(EV_TAB) => {
            let tab = match &e[EV_TAB.len()..] {
                "watch" => Tab::Watch,
                "face" => Tab::Face,
                _ => Tab::Import,
            };
            state::with_state(|s| s.tab = tab);
            if tab == Tab::Watch {
                watch::hello().await;
                watch::stats().await;
            }
        }
        EV_ADD => add_files().await,
        EV_CLEAR => state::with_state(|s| {
            s.assets.clear();
            s.built = None;
            s.overview = None;
            s.framing = None;
            s.name.clear();
            s.name_rev += 1;
            s.character = None;
            s.status.clear();
        }),
        EV_BUILD => build().await,
        EV_INSTALL => {
            if let Err(e) = watch::install().await {
                state::set_status(e, true);
            }
        }
        EV_CANCEL => watch::cancel().await,
        e if e.starts_with(EV_SELECT) => {
            if let Ok(id) = e[EV_SELECT.len()..].parse() {
                watch::select_pack(id).await;
            }
        }
        e if e.starts_with(EV_DELETE) => {
            if let Ok(id) = e[EV_DELETE.len()..].parse::<u32>() {
                let name = state::snapshot().packs.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_default();
                if confirm("删除角色", &format!("确定从手表删除“{name}”吗？"), "删除").await {
                    watch::delete_pack(id).await;
                }
            }
        }
        EV_FLUENT => {
            let current = state::snapshot().hello.map(|h| h.options).unwrap_or(bal2d_proto::options::DEFAULT);
            watch::set_options(current ^ bal2d_proto::options::FLUENT).await;
        }
        EV_DIRECT => {
            let current = state::snapshot().hello.map(|h| h.options).unwrap_or(bal2d_proto::options::DEFAULT);
            watch::set_options(current ^ bal2d_proto::options::CLASSIC).await;
        }
        EV_SINGLE => {
            let current = state::snapshot().hello.map(|h| h.options).unwrap_or(bal2d_proto::options::DEFAULT);
            watch::set_options(current ^ bal2d_proto::options::SINGLE).await;
        }
        EV_LOG => watch::fetch_log().await,
        EV_FACE_PICK => pick_face().await,
        EV_FACE_Z => state::with_state(|s| {
            s.face.z = value.parse().unwrap_or(0);
            s.face.patched = None;
        }),
        EV_FACE_PATCH => {
            let s = state::snapshot();
            let size = screen(&s).unwrap_or((336, 480));
            match face::patch(size) {
                Ok(()) => state::set_status("已生成带角色图层的表盘。", false),
                Err(e) => state::set_status(e, true),
            }
        }
        EV_FACE_QUEUE => queue_face(),
        EV_FACE_SAVE => save_face().await,
        _ => {}
    }
    rerender();
}

async fn confirm(title: &str, content: &str, action: &str) -> bool {
    dialog::show_dialog(
        DialogType::Alert,
        DialogStyle::System,
        DialogInfo {
            title: title.into(),
            content: content.into(),
            buttons: vec![
                DialogButton { id: "cancel".into(), primary: false, content: "取消".into() },
                DialogButton { id: "confirm".into(), primary: true, content: action.into() },
            ],
        },
    )
    .await
    .clicked_btn_id
        == "confirm"
}

async fn pick(extensions: &[&str]) -> Option<(String, Vec<u8>)> {
    let result = dialog::pick_file(
        PickConfig { read: true, copy_to: None },
        FilterConfig {
            multiple: false,
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
            default_directory: String::new(),
            default_file_name: String::new(),
        },
    )
    .await;
    match result {
        Ok(r) if !r.name.is_empty() => Some((r.name, r.data)),
        Ok(_) => None,
        Err(e) => {
            state::set_status(format!("无法选择文件：{e}"), true);
            None
        }
    }
}

async fn add_files() {
    let Some((name, data)) = pick(&["skel", "atlas", "png", "zip", "json", "txt"]).await else { return };
    let mut assets = state::snapshot().assets;
    let n = assets::recognise(&name, data, &mut assets);
    let suggested = assets::suggested_name(&assets);
    state::with_state(|s| {
        if s.assets.is_empty() {
            s.character = None;
        }
        s.assets = assets;
        s.built = None;
        s.overview = None;
        s.framing = None;
        if s.name.is_empty() && !suggested.is_empty() {
            s.name = suggested;
            s.name_rev += 1;
        }
    });
    if n == 0 {
        state::set_status(format!("“{name}”不是可识别的骨骼、图集、贴图、互动参数或压缩包。"), true);
    } else {
        state::set_status("", false);
    }
}

async fn build() {
    let s = state::snapshot();
    let Some(size) = screen(&s) else {
        state::set_status("请先连接手表：角色包要按手表的屏幕编译。", true);
        return;
    };
    state::with_state(|s| {
        s.busy = true;
        s.progress = Some(("准备".into(), 0));
    });
    state::set_status("", false);
    rerender();
    let (dialogue, dialogue_note) = match library::Lang::from_setting(&s.bubble_lang) {
        None => (None, String::new()),
        Some(lang) => {
            let mut show = progress_sink(|s, p| s.progress = p);
            show("准备对话台词", 0.0);
            match online::dialogue(s.character, &assets::lobby_names(&s.assets), lang, &route(&s), &mut show) {
                Ok(Some(d)) => (Some(d), String::new()),
                Ok(None) => (None, " · 没有找到这个大厅的对话台词".to_string()),
                Err(e) => {
                    tracing::warn!("dialogue: {e}");
                    (None, " · 对话台词未能下载（网络不可用）".to_string())
                }
            }
        }
    };
    let req = assets::BuildRequest {
        assets: s.assets.clone(),
        name: s.name.clone(),
        screen: size,
        zoom: s.zoom,
        high_quality: s.high_quality,
        idle_mode: s.idle_mode.clone(),
        framing: s.framing,
        dialogue,
        dialogue_note,
    };
    // Each step is shown as it starts (the compile runs in this call; the UI is redrawn from here).
    let mut shown = (String::new(), u32::MAX);
    let result = assets::build(&req, &mut |label, f| {
        let percent = (f * 100.0) as u32;
        if label != shown.0 || percent != shown.1 {
            shown = (label.to_string(), percent);
            state::with_state(|s| s.progress = Some((label.to_string(), percent)));
            rerender();
        }
    });
    state::with_state(|st| {
        st.busy = false;
        st.progress = None;
        match result {
            Ok(b) => {
                st.built = Some(b);
                st.status.clear();
                st.error = false;
            }
            Err(e) => {
                st.built = None;
                st.status = e;
                st.error = true;
            }
        }
    });
}

fn route(s: &UiState) -> online::Route {
    online::Route { kind: s.lib.route.clone(), prefix: s.lib.custom_prefix.clone() }
}

/// A progress callback for blocking work (downloads): stores `(label, percent)` with `set` and
/// redraws the page at most every [`PROGRESS_REDRAW_MS`].
fn progress_sink(set: fn(&mut UiState, Option<(String, u32)>)) -> impl FnMut(&str, f32) {
    let mut shown = (String::new(), u32::MAX);
    let mut drawn: Option<std::time::Instant> = None;
    move |label, f| {
        let percent = (f.clamp(0.0, 1.0) * 100.0) as u32;
        if label == shown.0 && percent == shown.1 {
            return;
        }
        shown = (label.to_string(), percent);
        state::with_state(|s| set(s, Some((label.to_string(), percent))));
        if drawn.is_none_or(|t| t.elapsed().as_millis() >= PROGRESS_REDRAW_MS) {
            drawn = Some(std::time::Instant::now());
            rerender();
        }
    }
}

/// Downloads the online library's lobby list (with Chinese names).
async fn load_library() {
    let s = state::snapshot();
    if s.lib.loading || s.lib.active.is_some() {
        return;
    }
    state::with_state(|s| {
        s.lib.loading = true;
        s.lib.progress = Some(("连接".into(), 0));
    });
    state::set_status("", false);
    rerender();
    let result = online::load_catalog(&route(&s), &mut progress_sink(|s, p| s.lib.progress = p));
    state::with_state(|s| {
        s.lib.loading = false;
        s.lib.progress = None;
    });
    match result {
        Ok((entries, warning)) => {
            state::with_state(|s| s.lib.entries = entries);
            state::set_status(warning.unwrap_or_default(), false);
        }
        Err(e) => state::set_status(format!("{e}。可以在“下载线路”里换一条线路再试。"), true),
    }
}

/// Downloads a library lobby's files into the asset list (replacing it) and names the pack after
/// the student. Returns the entry once the files are in; `lib.active` stays set (the caller
/// clears it when its own steps are done).
async fn fetch_lobby(id: &str) -> Option<library::LibEntry> {
    let s = state::snapshot();
    if s.busy || s.transfer.is_some() || s.lib.loading || s.lib.active.is_some() {
        return None;
    }
    let entry = s.lib.entries.iter().find(|e| e.id == id).cloned()?;
    state::with_state(|s| {
        s.busy = true;
        s.lib.active = Some(entry.id.clone());
        s.lib.progress = Some(("准备下载".into(), 0));
    });
    state::set_status("", false);
    rerender();
    let result = online::download_lobby(&entry, &route(&s), &mut progress_sink(|s, p| s.lib.progress = p));
    let files = match result {
        Ok(files) => files,
        Err(e) => {
            state::with_state(|s| {
                s.busy = false;
                s.lib.active = None;
                s.lib.progress = None;
            });
            state::set_status(format!("{e}。可以在“下载线路”里换一条线路再试。"), true);
            return None;
        }
    };
    let mut assets = Vec::new();
    for (name, data) in files {
        assets::recognise(&name, data, &mut assets);
    }
    state::with_state(|s| {
        s.assets = assets;
        s.name = entry.name.clone();
        s.name_rev += 1;
        s.character = entry.character;
        s.built = None;
        s.overview = None;
        s.framing = None;
        s.busy = false;
        s.lib.progress = None;
    });
    Some(entry)
}

/// One click in the online library: downloads the lobby, builds it with the current settings
/// and, if the watch is connected and the toggle is on, installs it.
async fn download_lobby(id: &str) {
    let Some(entry) = fetch_lobby(id).await else { return };
    if state::snapshot().hello.is_none() {
        state::with_state(|s| s.lib.active = None);
        state::set_status(format!("已下载“{}”的素材。连接手表后点“编译并预览”。", entry.name), false);
        return;
    }
    build().await;
    state::with_state(|s| s.lib.active = None);
    let after = state::snapshot();
    if after.built.is_none() {
        return;
    }
    if after.lib.auto_install && after.hello.is_some() {
        rerender();
        if let Err(e) = watch::install().await {
            state::set_status(e, true);
        }
    } else {
        state::set_status(format!("已编译“{}”。确认预览后点“安装到手表”。", entry.name), false);
    }
}

/// The other library button: downloads the lobby and opens the framing editor on it, without
/// building or installing (the user frames it, then builds and installs as for their own files).
async fn download_for_framing(id: &str) {
    let Some(entry) = fetch_lobby(id).await else { return };
    if state::snapshot().hello.is_none() {
        state::with_state(|s| s.lib.active = None);
        state::set_status(format!("已下载“{}”的素材。连接手表后在下方“取景”里打开取景器（取景按手表的屏幕比例进行）。", entry.name), false);
        return;
    }
    open_framing().await;
    state::with_state(|s| s.lib.active = None);
    if state::snapshot().framing.is_some() {
        state::set_status(format!("已下载“{}”，取景器在下方。调整好后点“编译并预览”，再点“安装到手表”。", entry.name), false);
    }
}

/// Generates the scene overview for the framing editor (a small compile, then a frame of each
/// phase), showing progress like a build.
async fn open_framing() {
    let s = state::snapshot();
    let Some(size) = screen(&s) else {
        state::set_status("请先连接手表：取景按手表的屏幕比例进行。", true);
        return;
    };
    state::with_state(|s| {
        s.busy = true;
        s.progress = Some(("生成场景预览".into(), 0));
    });
    state::set_status("", false);
    rerender();
    let mut shown = (String::new(), u32::MAX);
    let result = crate::framing::overview(&s.assets, size, &mut |label, f| {
        let percent = (f * 100.0) as u32;
        if label != shown.0 || percent != shown.1 {
            shown = (label.to_string(), percent);
            state::with_state(|s| s.progress = Some((label.to_string(), percent)));
            rerender();
        }
    });
    state::with_state(|st| {
        st.busy = false;
        st.progress = None;
        match result {
            Ok((o, f)) => {
                st.overview = Some(o);
                st.framing = Some(f);
                st.frame_phase = bal2d_pack::phase::IDLE;
                st.built = None;
            }
            Err(e) => {
                st.status = e;
                st.error = true;
            }
        }
    });
}

async fn pick_face() {
    let Some((name, data)) = pick(&["bin", "mwz", "zip"]).await else { return };
    if let Err(e) = face::load(&name, data) {
        state::set_status(e, true);
    } else {
        state::set_status("", false);
    }
}

fn queue_face() {
    let Some((file, bin)) = state::snapshot().face.patched else { return };
    let _ = std::fs::create_dir_all("media");
    let path = format!("media/{file}");
    if let Err(e) = std::fs::write(&path, &bin) {
        state::set_status(format!("写入失败：{e}"), true);
        return;
    }
    queue::add_resource_to_queue(queue::ResourceType::Watchface, &path);
    state::set_status("已加入 AstroBox 安装队列。安装后在手表上切换到这个表盘即可看到角色。", false);
}

async fn save_face() {
    let Some((file, bin)) = state::snapshot().face.patched else { return };
    let filter = FilterConfig { multiple: false, extensions: vec!["bin".into()], default_directory: String::new(), default_file_name: file };
    let session = match dialog::save_file_start(filter).await {
        Ok(s) => s,
        Err(e) => {
            state::set_status(format!("无法保存：{e}"), true);
            return;
        }
    };
    for chunk in bin.chunks(256 * 1024) {
        if let Err(e) = dialog::save_file_write_chunk(session.session_id, chunk.to_vec()).await {
            dialog::save_file_abort(session.session_id).await;
            state::set_status(format!("保存失败：{e}"), true);
            return;
        }
    }
    match dialog::save_file_finish(session.session_id).await {
        Ok(()) => state::set_status(format!("已保存为 {}。", session.name), false),
        Err(e) => state::set_status(format!("保存失败：{e}"), true),
    }
}

// -- views ---------------------------------------------------------------------------------------

fn text(content: &str, size: u32, color: &str) -> View {
    View::new(Kind::P, Some(content)).font_size(size).color(color)
}

fn heading(content: &str) -> View {
    text(content, 16, tone::TEXT).class("rt-r-weight-bold")
}

fn button(label: &str, event: &str, variant: &str) -> View {
    View::new(Kind::Button, None)
        .prop("variant", variant)
        .prop("size", "2")
        .on(On::Click, event)
        .child(View::new(Kind::Span, Some(label)))
}

fn badge(label: &str, color: &str) -> View {
    View::new(Kind::Badge, Some(label)).prop("color", color).prop("variant", "soft").prop("radius", "full")
}

fn card() -> View {
    View::new(Kind::Card, None).prop("size", "2").column().gap(12)
}

fn row() -> View {
    View::new(Kind::Div, None).row().align_center().gap(10)
}

fn stack(gap: u32) -> View {
    View::new(Kind::Div, None).column().gap(gap)
}

fn field(label: &str, control: View) -> View {
    stack(6).grow().child(text(label, 12, tone::MUTED).class("rt-r-weight-medium")).child(control)
}

fn callout(detail: &str, background: &str, color: &str) -> View {
    View::new(Kind::Div, None).padding(12).radius(10).bg(background).child(text(detail, 13, color))
}

fn select(event: &str, current: &str, options: &[(&str, &str)]) -> View {
    let mut s = View::new(Kind::Select, None).width_full().prop("size", "2").prop("default-value", current).prop("key", event).on(On::Change, event);
    for (value, label) in options {
        s = s.child(View::new(Kind::Option, Some(label)).prop("value", value));
    }
    s
}

pub fn page(s: &UiState) -> View {
    let current = match s.tab {
        Tab::Import => "import",
        Tab::Watch => "watch",
        Tab::Face => "face",
    };
    let trigger = |value: &str, label: &str| View::new(Kind::TabsTrigger, Some(label)).prop("value", value).on(On::Click, &format!("{EV_TAB}{value}"));
    let body = match s.tab {
        Tab::Import => import_tab(s),
        Tab::Watch => watch_tab(s),
        Tab::Face => face_tab(s),
    };
    stack(16)
        .width_full()
        .padding(16)
        .child(header(s))
        .maybe((!s.status.is_empty()).then(|| {
            if s.error { callout(&s.status, tone::RED_WELL, tone::RED) } else { callout(&s.status, tone::WELL, tone::TEXT) }
        }))
        .child(
            View::new(Kind::TabsRoot, None)
                .prop("value", current)
                .child(
                    View::new(Kind::TabsList, None)
                        .prop("size", "2")
                        .child(trigger("import", "导入角色"))
                        .child(trigger("watch", "手表上的角色"))
                        .child(trigger("face", "表盘图层")),
                )
                .child(View::new(Kind::TabsContent, None).prop("value", current).child(body.margin_top(16))),
        )
}

fn header(s: &UiState) -> View {
    let mut picker = View::new(Kind::Select, None)
        .width_full()
        .prop("size", "2")
        .prop("default-value", &s.selected_addr)
        .prop("key", EV_DEVICE)
        .on(On::Change, EV_DEVICE);
    for d in &s.devices {
        picker = picker.child(View::new(Kind::Option, Some(&d.name)).prop("value", &d.addr));
    }
    let status = match &s.hello {
        Some(h) => {
            let model = match h.target {
                bal2d_proto::target::BAND_10_PRO_043 => "小米手环 10 Pro",
                _ => "手表",
            };
            badge(&format!("{model} · {}×{} · {} 个角色", h.width, h.height, h.packs), "green")
        }
        None => badge("未连接模块", "gray"),
    };
    card()
        .child(row().child(heading("BA 记忆大厅").grow()).child(status).child(button("刷新", EV_REFRESH, "ghost")))
        .child(text(
            "把记忆大厅（Spine 骨骼 + 图集 + 贴图）编译成手表角色包：用你自己从游戏中提取的文件，或从在线角色库（第三方社区仓库）直接下载。插件不包含任何游戏素材；素材仅限个人使用，请勿分享。",
            13,
            tone::MUTED,
        ))
        .child(if s.devices.is_empty() {
            callout("没有已连接的手表。请先在 AstroBox 中连接手表，然后点右上角“刷新”。", tone::WELL, tone::MUTED)
        } else {
            row().child(field("手表", picker)).child(button("连接", EV_CONNECT, "soft"))
        })
}

fn import_tab(s: &UiState) -> View {
    let mut files = stack(6);
    for a in &s.assets {
        files = files.child(
            row()
                .child(badge(a.kind.label(), "blue"))
                .child(text(&a.name, 13, tone::TEXT).class("rt-truncate").grow())
                .child(text(&format!("{:.0} KB", a.data.len() as f64 / 1024.0), 12, tone::MUTED)),
        );
    }
    let assets_card = card()
        .child(row().child(heading("素材").grow()).child(button("清空", EV_CLEAR, "ghost").disabled_if(s.assets.is_empty())))
        .child(text(
            "逐个添加 .skel、.atlas 和全部 .png（或一个包含它们的 zip）。有背景层的角色请把背景骨骼一起加进来；可选再加入社区整理的 interactions.json 以获得原版的摸头参数。",
            13,
            tone::MUTED,
        ))
        .maybe((!s.assets.is_empty()).then(|| files))
        .child(row().child(button("添加文件", EV_ADD, "solid")));
    let can_build = !s.assets.is_empty() && s.hello.is_some() && !s.busy && s.transfer.is_none();
    let settings = card()
        .child(heading("编译设置"))
        .child(
            field(
                "名称",
                View::new(Kind::Input, None)
                    .width_full()
                    .prop("size", "2")
                    .prop("placeholder", "显示在手表与列表中的名称")
                    .prop("default-value", &s.name)
                    .prop("key", &format!("{EV_NAME}.{}", s.name_rev))
                    .on(On::Input, EV_NAME),
            ),
        )
        .child(
            row().child(field(
                    "画质",
                    select(
                        EV_QUALITY,
                        if s.high_quality { "high" } else { "standard" },
                        &[("standard", "标准（推荐）"), ("high", "高清（占用更多手表内存）")],
                    ),
                )),
        )
        .child(
            row().child(field(
                "待机动画",
                select(
                    EV_IDLE,
                    if s.idle_mode.is_empty() { "balanced" } else { &s.idle_mode },
                    &[
                        ("balanced", "预烘焙 · 均衡（推荐）"),
                        ("fine", "预烘焙 · 精细（手表绘制次数更多）"),
                        ("fast", "预烘焙 · 最流畅（细节略软）"),
                        ("off", "不预烘焙（角色包更小，手表逐帧计算）"),
                    ],
                ),
            )),
        )
        .child(
            row().child(field(
                "气泡文字",
                select(
                    EV_LANG,
                    if s.bubble_lang.is_empty() { "zh-hans" } else { &s.bubble_lang },
                    &[("zh-hans", "简体中文"), ("zh-hant", "繁體中文"), ("ja", "日本語"), ("en", "English"), ("off", "关闭（不显示对话）")],
                ),
            )),
        )
        .child(text(
            "对话时手表上显示角色的台词气泡。台词取自游戏数据表（electricgoat/ba-data），编译时联网下载；简体中文由繁体逐字转换（OpenCC 字表）。没有这种语言台词的新角色用日语。离线时照常编译，只是没有气泡。",
            12,
            tone::MUTED,
        ))
        .child(row().child(button(if s.busy { "正在编译…" } else { "编译并预览" }, EV_BUILD, "solid").disabled_if(!can_build)))
        .maybe(s.progress.as_ref().map(|(label, percent)| {
            stack(6)
                .child(text(&format!("{label} · {percent}%"), 13, tone::MUTED))
                .child(View::new(Kind::Progress, None).prop("value", &percent.to_string()).prop("max", "100").prop("size", "2"))
        }))
        .maybe(s.hello.is_none().then(|| text("连接手表后才能编译：角色包按手表的屏幕尺寸生成。", 12, tone::MUTED)));
    let mut out = stack(16).child(library_card(s)).child(assets_card).maybe((!s.assets.is_empty()).then(|| framing_card(s))).child(settings);
    if let Some(b) = &s.built {
        let mut c = card()
            .child(row().child(heading(&format!("预览 · {}", b.name)).grow()).child(badge(&format!("{}×{}", b.screen.0, b.screen.1), "gray")))
            .child(
                View::new(Kind::Div, None).row().justify_center().child(
                    View::new(Kind::Image, Some(&b.preview))
                        .width(b.screen.0 * 240 / b.screen.1.max(1))
                        .height(240)
                        .radius(12),
                ),
            )
            .child(text(&b.summary, 13, tone::MUTED));
        match &s.transfer {
            Some(t) => {
                let percent = if t.total > 0 { t.sent * 100 / t.total } else { 0 };
                c = c
                    .child(View::new(Kind::Progress, None).prop("value", &percent.to_string()).prop("max", "100").prop("size", "2"))
                    .child(
                        row()
                            .child(text(&format!("{:.0} / {:.0} KB · {:.1} KB/s", t.sent as f64 / 1024.0, t.total as f64 / 1024.0, t.speed as f64 / 1024.0), 12, tone::MUTED).grow())
                            .child(button("取消", EV_CANCEL, "soft")),
                    );
            }
            None => {
                let est = b.bytes.len() as f64 / 1024.0 / 10.0;
                c = c.child(
                    row()
                        .child(text(&format!("传输约需 {:.0}–{:.0} 秒", est / 3.0, est), 12, tone::MUTED).grow())
                        .child(button("安装到手表", EV_INSTALL, "solid").disabled_if(s.hello.is_none())),
                );
            }
        }
        out = out.child(c);
    }
    out
}

/// The online character library: route, list, search, one-click download.
fn library_card(s: &UiState) -> View {
    let l = &s.lib;
    let blocked = s.busy || l.loading || l.active.is_some() || s.transfer.is_some();
    let load_label = if l.loading {
        "正在载入…"
    } else if l.entries.is_empty() {
        "载入角色列表"
    } else {
        "刷新列表"
    };
    let route_value = if l.route.is_empty() { "auto" } else { l.route.as_str() };
    let mut c = card()
        .child(
            row()
                .child(heading("在线角色库").grow())
                .child(button(load_label, EV_LIB_LOAD, if l.entries.is_empty() { "solid" } else { "ghost" }).disabled_if(blocked)),
        )
        .child(text(
            "从第三方社区仓库在线下载记忆大厅：素材来自 asdfdsa12/BA-Spine-Viewer-Asset，中文名来自 SchaleDB，对话台词来自 electricgoat/ba-data。插件本身不包含、也不托管任何素材；仅限个人使用，请勿分享。",
            13,
            tone::MUTED,
        ))
        .child(
            row().child(field(
                "下载线路",
                select(
                    EV_LIB_ROUTE,
                    route_value,
                    &[("auto", "自动（jsDelivr → GitHub）"), ("jsdelivr", "jsDelivr"), ("github", "GitHub 直连"), ("custom", "自定义")],
                ),
            )),
        )
        .maybe((route_value == "custom").then(|| {
            field(
                "自定义前缀（加在 GitHub 原始链接前）",
                View::new(Kind::Input, None)
                    .width_full()
                    .prop("size", "2")
                    .prop("placeholder", "例如 https://ghfast.top/")
                    .prop("default-value", &l.custom_prefix)
                    .prop("key", EV_LIB_PREFIX)
                    .on(On::Input, EV_LIB_PREFIX),
            )
        }))
        .child(
            row()
                .child(
                    text(
                        if l.auto_install { "下载后直接安装到手表：开（编译完成后自动安装）" } else { "下载后直接安装到手表：关（只编译并预览）" },
                        13,
                        tone::TEXT,
                    )
                    .grow(),
                )
                .child(button(if l.auto_install { "关闭" } else { "开启" }, EV_LIB_AUTO, "soft")),
        );
    let progress = l.progress.as_ref().or(if l.active.is_some() { s.progress.as_ref() } else { None });
    if let Some((label, percent)) = progress {
        c = c
            .child(text(&format!("{label} · {percent}%"), 13, tone::MUTED))
            .child(View::new(Kind::Progress, None).prop("value", &percent.to_string()).prop("max", "100").prop("size", "2"));
    }
    if l.entries.is_empty() {
        return c;
    }
    let found: Vec<&library::LibEntry> = l.entries.iter().filter(|e| library::matches(e, &l.query)).collect();
    let count = if l.query.trim().is_empty() {
        format!("共 {} 个大厅", l.entries.len())
    } else {
        format!("找到 {} 个", found.len())
    };
    let count = if found.len() > LIB_ROWS { format!("{count}，显示前 {LIB_ROWS} 个（输入名字搜索其他）") } else { count };
    let r = route(s);
    let rows: Vec<View> = found
        .iter()
        .take(LIB_ROWS)
        .map(|e| {
            let active = l.active.as_deref() == Some(e.id.as_str());
            let sub = if e.extras.is_empty() { e.id.clone() } else { format!("{} · 含背景层", e.id) };
            View::new(Kind::Div, None)
                .row()
                .align_center()
                .gap(10)
                .padding(8)
                .radius(8)
                .bg(if active { tone::GREEN_WELL } else { tone::WELL })
                .child(View::new(Kind::Image, Some(&online::icon_url(&e.id, &r))).prop("alt", "").square(44).radius(8))
                .child(
                    stack(2)
                        .grow()
                        .child(
                            View::new(Kind::Div, None)
                                .row()
                                .align_center()
                                .gap(6)
                                .child(text(&e.name, 14, if active { tone::GREEN } else { tone::TEXT }).class("rt-r-weight-medium rt-truncate"))
                                .maybe(e.new.then(|| badge("新", "orange"))),
                        )
                        .child(text(&sub, 12, tone::MUTED).class("rt-truncate")),
                )
                .child(if active {
                    button("处理中…", &format!("{EV_LIB_GET}{}", e.id), "soft").disabled_if(true)
                } else {
                    row()
                        .child(button("下载并取景", &format!("{EV_LIB_FRAME}{}", e.id), "ghost").disabled_if(blocked))
                        .child(button("下载并导入", &format!("{EV_LIB_GET}{}", e.id), "soft").disabled_if(blocked))
                })
        })
        .collect();
    let height = (rows.len().max(1) as u32 * 68).min(420);
    c = c
        .child(
            View::new(Kind::Input, None)
                .width_full()
                .prop("size", "2")
                .prop("placeholder", "搜索：中文名、韩文名或 ID")
                .prop("default-value", &l.query)
                .prop("key", EV_LIB_QUERY)
                .on(On::Input, EV_LIB_QUERY),
        )
        .child(text(&count, 12, tone::MUTED));
    if rows.is_empty() {
        return c.child(text("没有匹配的大厅。", 13, tone::MUTED));
    }
    c.child(View::new(Kind::ScrollArea, None).prop("type", "auto").prop("scrollbars", "vertical").height(height).child(stack(8).children(rows)))
        .child(text("“下载并导入”按默认取景（头部居中）直接编译；想先调整画面范围，点“下载并取景”，调整好再编译安装。", 12, tone::MUTED))
        .maybe(s.hello.is_none().then(|| text("未连接手表时只下载素材；连接后点“编译并预览”。", 12, tone::MUTED)))
}

/// The framing editor: the scene with the watch-shaped box over it, per phase.
fn framing_card(s: &UiState) -> View {
    use bal2d_pack::phase;
    const ACCENT: &str = "var(--accent-9)";
    let (Some(o), Some(f)) = (&s.overview, s.framing) else {
        let can = !s.busy && s.hello.is_some() && s.transfer.is_none();
        return card()
            .child(heading("取景"))
            .child(text(
                "默认按头部居中取景。打开取景器可以看到整个场景，拖动方框决定手环屏幕上显示哪一部分；开场、对话、摸头还可以各用一个取景，手环上会在这些阶段之间平滑切换。",
                13,
                tone::MUTED,
            ))
            .child(row().child(button("打开取景器", EV_FRAME_OPEN, "soft").disabled_if(!can)));
    };
    let p = s.frame_phase;
    let mut tabs = row();
    for ph in [phase::IDLE, phase::INTRO, phase::TALK, phase::TOUCH] {
        let own = ph != phase::IDLE && f[ph as usize].is_some();
        let label = format!("{}{}", crate::framing::phase_name(ph), if own { " ·单独" } else { "" });
        tabs = tabs.child(
            button(&label, &format!("{EV_FRAME_PHASE}{ph}"), if ph == p { "solid" } else { "soft" }).disabled_if(o.frames[ph as usize].is_none()),
        );
    }
    let own = f[p as usize].is_some();
    let cam = f[p as usize].or(f[phase::IDLE as usize]).unwrap_or(o.default);
    let (x, y, w, h) = crate::framing::frame_rect(o, cam);
    let image = o.frames[p as usize].as_deref().or(o.frames[phase::IDLE as usize].as_deref()).unwrap_or("");
    let handle = View::new(Kind::Div, None)
        .absolute()
        .translate(w - 12.0, h - 12.0)
        .width(22)
        .height(22)
        .radius(11)
        .bg(ACCENT)
        .on(On::PointerDown, EV_FRAME_RESIZE);
    let frame = View::new(Kind::Div, None)
        .absolute()
        .translate(x, y)
        .width(w.max(1.0) as u32)
        .height(h.max(1.0) as u32)
        .border(2, ACCENT)
        .z_index(1)
        .on(On::PointerDown, EV_FRAME_MOVE)
        .child(handle);
    let scene = View::new(Kind::Div, None)
        .relative()
        .width(o.size.0)
        .height(o.size.1)
        .bg("#000")
        .on(On::PointerMove, EV_FRAME_DRAG)
        .on(On::PointerUp, EV_FRAME_UP)
        .on(On::MouseLeave, EV_FRAME_UP)
        .child(View::new(Kind::Image, Some(image)).absolute().width(o.size.0).height(o.size.1).opacity(0.85))
        .child(frame);
    let slider = |label: &str, event: &str, min: f32, max: f32, step: f32, v: f32| {
        field(
            label,
            View::new(Kind::Slider, None)
                .width_full()
                .prop("min", &format!("{min}"))
                .prop("max", &format!("{max}"))
                .prop("step", &format!("{step}"))
                .prop("value", &format!("{}", v.clamp(min, max).round()))
                .prop("key", &format!("{event}.{p}"))
                .on(On::Change, event),
        )
    };
    let zoom = cam.scale / o.default.scale * 100.0;
    let mut c = card()
        .child(row().child(heading("取景").grow()).child(button("恢复默认", EV_FRAME_RESET, "ghost")))
        .child(tabs);
    if p != phase::IDLE {
        c = c.child(
            row()
                .child(text(if own { "这一阶段使用单独的取景。" } else { "这一阶段沿用待机的取景（拖动方框即改为单独取景）。" }, 13, tone::MUTED).grow())
                .child(button(if own { "改为沿用待机" } else { "单独取景" }, EV_FRAME_OWN, "soft")),
        );
    }
    c.child(View::new(Kind::Div, None).row().justify_center().child(scene))
        .child(text("方框就是手环屏幕显示的范围：拖动方框移动，拖动右下角的圆点缩放；也可以用下面的滑块。", 12, tone::MUTED))
        .child(slider("缩放（%）", EV_FRAME_ZOOM, 20.0, 600.0, 5.0, zoom))
        .child(slider("左右", EV_FRAME_X, 0.0, o.size.0 as f32, 1.0, x + w * 0.5))
        .child(slider("上下", EV_FRAME_Y, 0.0, o.size.1 as f32, 1.0, y + h * 0.5))
}

fn watch_tab(s: &UiState) -> View {
    let Some(h) = &s.hello else {
        return card().child(heading("手表上的角色")).child(text("连接手表后显示。", 13, tone::MUTED));
    };
    let mut list = stack(8);
    if s.packs.is_empty() {
        list = list.child(text("手表上还没有角色。在“导入角色”中编译并安装一个。", 13, tone::MUTED));
    }
    for p in &s.packs {
        let active = p.id == h.active;
        list = list.child(
            row()
                .padding(8)
                .radius(8)
                .bg(if active { tone::GREEN_WELL } else { tone::WELL })
                .child(
                    stack(2)
                        .grow()
                        .child(text(&p.name, 14, if active { tone::GREEN } else { tone::TEXT }).class("rt-r-weight-medium"))
                        .child(text(&format!("{:.0} KB{}", p.size as f64 / 1024.0, if active { " · 正在显示" } else { "" }), 12, tone::MUTED)),
                )
                .maybe((!active).then(|| button("显示", &format!("{EV_SELECT}{}", p.id), "soft")))
                .child(button("删除", &format!("{EV_DELETE}{}", p.id), "ghost")),
        );
    }
    let load_error = s.stats.as_ref().map(|st| (st.flags >> 8) & 0xff).unwrap_or(0);
    let perf = match &s.stats {
        Some(_) if load_error != 0 => match load_error {
            2 => "手表上还没有角色：先在“导入角色”页编译并安装一个。".into(),
            12 => "手表内存不足，角色没有加载：请用“标准”画质重新编译，或换一个较小的大厅。".into(),
            5 | 22 => "手表上的角色包无法读取：请删除后重新安装。".into(),
            e => format!("角色加载失败（错误 {e}）。"),
        },
        Some(st) if st.flags & 8 != 0 => "正在加载角色…".into(),
        Some(st) if st.frames > 0 && st.fps_x10 > 0 => format!(
            "{}.{} fps（{}）· 动画 {} ms · 绘制 {} ms（其中驱动 {} ms、等 GPU {} ms）· GPU 收尾 {} ms{}{} · {} 个三角形 · {} 次绘制{} · 手表剩余内存 {:.1} MB{}{}",
            st.fps_x10 / 10,
            st.fps_x10 % 10,
            match (st.flags & 32 != 0, st.flags & 64 != 0, st.flags & 128 != 0) {
                (_, _, true) => "缓冲绘制：直接绘制在这块手表上自检未通过",
                (true, true, _) => "直接绘制，已读回校验",
                (true, false, _) => "直接绘制",
                _ => "缓冲绘制",
            },
            st.update_ms,
            st.draw_ms,
            st.driver_ms,
            st.flush_ms,
            st.gpu_ms,
            if st.ui_ms > 0 { format!(" · 屏幕刷新 {} ms", st.ui_ms) } else { String::new() },
            if st.idle_ms + st.late_ms > 0 { format!(" · 空闲 {} ms · 迟到 {} ms", st.idle_ms, st.late_ms) } else { String::new() },
            st.triangles,
            st.calls,
            if st.flags & 16 != 0 {
                " · 待机已预烘焙"
            } else if st.flags & 1 != 0 {
                " · 背景已烘焙"
            } else {
                ""
            },
            st.heap_free_kib as f64 / 1024.0,
            selftest_note(st.flags, st.selftest),
            if st.gpu_resets > 0 {
                format!(" · GPU 超时复位 {} 次（驱动等 GPU 超过 100 ms 会复位 GPU，这一批绘制丢失，画面会缺一块）", st.gpu_resets)
            } else {
                String::new()
            }
        ),
        Some(st) if st.flags & 2 == 0 => "当前没有显示：这个表盘没有角色图层（在“表盘图层”页给表盘加入）。".into(),
        Some(_) => "角色暂停中（熄屏或不在表盘上）。".into(),
        None => "读取中…".into(),
    };
    let fluent = h.options & bal2d_proto::options::FLUENT != 0;
    let direct = h.options & bal2d_proto::options::CLASSIC == 0;
    let batched = h.options & bal2d_proto::options::SINGLE == 0;
    stack(16)
        .child(card().child(row().child(heading("手表上的角色").grow()).child(button("刷新", EV_CONNECT, "ghost"))).child(list))
        .child(
            card()
                .child(row().child(heading("手表上的性能").grow()).child(button("导出手表日志", EV_LOG, "ghost")))
                .child(text(&perf, 13, tone::MUTED))
                .child(
                    row()
                        .child(
                            text(
                                if fluent {
                                    "流畅优先：开。每帧只留约十二分之一的时间给手表系统，动画更快（默认）；如果蓝牙连接变慢、断开或手表操作卡顿，请关闭。"
                                } else {
                                    "流畅优先：关。每帧留约六分之一的时间给手表系统和蓝牙。"
                                },
                                13,
                                tone::TEXT,
                            )
                            .grow(),
                        )
                        .child(button(if fluent { "关闭" } else { "开启" }, EV_FLUENT, "soft")),
                )
                .child(
                    row()
                        .child(
                            text(
                                if direct {
                                    "直接绘制：开。角色直接画进系统的屏幕缓冲区（片上内存），省去一次整屏拷贝（默认）。"
                                } else {
                                    "直接绘制：关。角色先画进模块自己的缓冲区，再由系统拷到屏幕（旧方式，用于对比）。"
                                },
                                13,
                                tone::TEXT,
                            )
                            .grow(),
                        )
                        .child(button(if direct { "关闭" } else { "开启" }, EV_DIRECT, "soft")),
                )
                .child(
                    row()
                        .child(
                            text(
                                if batched {
                                    "批量绘制：开。同一张贴图的一串绘制只调用一次 GPU 驱动（默认）。"
                                } else {
                                    "批量绘制：关。每次绘制单独调用 GPU 驱动（旧方式，用于对比）。"
                                },
                                13,
                                tone::TEXT,
                            )
                            .grow(),
                        )
                        .child(button(if batched { "关闭" } else { "开启" }, EV_SINGLE, "soft")),
                ),
        )
}

fn face_tab(s: &UiState) -> View {
    let f = &s.face;
    let mut c = card()
        .child(heading("给表盘加入角色图层"))
        .child(text(
            "选择一个成品表盘（.bin 或 .mwz），指定角色图层插在第几层：图层越靠下，越多表盘内容叠在角色上面。生成的新表盘安装到手表后，角色就会出现在这一层。",
            13,
            tone::MUTED,
        ))
        .child(row().child(button("选择表盘文件", EV_FACE_PICK, "solid")));
    if f.data.is_empty() {
        return c.child(text(
            "自制 Lua 表盘：创建一个显示 /data/canopus/m.ba_l2d/layer.bin 的 lvgl.Image，它就是角色图层（不设尺寸时全屏），无需在这里生成。",
            12,
            tone::MUTED,
        ));
    }
    c = c.child(text(&format!("{} · {} 层", f.name, f.layers.len()), 14, tone::TEXT).class("rt-r-weight-medium"));
    if f.has_marker {
        return c.child(callout("这个表盘已经有角色图层了，可以直接安装。", tone::GREEN_WELL, tone::GREEN));
    }
    let mut options: Vec<(String, String)> = (0..f.layers.len())
        .map(|z| (z.to_string(), format!("第 {z} 层之下：{}", f.layers[z])))
        .collect();
    options.push((f.layers.len().to_string(), "最上层（覆盖整个表盘）".into()));
    let opts: Vec<(&str, &str)> = options.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    c = c
        .child(field("角色图层的位置", select(EV_FACE_Z, &f.z.to_string(), &opts)))
        .child(text("一般选第 1 层之下：角色在表盘背景之上、其他内容之下。", 12, tone::MUTED))
        .child(row().child(button("生成表盘", EV_FACE_PATCH, "solid")));
    if s.hello.is_none() {
        c = c.child(text("未连接手表时按小米手环 10 Pro（336×480）生成。", 12, tone::MUTED));
    }
    if let Some((file, bin)) = &f.patched {
        c = c.child(callout(&format!("已生成 {file}（{:.0} KB）", bin.len() as f64 / 1024.0), tone::GREEN_WELL, tone::GREEN)).child(
            row().child(button("加入安装队列", EV_FACE_QUEUE, "solid")).child(button("保存到手机", EV_FACE_SAVE, "soft")),
        );
    }
    c
}

/// What the watch's GPU self-test changed (Stats flags bits 16..23), for the performance card.
/// The GPU self-test probes (module selftest.rs `probe`), bit by bit.
const PROBES: [&str; 20] = [
    "FP32 路径",
    "S16 路径",
    "大贴图",
    "ARGB4444 贴图",
    "ARGB8565 贴图",
    "索引色贴图",
    "PAD 边缘",
    "调色",
    "叠加混合",
    "正片叠底",
    "滤色混合",
    "半透明",
    "整页贴图",
    "小贴图直写",
    "大贴图直写",
    "越界检查",
    "区域贴图",
    "批量绘制 1",
    "批量绘制 2",
    "右端 S16 路径",
];

fn selftest_note(flags: u32, passed: u32) -> String {
    let base = selftest_flags_note(flags);
    if passed == 0 || flags & (0xFF << 17) == 0 {
        return base;
    }
    let failed: Vec<&str> = PROBES.iter().enumerate().filter(|(i, _)| passed & (1 << i) == 0).map(|(_, n)| *n).collect();
    if failed.is_empty() {
        return base;
    }
    format!("{base}（未通过：{}；0x{passed:05x}）", failed.join("、"))
}

fn selftest_flags_note(flags: u32) -> String {
    if flags & (1 << 16) == 0 {
        return String::new();
    }
    let notes: Vec<&str> = [
        (17, "自检未能运行"),
        (18, "S16 路径异常，已改用 FP32"),
        (19, "缓存回写异常，已改为直写"),
        (20, "部分纹理格式异常，已改用 32 位"),
        (21, "边缘填充异常，已改用透明边缘"),
        (22, "混合模式结果有偏差"),
        (23, "绘制结果异常"),
        (24, "批量绘制异常，已改为逐次绘制"),
    ]
    .iter()
    .filter(|(bit, _)| flags & (1 << bit) != 0)
    .map(|(_, t)| *t)
    .collect();
    if notes.is_empty() {
        " · GPU 自检通过".into()
    } else {
        format!(" · GPU 自检：{}", notes.join("；"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(v: &serde_json::Value, out: &mut Vec<String>) {
        for l in v["event_listeners"].as_array().into_iter().flatten() {
            out.push(l["id"].as_str().unwrap_or("").to_string());
        }
        for c in v["children"].as_array().into_iter().flatten() {
            events(c, out);
        }
    }

    fn page_events(s: &UiState) -> (Vec<String>, String) {
        let json = page(s).to_json(&mut 0);
        let mut out = Vec::new();
        events(&json, &mut out);
        (out, json.to_string())
    }

    #[test]
    fn the_library_lists_a_limited_searchable_set_of_rows() {
        let mut s = UiState::default();
        let (ev, _) = page_events(&s);
        assert!(ev.iter().any(|e| e == EV_LIB_LOAD) && ev.iter().any(|e| e == EV_LANG));
        assert!(!ev.iter().any(|e| e.starts_with(EV_LIB_GET)));
        s.lib.entries = (0..80)
            .map(|i| library::LibEntry { id: format!("CH{i:04}_home"), name: format!("学生{i}"), kr: format!("학생{i}"), new: i < 3, ..Default::default() })
            .collect();
        s.lib.entries.push(library::LibEntry { id: "Hoshino_home".into(), name: "星野".into(), character: Some(10005), extras: vec!["Hoshino_home_background".into()], ..Default::default() });
        let (ev, json) = page_events(&s);
        assert_eq!(ev.iter().filter(|e| e.starts_with(EV_LIB_GET)).count(), LIB_ROWS);
        assert_eq!(ev.iter().filter(|e| e.starts_with(EV_LIB_FRAME)).count(), LIB_ROWS);
        assert!(json.contains("SCROLL_AREA") && json.contains("Lobbyillust_Icon_CH0000_01.png") && json.contains("共 81 个大厅"));
        s.lib.query = "星野".into();
        let (ev, json) = page_events(&s);
        assert_eq!(ev.iter().filter(|e| e.starts_with(EV_LIB_GET)).collect::<Vec<_>>(), [&format!("{EV_LIB_GET}Hoshino_home")]);
        assert_eq!(ev.iter().filter(|e| e.starts_with(EV_LIB_FRAME)).collect::<Vec<_>>(), [&format!("{EV_LIB_FRAME}Hoshino_home")]);
        assert!(json.contains("含背景层"));
        // The custom prefix field shows only for the custom route.
        assert!(!ev.iter().any(|e| e == EV_LIB_PREFIX));
        s.lib.route = "custom".into();
        assert!(page_events(&s).0.iter().any(|e| e == EV_LIB_PREFIX));
    }
}

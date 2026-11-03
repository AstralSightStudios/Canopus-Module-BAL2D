//! The plugin page: import (pick files, compile, preview, install), the packs on the watch, and
//! the watchface layer tool.

use serde::Deserialize;

use crate::astrobox::psys_host_v4::{
    dialog::{self, DialogButton, DialogInfo, DialogStyle, DialogType, FilterConfig, PickConfig},
    queue, ui,
};
use crate::state::{self, Tab, UiState};
use crate::view::{Kind, On, View};
use crate::{assets, face, watch};

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
const EV_OVERLAY: &str = "action:overlay";
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
        EV_OVERLAY => {
            let current = state::snapshot().hello.map(|h| h.options).unwrap_or(0);
            watch::set_options(current ^ bal2d_proto::options::OVERLAY).await;
        }
        EV_FLUENT => {
            let current = state::snapshot().hello.map(|h| h.options).unwrap_or(0);
            watch::set_options(current ^ bal2d_proto::options::FLUENT).await;
        }
        EV_DIRECT => {
            let current = state::snapshot().hello.map(|h| h.options).unwrap_or(0);
            watch::set_options(current ^ bal2d_proto::options::CLASSIC).await;
        }
        EV_SINGLE => {
            let current = state::snapshot().hello.map(|h| h.options).unwrap_or(0);
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
        s.assets = assets;
        s.built = None;
        s.overview = None;
        s.framing = None;
        if s.name.is_empty() {
            s.name = suggested;
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
    let req = assets::BuildRequest {
        assets: s.assets.clone(),
        name: s.name.clone(),
        screen: size,
        zoom: s.zoom,
        high_quality: s.high_quality,
        idle_mode: s.idle_mode.clone(),
        framing: s.framing,
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
            "把你自己从游戏中提取的记忆大厅（Spine 骨骼 + 图集 + 贴图）编译成手表角色包。插件不包含任何游戏素材；素材仅限个人使用，请勿分享。",
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
                    .prop("key", EV_NAME)
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
        .child(row().child(button(if s.busy { "正在编译…" } else { "编译并预览" }, EV_BUILD, "solid").disabled_if(!can_build)))
        .maybe(s.progress.as_ref().map(|(label, percent)| {
            stack(6)
                .child(text(&format!("{label} · {percent}%"), 13, tone::MUTED))
                .child(View::new(Kind::Progress, None).prop("value", &percent.to_string()).prop("max", "100").prop("size", "2"))
        }))
        .maybe(s.hello.is_none().then(|| text("连接手表后才能编译：角色包按手表的屏幕尺寸生成。", 12, tone::MUTED)));
    let mut out = stack(16).child(assets_card).maybe((!s.assets.is_empty()).then(|| framing_card(s))).child(settings);
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
            "{}.{} fps（{}）· 动画 {} ms · 绘制 {} ms（其中驱动 {} ms、等 GPU {} ms）· GPU 收尾 {} ms{}{} · {} 个三角形 · {} 次绘制{} · 手表剩余内存 {:.1} MB{}",
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
            selftest_note(st.flags, st.selftest)
        ),
        Some(st) if st.flags & 2 == 0 => "当前没有显示：表盘没有角色图层且未开启全屏显示。".into(),
        Some(_) => "角色暂停中（熄屏或不在表盘上）。".into(),
        None => "读取中…".into(),
    };
    let overlay = h.options & bal2d_proto::options::OVERLAY != 0;
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
                                    "流畅优先：开。每帧只留约十二分之一的时间给手表系统，动画更快；如果蓝牙连接变慢、断开或手表操作卡顿，请关闭。"
                                } else {
                                    "流畅优先：关。每帧留约六分之一的时间给手表系统和蓝牙（默认）。"
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
        .child(
        card()
            .child(heading("显示方式"))
            .child(text(
                "加入了角色图层的表盘会在图层位置显示角色，表盘上的时间等组件照常叠在上面。没有角色图层的表盘：",
                13,
                tone::MUTED,
            ))
            .child(
                row()
                    .child(text(if overlay { "全屏显示在表盘最上层（覆盖表盘内容）" } else { "不显示角色" }, 14, tone::TEXT).grow())
                    .child(button(if overlay { "改为不显示" } else { "改为全屏显示" }, EV_OVERLAY, "soft")),
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

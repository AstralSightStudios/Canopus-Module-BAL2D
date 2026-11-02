//! Device runtime: one LVGL timer poses the lobby and draws it with the GPU into the render
//! target shown by the view. Everything runs on the UI owner task.

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use bal2d_core::{Lobby, Touch};
use bal2d_render::DrawList;
use canopus_target_private::*;

pub mod app;
pub mod connect;
pub mod fw;
pub mod gpu;
pub mod host;
pub mod lv;
pub mod selftest;
pub mod view;

use host::Fs;

/// About 30 frames per second while visible.
const FRAME_MS: u32 = 33;
pub const PACK_DIR: &str = "packs";
/// The file name of the pack to show (in `PACK_DIR`).
pub const CURRENT_FILE: &str = "packs/current.txt";
/// `bal2d_proto::options` bits, one byte.
pub const OPTIONS_FILE: &str = "options.bin";
/// Development: show on the top layer when this file exists in the module directory (the same
/// as `options::OVERLAY`).
const OVERLAY_FLAG: &str = "overlay.flag";

/// Set by the Connect endpoint: the pack to show changed.
static RELOAD: AtomicBool = AtomicBool::new(false);
/// Set by the Connect endpoint: the options changed.
static OPTIONS_DIRTY: AtomicBool = AtomicBool::new(true);
/// The phone link as the Supervisor reports it (`canopus/bt/state`): 1 up, 0 down, 0xFF unknown.
static BT_LINK: AtomicU8 = AtomicU8::new(0xFF);
/// Without a settled phone link, drawing pauses this long every `BREATHER_EVERY_MS`: the phone
/// may page the watch at any moment, and answering and authenticating take the UI task several
/// turns; long gaps now and then leave it room beyond the gaps between frames. A link counts as
/// settled 10 s after it came up. (Mirage's failures to connect under a heavy lobby turned out to
/// be the emulator's own lock starving its Bluetooth socket, fixed there.)
const BREATHER_MS: u32 = 250;
const BREATHER_EVERY_MS: u32 = 2000;
const LINK_SETTLE_MS: u32 = 10_000;

unsafe extern "C" fn on_bt_state(_ctx: *const canopus_abi::ContextV1, _topic: *const core::ffi::c_char, payload: *const c_void, len: u32, _user: *mut c_void) {
    if len >= 2 && !payload.is_null() {
        BT_LINK.store(unsafe { *payload.cast::<u8>().add(1) }, Ordering::Relaxed);
    }
}

pub fn request_reload() {
    RELOAD.store(true, Ordering::Release);
}

pub fn options_changed() {
    OPTIONS_DIRTY.store(true, Ordering::Release);
}

/// The persisted options byte.
pub fn options() -> u8 {
    Fs::read(OPTIONS_FILE, 4).and_then(|b| b.first().copied()).unwrap_or(0)
}
/// Development: hold the screen on (recordings) when this file exists.
const KEEPAWAKE_FLAG: &str = "keepawake.flag";

#[derive(Default, Clone, Copy)]
pub struct Perf {
    pub frames: u32,
    pub update_ms: u32,
    pub draw_ms: u32,
    pub gpu_ms: u32,
    /// Part of `draw_ms` spent inside the driver's draw calls.
    pub submit_ms: u32,
    pub triangles: u32,
    pub calls: u32,
    pub errors: u32,
    pub last_error: i32,
    pub fps_x10: u32,
}

pub struct Runtime {
    pub lobby: Option<Lobby>,
    pages: Vec<Vec<gpu::Page>>,
    /// The pack's baked static bottom, if it has one.
    bake: Option<gpu::BakeImage>,
    target: Option<gpu::Target>,
    renderer: gpu::Renderer,
    /// The GPU self-test's outcome (run on the first frame).
    selftest: Option<selftest::Outcome>,
    list: DrawList,
    pub timer: *mut c_void,
    pub screen: (i32, i32),
    pub boot_error: i32,
    pub pack: String,
    last_ms: u32,
    pub perf: Perf,
    window: Perf,
    window_start: u32,
    attach_check: u32,
    pressed: bool,
    load_tried: bool,
    /// The pack being loaded, one step per frame.
    loader: Option<Loader>,
    /// Show full screen above watchfaces without a BAL2D layer.
    overlay: bool,
    /// What `connect::start` answered (0: the phone can send packs).
    pub connect: i32,
    /// The last frame drew the baked background.
    pub bake_active: bool,
    /// No frame starts before this (duty-cycle limit).
    idle_until: u32,
    /// The last drawn frame: when it started and ended; `settling` until the first timer call
    /// after it, which measures the UI work it caused (LVGL redrawing the screen).
    frame_start: u32,
    frame_end: u32,
    settling: bool,
    /// The UI work after a frame (ms, smoothed).
    pub refresh_ms: u32,
    /// When the app page's status was last rewritten.
    status_at: u32,
    /// The phone link at the last frame, when it last came up, and the last breather (ms).
    bt_was: u8,
    bt_up_at: u32,
    breather_at: u32,
    /// A frame's whole cost, its own plus the UI work it causes (ms, smoothed), and when the
    /// clustering tolerance last changed (see `adapt_quality`).
    busy_ms: u32,
    quality_changed: u32,
}

static mut RUNTIME: Option<Runtime> = None;

pub fn rt() -> &'static mut Runtime {
    unsafe {
        let slot = &mut *core::ptr::addr_of_mut!(RUNTIME);
        if slot.is_none() {
            *slot = Some(Runtime {
                lobby: None,
                pages: Vec::new(),
                bake: None,
                target: None,
                renderer: gpu::Renderer::new(),
                selftest: None,
                list: DrawList::default(),
                timer: core::ptr::null_mut(),
                screen: (0, 0),
                boot_error: 0,
                pack: String::new(),
                last_ms: 0,
                perf: Perf::default(),
                window: Perf::default(),
                window_start: 0,
                attach_check: 0,
                load_tried: false,
                loader: None,
                overlay: false,
                connect: 1,
                bake_active: false,
                idle_until: 0,
                frame_start: 0,
                frame_end: 0,
                settling: false,
                refresh_ms: 0,
                busy_ms: 0,
                status_at: 0,
                bt_was: 0xFF,
                bt_up_at: 0,
                breather_at: 0,
                quality_changed: 0,
                pressed: false,
            });
        }
        slot.as_mut().unwrap()
    }
}

static mut TRACE_LEFT: u32 = 200;

/// Diagnostic line in `log.txt` (bounded per boot).
pub fn trace(line: &str) {
    unsafe {
        if TRACE_LEFT == 0 {
            return;
        }
        TRACE_LEFT -= 1;
    }
    let mut text = alloc::format!("[{}] {line}\n", lv::now_ms()).into_bytes();
    text.truncate(300);
    Fs::append("log.txt", &text);
}

/// What `STATS` reports.
pub fn stats() -> bal2d_proto::Stats {
    let r = rt();
    let v = view::view();
    let p = r.perf;
    bal2d_proto::Stats {
        frames: p.frames,
        fps_x10: p.fps_x10,
        update_ms: p.update_ms,
        draw_ms: p.draw_ms,
        driver_ms: p.submit_ms,
        gpu_ms: p.gpu_ms,
        triangles: p.triangles,
        calls: p.calls,
        errors: p.errors,
        heap_free_kib: heap_free_kb(),
        pack: r.pack.as_str().split('/').next_back().map(connect::file_id).filter(|_| !r.pack.is_empty()).unwrap_or(0),
        flags: r.bake_active as u32
            | (v.attached() as u32) << 1
            | (v.visible() as u32) << 2
            | (r.loader.is_some() as u32) << 3
            | (r.boot_error.unsigned_abs().min(255)) << 8
            | r.selftest.map(|t| t.flags).unwrap_or(0),
        ui_ms: r.refresh_ms,
    }
}

pub fn status() -> [u32; 8] {
    let r = unsafe { (*core::ptr::addr_of!(RUNTIME)).as_ref() };
    match r {
        Some(r) => [
            r.perf.frames,
            r.boot_error as u32,
            r.perf.fps_x10,
            r.perf.update_ms,
            r.perf.draw_ms,
            r.perf.gpu_ms,
            r.perf.triangles,
            r.perf.errors,
        ],
        None => [0; 8],
    }
}

/// The shown character's name for the app page.
pub fn pack_title() -> String {
    match rt().lobby.as_ref() {
        Some(l) if !l.name.is_empty() => l.name.clone(),
        Some(_) => rt().pack.clone(),
        None if pick_pack().is_some() => "正在加载…".into(),
        None => "还没有角色".into(),
    }
}

pub fn overlay_on() -> bool {
    options() & bal2d_proto::options::OVERLAY != 0
}

/// Shows the next installed pack (app page button).
pub fn next_pack() {
    let files = connect::pack_files();
    if files.is_empty() {
        return;
    }
    let current = pick_pack();
    let at = current.and_then(|c| files.iter().position(|f| *f == c));
    let next = &files[at.map_or(0, |i| (i + 1) % files.len())];
    if Fs::write(CURRENT_FILE, next.as_bytes()) {
        request_reload();
    }
}

/// Flips "full screen on watchfaces without a BAL2D layer" (app page button).
pub fn toggle_overlay() {
    let o = options() ^ bal2d_proto::options::OVERLAY;
    if Fs::write(OPTIONS_FILE, &[o]) {
        options_changed();
    }
}

pub fn status_text() -> String {
    let r = rt();
    let v = view::view();
    let mut text = String::from(match v.mode {
        view::Mode::Marker => "显示在表盘图层\n",
        view::Mode::Overlay => "全屏显示\n",
        view::Mode::None => "当前表盘没有角色图层\n",
    });
    if r.lobby.is_some() && r.perf.frames > 0 {
        let quality = match TOLERANCES.iter().position(|t| *t >= r.renderer.paths.tolerance).unwrap_or(0) {
            0 => "精细",
            1 => "均衡",
            _ => "流畅优先",
        };
        text += &alloc::format!(
            "{}.{} fps · 画质{}\n动画 {} ms · 绘制 {} ms\nGPU {} ms{}\n{} 三角形 · {} 次绘制\n",
            r.perf.fps_x10 / 10,
            r.perf.fps_x10 % 10,
            quality,
            r.perf.update_ms,
            r.perf.draw_ms,
            r.perf.gpu_ms,
            if r.bake_active { " · 背景已烘焙" } else { "" },
            r.perf.triangles,
            r.perf.calls,
        );
    }
    if r.loader.is_some() {
        text += "正在加载角色…\n";
    }
    if let Some(t) = r.selftest {
        text += &selftest_text(t.flags);
    }
    match r.boot_error {
        0 => {}
        -2 => text += "手表上还没有角色\n",
        -22 | -5 => text += "角色包无法读取，请在手机上重新安装\n",
        -12 => text += "内存不足：请用“标准”画质重新编译，或关闭其他占内存的应用\n",
        e => text += &alloc::format!("加载失败（{e}）\n"),
    }
    if r.perf.errors != 0 {
        text += &alloc::format!("GPU 错误 {} 次（{}）\n", r.perf.errors, r.perf.last_error);
    }
    if r.connect != 0 {
        text += "此固件不支持从手机导入\n";
    }
    if r.boot_error == -2 || v.mode == view::Mode::None {
        text += "在手机 AstroBox 的「BA 记忆大厅导入器」插件中导入角色，并给表盘加入角色图层（或打开全屏显示）。\n";
    }
    text
}

/// What the GPU self-test changed or could not check (empty when everything passed).
pub fn selftest_text(flags: u32) -> String {
    use selftest::flag;
    let mut t = String::new();
    if flags & flag::UNVERIFIED != 0 {
        t += "GPU 自检未能运行\n";
    }
    if flags & flag::FP32_PATHS != 0 {
        t += "GPU 自检：S16 路径异常，已改用 FP32\n";
    }
    if flags & flag::MIRROR != 0 {
        t += "GPU 自检：缓存回写异常，已改为直写\n";
    }
    if flags & flag::FORMATS != 0 {
        t += "GPU 自检：部分纹理格式异常，已改用 32 位\n";
    }
    if flags & flag::NO_PAD != 0 {
        t += "GPU 自检：边缘填充异常，已改用透明边缘\n";
    }
    if flags & flag::BLENDS != 0 {
        t += "GPU 自检：混合模式结果有偏差\n";
    }
    if flags & flag::OTHER != 0 {
        t += "GPU 自检：绘制结果异常\n";
    }
    t
}

/// Free heap the module leaves to the rest of the system.
pub const HEAP_RESERVE: u32 = 512 * 1024;

/// Whether `size` bytes can be allocated without the heap dropping under the reserve.
pub fn heap_allows(size: usize) -> bool {
    let (free, largest) = fw::heap_info();
    (size as u32) < largest && free.saturating_sub(size as u32) >= HEAP_RESERVE
}

/// Largest free block in KB (for the log).
fn largest_block_kb() -> u32 {
    let (free, largest) = fw::heap_info();
    let _ = free;
    largest / 1024
}

fn heap_free_kb() -> u32 {
    fw::heap_info().0 / 1024
}

/// The pack file as a `Source`.
pub struct FileSource {
    pub file: host::File,
}

impl bal2d_pack::Source for FileSource {
    fn read_at(&mut self, offset: u32, out: &mut [u8]) -> bool {
        self.file.seek(offset) && self.file.read_exact(out)
    }
}

fn screen_size() -> (i32, i32) {
    let ctx = crate::module::context();
    let (w, h) = canopus_runtime::display::get_resolution(ctx);
    if w > 0 && h > 0 {
        (w as i32, h as i32)
    } else {
        (336, 480)
    }
}

/// Picks the pack: `packs/current.txt` names it, else the first `.bal2d` in `packs/`.
pub fn pick_pack() -> Option<String> {
    if let Some(name) = Fs::read(CURRENT_FILE, 256) {
        let name = String::from_utf8_lossy(&name).trim().into();
        if Fs::stat(&alloc::format!("{PACK_DIR}/{name}")).is_some() {
            return Some(name);
        }
    }
    Fs::list(PACK_DIR).into_iter().filter(|n| n.ends_with(".bal2d") && !n.starts_with("stage")).min()
}

/// A pack being loaded one step per frame (skeletons, then each texture page, then the baked
/// background), so the watch's own UI runs between the steps instead of freezing for the whole
/// load.
struct Loader {
    name: String,
    src: FileSource,
    t0: u32,
    lobby: Option<Lobby>,
    pages: Vec<bal2d_core::Page>,
    per_layer: Vec<Vec<gpu::Page>>,
    next: usize,
}

/// One loading step: `None` while more remain, else the result (0: the pack is shown).
fn load_step() -> Option<i32> {
    let r = rt();
    let Some(l) = r.loader.as_mut() else {
        let Some(name) = pick_pack() else { return Some(-2) };
        trace(&alloc::format!("loading {name}; heap free {} KB largest {} KB", heap_free_kb(), largest_block_kb()));
        let Some(file) = Fs::open(&alloc::format!("{PACK_DIR}/{name}"), canopus_abi::FS_READ) else { return Some(-5) };
        r.loader = Some(Loader { name, src: FileSource { file }, t0: lv::now_ms(), lobby: None, pages: Vec::new(), per_layer: Vec::new(), next: 0 });
        return None;
    };
    let Some(lobby) = l.lobby.as_ref() else {
        match Lobby::load_from(&mut l.src) {
            Ok((lobby, pages)) => {
                trace(&alloc::format!("parsed in {} ms; {} pages; heap free {} KB largest {} KB", lv::now_ms().wrapping_sub(l.t0), pages.len(), heap_free_kb(), largest_block_kb()));
                l.per_layer = (0..lobby.layers.len()).map(|_| Vec::new()).collect();
                l.lobby = Some(lobby);
                l.pages = pages;
                return None;
            }
            Err(e) => {
                trace(&alloc::format!("load {}: {e:?}", l.name));
                return Some(-22);
            }
        }
    };
    if let Some(p) = l.pages.get(l.next) {
        let up = gpu::Page::from_pack(&mut l.src, p);
        trace(&alloc::format!("page {}/{} {}x{} {:?}: {}; heap free {} KB", p.layer, p.index, p.header.width, p.header.height, p.header.format, up.as_ref().err().copied().unwrap_or(0), heap_free_kb()));
        match up {
            Ok(page) => l.per_layer[p.layer as usize].push(page),
            Err(e) => return Some(e),
        }
        l.next += 1;
        return None;
    }
    let bake = match &lobby.bake {
        Some(b) => match gpu::BakeImage::from_pack(&mut l.src, b) {
            Ok(img) => {
                trace(&alloc::format!("bake {}x{} at ({}, {}){}; heap free {} KB", b.header.width, b.header.height, b.header.x, b.header.y, if b.after_intro { " after the intro" } else { "" }, heap_free_kb()));
                Some(img)
            }
            Err(e) => return Some(e),
        },
        None => None,
    };
    trace(&alloc::format!(
        "loaded {}: {} layers, {} pages, {} ms",
        l.name,
        lobby.layers.len(),
        l.per_layer.iter().map(|p| p.len()).sum::<usize>(),
        lv::now_ms().wrapping_sub(l.t0)
    ));
    // Working buffers at their final size: the frame loop must not grow anything large.
    let (mut verts, mut idx, mut max_tris) = (0usize, 0usize, 0usize);
    for layer in &lobby.layers {
        let (mut v, mut i) = (0usize, 0usize);
        for a in &layer.data.attachments {
            match &a.kind {
                bal2d_spine::AttachmentKind::Mesh(m) => {
                    v += m.vertices.vertex_count();
                    i += m.triangles.len();
                    max_tris = max_tris.max(m.triangles.len() / 3);
                }
                bal2d_spine::AttachmentKind::Region(_) => {
                    v += 4;
                    i += 6;
                    max_tris = max_tris.max(2);
                }
                _ => {}
            }
        }
        verts = verts.max(v);
        idx = idx.max(i);
    }
    let ok = r.list.positions.try_reserve_exact(verts * 2).is_ok()
        && r.list.uvs.try_reserve_exact(verts * 2).is_ok()
        && r.list.indices.try_reserve_exact(idx).is_ok()
        && r.list.batches.try_reserve_exact(512).is_ok()
        && r.renderer.reserve(max_tris);
    trace(&alloc::format!("buffers: {verts} vertices, {idx} indices, {max_tris} triangles max: {ok}; heap free {} KB", heap_free_kb()));
    if !ok {
        return Some(-12);
    }
    let Some(l) = r.loader.take() else { return Some(-22) };
    r.pack = l.name;
    r.lobby = l.lobby;
    r.pages = l.per_layer;
    r.bake = bake;
    Some(0)
}

/// Writes the placeholder image an LVGL image object shows until the module takes it over
/// (`view::LAYER_FILE`): 16 x 16 transparent ARGB8888, LVGL v9 binary.
fn ensure_layer_file() {
    const NAME: &str = "layer.bin";
    const LEN: usize = 12 + 16 * 16 * 4;
    if Fs::stat(NAME).is_some_and(|s| s.size as usize == LEN) {
        return;
    }
    let mut v = alloc::vec![0u8; LEN];
    v[..12].copy_from_slice(&[0x19, 0x10, 0, 0, 16, 0, 16, 0, 64, 0, 0, 0]);
    Fs::write(NAME, &v);
}

/// Drops the shown pack (its pages free their GPU memory); the next frame loads the current one.
fn unload() {
    let r = rt();
    r.lobby = None;
    r.pages = Vec::new();
    r.bake = None;
    r.pack = String::new();
    r.load_tried = false;
    r.loader = None;
    r.boot_error = 0;
    view::view().set_hidden(true);
    trace(&alloc::format!("unloaded; heap free {} KB", heap_free_kb()));
}

/// Starts the runtime: loads the pack, allocates the target, starts the frame timer.
pub fn start() -> i32 {
    let r = rt();
    if !r.timer.is_null() {
        return 0;
    }
    r.screen = screen_size();
    Fs::mkdirs(PACK_DIR);
    // The pack is loaded on the first frame, outside the Supervisor's stage callback.
    r.timer = unsafe { lvx_timer_create(frame, FRAME_MS, core::ptr::null_mut()) };
    if r.timer.is_null() {
        r.boot_error = -12;
        return -12;
    }
    r.connect = connect::start(r.screen);
    let ctx = crate::module::context();
    let bt = canopus_runtime::bus::subscribe(ctx, "canopus/bt/state", on_bt_state, core::ptr::null_mut());
    trace(&alloc::format!("bt state subscription: {bt:?}"));
    ensure_layer_file();
    r.last_ms = lv::now_ms();
    r.window_start = r.last_ms;
    if Fs::stat(KEEPAWAKE_FLAG).is_some() {
        let ctx = crate::module::context();
        let lock = canopus_runtime::display::acquire_wake_lock(ctx, canopus_abi::WAKELOCK_SCREEN);
        trace(&alloc::format!("keep-awake wake lock: {lock:?}"));
    }
    trace(&alloc::format!("start: screen {:?} heap free {} KB largest {} KB", r.screen, heap_free_kb(), largest_block_kb()));
    0
}

/// Whether a press at screen coordinates starts a drag of the shown lobby (touch callback).
pub fn grabs(x: i32, y: i32) -> bool {
    rt().lobby.as_ref().is_some_and(|l| l.grabs(x as f32, y as f32))
}

fn screen_on() -> bool {
    let ctx = crate::module::context();
    canopus_runtime::display::is_screen_on(ctx).unwrap_or(true)
}

/// Finds where to show the frame: a watchface marker, else the development overlay.
fn ensure_attached(now: u32) {
    let r = rt();
    let v = view::view();
    if now.wrapping_sub(r.attach_check) >= 1000 {
        r.attach_check = now;
        let was = v.mode;
        if OPTIONS_DIRTY.swap(false, Ordering::AcqRel) {
            r.overlay = options() & bal2d_proto::options::OVERLAY != 0 || Fs::stat(OVERLAY_FLAG).is_some();
            if !r.overlay && v.mode == view::Mode::Overlay {
                v.detach();
            }
        }
        if !v.attach_marker(r.screen) && r.overlay {
            let (w, h) = r.screen;
            v.attach_overlay(w, h);
        }
        if v.mode != was {
            trace(&alloc::format!("view {:?} -> {:?}", was, v.mode));
        }
    }
    if !v.attached() {
        return;
    }
    let before = v.rect;
    v.remeasure();
    if v.rect != before {
        trace(&alloc::format!("view rect {:?}", v.rect));
    }
    // The touch proxy follows the head while the lobby is at rest (once a second).
    if r.attach_check == now {
        if let Some(lobby) = r.lobby.as_ref().filter(|l| l.is_idle()) {
            if let Some((x, y, w, h)) = lobby.grab_box() {
                // Whole pixels, a little margin, snapped to 8 px so small sways do not move it.
                let snap = |v: f32| ((v / 8.0) as i32) * 8;
                v.place_touch((snap(x) - 8, snap(y) - 8, snap(w) + 16, snap(h) + 16));
            }
        }
    }
    // (Re)size the target to the view.
    let (w, h) = (v.rect.2.max(1) as u32, v.rect.3.max(1) as u32);
    let need = r.target.as_ref().is_none_or(|t| t.buffer.width != w || t.buffer.height != h);
    if need {
        r.target = None;
        match gpu::Target::new(w, h) {
            Some(t) => {
                let (p, stride) = (t.pixels, t.buffer.stride);
                v.bind(p, w, h, stride);
                trace(&alloc::format!("target {w}x{h} stride {stride}"));
                r.target = Some(t);
            }
            None => {
                r.boot_error = -12;
                trace("target allocation failed");
            }
        }
    }
}

/// Clustering tolerances (screen pixels) from the most exact up: a looser one groups more
/// triangles per GPU call (on heavy lobbies 10-40% fewer calls at 1.25 px) at a small cost in
/// texture placement.
const TOLERANCES: [f32; 3] = [0.75, 1.0, 1.25];
/// A frame (with the UI work it causes) above this cannot keep about 24 fps with the idle share;
/// below the lower mark there is room for the next more exact step.
const BUSY_HIGH_MS: u32 = 31;
const BUSY_LOW_MS: u32 = 20;

/// The most exact clustering the watch keeps up with: one step looser while frames are too
/// heavy, one step back when there is room, at most every 3 seconds.
fn adapt_quality(now: u32) {
    let r = rt();
    if now.wrapping_sub(r.quality_changed) < 3000 {
        return;
    }
    let tol = r.renderer.paths.tolerance;
    let step = TOLERANCES.iter().position(|t| *t >= tol).unwrap_or(0);
    let next = if r.busy_ms > BUSY_HIGH_MS && step + 1 < TOLERANCES.len() {
        step + 1
    } else if r.busy_ms < BUSY_LOW_MS && step > 0 {
        step - 1
    } else {
        return;
    };
    r.renderer.paths.tolerance = TOLERANCES[next];
    r.quality_changed = now;
    // (Integer hundredths: formatting a float would pull in core's float printing.)
    trace(&alloc::format!("busy {} ms: tolerance {}/100 px", r.busy_ms, (TOLERANCES[next] * 100.0) as u32));
}

/// The frame timer's period (`lv_timer_t::period`, its first field, as `lv_timer_set_period`
/// writes it), when the timer is the one this module created (its callback field is `frame`).
fn set_timer_period(timer: *mut c_void, ms: u32) {
    if timer.is_null() {
        return;
    }
    let t = timer as *mut u32;
    // lv_timer_t: period, last_run, timer_cb, user_data, ...
    let cb = unsafe { core::ptr::read_volatile(t.add(2)) } as usize;
    if cb & !1 == (frame as usize) & !1 {
        unsafe { core::ptr::write_volatile(t, ms.clamp(1, FRAME_MS)) };
    }
}

extern "C" fn frame(timer: *mut c_void) {
    let now = lv::now_ms();
    let r = rt();
    // The app page (when open) follows loading and errors even while nothing is drawn.
    if now.wrapping_sub(r.status_at) >= 1000 {
        r.status_at = now;
        app::refresh();
    }
    // Duty-cycle limit. The first call after a frame comes right after LVGL has redrawn the
    // screen with it (the timer then runs every millisecond): that measures what a frame costs
    // beyond its own work (mostly waiting for the panel's TE pulse and transfer). The UI task is
    // blocked for all of it, so a third of frame + redraw stays idle (at least 4 ms): the watch's
    // own UI and Bluetooth always get their turn, however heavy the character and the face are.
    if r.settling {
        r.settling = false;
        let refresh = now.wrapping_sub(r.frame_end).min(1000);
        r.refresh_ms = (r.refresh_ms * 3 + refresh) / 4;
        let own = r.frame_end.wrapping_sub(r.frame_start);
        let busy = own + refresh;
        r.busy_ms = (r.busy_ms * 7 + busy) / 8;
        adapt_quality(now);
        let mut rest = (busy / 3).max(4);
        let link = BT_LINK.load(Ordering::Relaxed);
        if link == 1 && r.bt_was != 1 {
            r.bt_up_at = now;
        }
        if link != r.bt_was {
            trace(&alloc::format!("phone link {link}"));
        }
        r.bt_was = link;
        let settled = link == 1 && now.wrapping_sub(r.bt_up_at) >= LINK_SETTLE_MS;
        if !settled && now.wrapping_sub(r.breather_at) >= BREATHER_EVERY_MS {
            r.breather_at = now;
            rest = rest.max(BREATHER_MS);
        }
        let mut until = now.wrapping_add(rest);
        // And no faster than the frame rate.
        let earliest = r.frame_start.wrapping_add(FRAME_MS);
        if (earliest.wrapping_sub(until) as i32) > 0 {
            until = earliest;
        }
        r.idle_until = until;
    }
    if (now.wrapping_sub(r.idle_until) as i32) < 0 {
        set_timer_period(timer, r.idle_until.wrapping_sub(now));
        return;
    }
    set_timer_period(timer, FRAME_MS);
    // While a pack arrives, two frames a second: the transfer gets the CPU.
    if connect::receiving() && now.wrapping_sub(r.last_ms) < 500 {
        return;
    }
    let dt_ms = now.wrapping_sub(r.last_ms).min(200);
    r.last_ms = now;
    if RELOAD.swap(false, Ordering::AcqRel) {
        unload();
    }
    if r.selftest.is_none() {
        let t = selftest::run(&mut r.renderer);
        trace(&alloc::format!("self-test flags {:#x}", t.flags));
        r.selftest = Some(t);
        return;
    }
    if !r.load_tried {
        let Some(rc) = load_step() else { return };
        let r = rt();
        r.load_tried = true;
        if rc != 0 {
            r.loader = None;
            r.boot_error = rc;
        }
        trace(&alloc::format!("load result {rc}"));
        return;
    }
    if !screen_on() {
        return;
    }
    ensure_attached(now);
    let r = rt();
    let v = view::view();
    let (Some(lobby), Some(target)) = (r.lobby.as_mut(), r.target.as_mut()) else { return };
    if !v.visible() {
        // Pressed state belongs to the screen that went away.
        v.touches.len = 0;
        r.last_ms = now;
        return;
    }
    // Touches, in layer pixels.
    let q = core::mem::take(&mut v.touches);
    for e in &q.items[..q.len] {
        let kind = match e.kind {
            0 => Touch::Down,
            1 => Touch::Move,
            _ => Touch::Up,
        };
        if kind == Touch::Down {
            r.pressed = true;
        }
        if kind == Touch::Up {
            r.pressed = false;
        }
        lobby.touch(kind, (e.x + v.rect.0) as f32, (e.y + v.rect.1) as f32);
    }
    let t0 = lv::now_ms();
    lobby.update(dt_ms as f32 / 1000.0);
    lobby.notices.clear();
    let t1 = lv::now_ms();
    let (fw_w, fw_h) = (lobby.width, lobby.height);
    // The baked bottom replaces its slots (and the clear, when it covers the whole target).
    let bake = r.bake.as_mut().filter(|_| lobby.bake_active());
    r.bake_active = bake.is_some();
    let covers = bake.as_ref().is_some_and(|b| {
        b.x <= v.rect.0
            && b.y <= v.rect.1
            && b.x + b.buffer.width as i32 >= v.rect.0 + target.buffer.width as i32
            && b.y + b.buffer.height as i32 >= v.rect.1 + target.buffer.height as i32
    });
    r.renderer.begin(target, (!covers).then_some(0xFF00_0000));
    if let Some(b) = bake {
        r.renderer.draw_bake(target, b, (v.rect.0, v.rect.1));
        // The GPU copies the background while the CPU builds the paths.
        r.renderer.flush();
    }
    // The pack frames the whole screen; the layer shows its own rectangle of it.
    let cam = lobby.camera;
    let (ox, oy) = (v.rect.0 as f32, v.rect.1 as f32);
    let cx = cam.cx + (ox + target.buffer.width as f32 * 0.5 - fw_w * 0.5) / cam.scale;
    let cy = cam.cy - (oy + target.buffer.height as f32 * 0.5 - fw_h * 0.5) / cam.scale;
    for li in 0..lobby.layers.len() {
        lobby.draw_list(li, &mut r.list);
        r.renderer.draw(target, li, &r.list, &mut r.pages[li], (cx, cy, cam.scale));
    }
    let t2 = lv::now_ms();
    r.renderer.finish();
    let t3 = lv::now_ms();
    r.frame_start = now;
    r.frame_end = t3;
    r.settling = true;
    r.idle_until = t3;
    set_timer_period(timer, 1);
    v.present();
    // Stats over one-second windows.
    let w = &mut r.window;
    w.frames += 1;
    w.update_ms += t1.wrapping_sub(t0);
    w.draw_ms += t2.wrapping_sub(t1);
    w.gpu_ms += t3.wrapping_sub(t2);
    // Triangles drawn (on screen), not the whole draw list.
    w.triangles = r.renderer.stats.triangles;
    w.calls = r.renderer.stats.calls;
    w.submit_ms += r.renderer.stats.submit_ms;
    w.errors += r.renderer.stats.errors;
    if r.renderer.stats.errors != 0 {
        w.last_error = r.renderer.stats.last_error;
    }
    let span = now.wrapping_sub(r.window_start);
    if span >= 1000 {
        let n = w.frames.max(1);
        r.perf = Perf {
            frames: r.perf.frames + w.frames,
            update_ms: w.update_ms / n,
            draw_ms: w.draw_ms / n,
            gpu_ms: w.gpu_ms / n,
            submit_ms: w.submit_ms / n,
            triangles: w.triangles,
            calls: w.calls,
            errors: r.perf.errors + w.errors,
            last_error: if w.errors != 0 { w.last_error } else { r.perf.last_error },
            fps_x10: w.frames * 10_000 / span,
        };
        if r.perf.frames < 400 || r.perf.frames % 300 < 31 {
            trace(&alloc::format!(
                "perf fps {}.{} update {} draw {} (driver {}) gpu {} ui {} tris {} calls {} errors {} last {}",
                r.perf.fps_x10 / 10,
                r.perf.fps_x10 % 10,
                r.perf.update_ms,
                r.perf.draw_ms,
                r.perf.submit_ms,
                r.perf.gpu_ms,
                r.refresh_ms,
                r.perf.triangles,
                r.perf.calls,
                r.perf.errors,
                r.perf.last_error
            ));
        }
        r.window = Perf::default();
        r.window_start = now;
    }
}

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
pub mod bench;
pub mod bubble;
pub mod connect;
pub mod fw;
pub mod fx;
pub mod gpu;
pub mod host;
pub mod lv;
pub mod selftest;
pub mod trust;
pub mod view;

use host::Fs;

/// About 30 frames per second while visible.
const FRAME_MS: u32 = 33;
/// Heap a baked idle must leave free once read (the frame loop and the watch's own UI).
const IDLE_HEADROOM: usize = 768 * 1024;
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
/// Light frames (a frame + redraw under `BREATHER_BUSY_MS`, a baked idle) leave the UI task
/// enough room between them: no breathers then (they showed as a hitch every two seconds).
const BREATHER_MS: u32 = 250;
const BREATHER_EVERY_MS: u32 = 2000;
const BREATHER_BUSY_MS: u32 = 45;
const LINK_SETTLE_MS: u32 = 10_000;
/// The frame timer fires a few milliseconds late (the UI loop's own granularity): it is asked for
/// this much less, and the frame rate cap allows for the lateness measured.
const TIMER_EARLY_MS: u32 = 3;
/// The idle share between frames (frame + redraw / this), and its floor (ms). A third (half
/// for heavy frames) cost the real watch as much frame rate as Mirage showed it was not needed
/// for: the phone connects and the UI answers with a sixth.
const IDLE_DIV: u32 = 6;
const IDLE_MIN_MS: u32 = 4;
/// "Smoothness first" (`options::FLUENT`): the idle share between frames, and its floor (ms).
const FLUENT_IDLE_DIV: u32 = 12;
const FLUENT_IDLE_MIN_MS: u32 = 2;
/// Breathers only within this long of the module starting or the link changing state: a watch
/// without a phone (or whose Supervisor never reports the link) otherwise paused every 2 s.
const BREATHER_WINDOW_MS: u32 = 30_000;
/// After LVGL drew the layer somewhere else than it was framed for (a page or home swipe
/// scrolling it), frames go through the module's own buffer this long: LVGL then redraws the
/// moving layer from it at its own rate instead of having the scene drawn again each time.
const DIRECT_SETTLE_MS: u32 = 400;

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
    /// Left idle on purpose after a frame, and how late the timer then fired.
    pub idle_ms: u32,
    pub late_ms: u32,
    /// Part of `draw_ms` waiting in `vg_lite_flush` for the GPU (it was that far behind).
    pub flush_ms: u32,
    /// Frames drawn (those LVGL drew: in direct mode the draw happens in its redraw), and
    /// redraws of an unchanged frame (LVGL refreshing part of the screen) with their time.
    pub drawn: u32,
    pub redraws: u32,
    pub redraw_ms: u32,
    /// The frames were drawn straight into LVGL's layer (`direct`), not the module's buffer.
    pub direct: bool,
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
    /// `options::FLUENT` (read with the overlay option when the options change).
    fluent: bool,
    /// What `connect::start` answered (0: the phone can send packs).
    pub connect: i32,
    /// The last frame drew the baked background.
    pub bake_active: bool,
    /// The last frame was drawn from the baked idle.
    pub idle_active: bool,
    /// Development: `dev-noidle` turns the baked idle off (A/B timing).
    pub idle_off: bool,
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
    /// Why the last frame was skipped (bits: not visible, paged over, covered; 0 drawn), for
    /// the log on changes.
    gate_was: u8,
    bt_up_at: u32,
    breather_at: u32,
    /// How late the frame timer fires past the planned start (ms, running average).
    late_avg: u32,
    /// Development (`dev-pacetrace`): frames whose pacing is still to be traced.
    pace_trace: u32,
    pace_traced: bool,
    /// A frame's whole cost, its own plus the UI work it causes (ms, smoothed), and when the
    /// clustering tolerance last changed (see `adapt_quality`).
    busy_ms: u32,
    quality_changed: u32,
    /// Layers drawn from their baked idle in the current frame (`Lobby::update_with`).
    baked_mask: u32,
    /// Whether clipping baked shapes pays (`Lobby::idle_clip`), measured per lobby.
    clip_ab: ClipAb,
    /// The current frame is drawn by LVGL's DRAW_MAIN of the view straight into its layer
    /// (LVGL's frame buffer in SRAM): no buffer of the module's own, no copy of it by LVGL.
    pub direct: bool,
    /// `options::CLASSIC`: always draw into the module's buffer.
    direct_off: bool,
    /// `options::SINGLE`: every draw through the driver.
    single: bool,
    /// The next DRAW_MAIN draws a new frame (its cost is the frame's), not a redraw.
    direct_pending: bool,
    /// The current frame was drawn over the whole layer at its first DRAW_MAIN (LVGL refreshes
    /// the screen in two halves: the second finds it done), and the parts LVGL has refreshed
    /// since (absolute clips): a part refreshed again is a new refresh (a widget above changed),
    /// which finds only the widget's old pixels there and must draw.
    direct_whole: bool,
    direct_passed: Vec<[i32; 4]>,
    /// When LVGL last drew the layer away from where it was framed (scrolling), and the time the
    /// current frame's direct draw took (it happens inside LVGL's redraw).
    direct_moved_at: u32,
    direct_frame_ms: u32,
    /// DRAW_MAIN calls that could not draw (no usable layer buffer), in a row.
    direct_failures: u32,
    /// Frames go through the module's buffer until then (LVGL redraws the layer too often).
    direct_backoff_until: u32,
    /// Development: DRAW_MAIN calls traced so far.
    direct_traced: u32,
    /// Direct frames whose pixels were read back (`DIRECT_CHECKS`), and whether one showed the
    /// GPU's output missing from LVGL's layer (then the module's buffer for good).
    direct_checked: u32,
    direct_broken: bool,
    /// Waits for the GPU since start that ran into the driver's 100 ms timeout (it resets the
    /// GPU and drops what was queued: parts of a frame go missing).
    gpu_resets: u32,
    /// After a frame with timeouts no new frame starts until then (the system gets its CPU
    /// and the GPU a rest); timeouts in the current escalation window and when it began; how
    /// far the module has stepped back (`GPU_STEPS`); paused for good at the last step.
    gpu_cool_until: u32,
    gpu_window_resets: u32,
    gpu_window_at: u32,
    /// GPU features (`trust`): the verdicts, those on, and what the self-test allows of them
    /// (page formats it found wrong, batched draws, PAD edges, the background blit); frames
    /// drawn since the last timeout and since the trial began.
    trust: trust::Trust,
    features: u8,
    st_formats: u32,
    st_stream: bool,
    st_pad: bool,
    st_blit: bool,
    bake_allowed: bool,
    clean_frames: u32,
    trial_frames: u32,
    trial_resets: u32,
    /// Development (`dev-gpu-timeout`, a hex feature mask): frames with any of these features on
    /// count a timeout, to try the trials where the GPU never times out (Mirage).
    force_timeout: u8,
    /// The start-up GPU measurement (`bench`).
    bench: Option<bench::Bench>,
    /// Stats windows traced (the first few, then one in ten).
    perf_traced: u32,
    /// Degenerate draws left out since start (`gpu::degenerate`), the first few logged.
    skipped: u32,
    /// Hung submits whose draws went to the log; development (`dev-gpu-hang` "auto"): fake a
    /// hang on a draw picked after a while.
    hang_dumps: u32,
    fake_auto: bool,
    /// LVGL's layer, wrapped for the GPU.
    layer_buffer: Option<gpu::VgBuffer>,
    /// When the module started and when the phone link last changed (breathers).
    started_at: u32,
    link_changed_at: u32,
    /// The previous drawn frame (a gap ends the stats window: hidden time is no frame rate).
    last_frame_at: u32,
    /// Touch effects, drawn over the lobby.
    fx: fx::Fx,
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
                fluent: false,
                connect: 1,
                bake_active: false,
                idle_active: false,
                idle_off: false,
                idle_until: 0,
                frame_start: 0,
                frame_end: 0,
                settling: false,
                refresh_ms: 0,
                busy_ms: 0,
                status_at: 0,
                bt_was: 0xFF,
                gate_was: 0,
                bt_up_at: 0,
                breather_at: 0,
                late_avg: 0,
                pace_trace: 0,
                pace_traced: false,
                quality_changed: 0,
                pressed: false,
                baked_mask: 0,
                clip_ab: ClipAb::new(),
                direct: false,
                direct_off: false,
                single: false,
                direct_pending: false,
                direct_whole: false,
                direct_passed: Vec::new(),
                direct_moved_at: 0,
                direct_frame_ms: 0,
                direct_failures: 0,
                direct_backoff_until: 0,
                direct_traced: 0,
                direct_checked: 0,
                direct_broken: false,
                gpu_resets: 0,
                gpu_cool_until: 0,
                gpu_window_resets: 0,
                gpu_window_at: 0,
                trust: trust::Trust { good: 0, bad: 0, trying: 0 },
                features: 0,
                st_formats: 0,
                st_stream: false,
                st_pad: false,
                st_blit: false,
                bake_allowed: false,
                clean_frames: 0,
                trial_frames: 0,
                trial_resets: 0,
                force_timeout: 0,
                bench: None,
                perf_traced: 0,
                skipped: 0,
                hang_dumps: 0,
                fake_auto: false,
                layer_buffer: None,
                started_at: 0,
                link_changed_at: 0,
                last_frame_at: 0,
                fx: fx::Fx::new(),
            });
        }
        slot.as_mut().unwrap()
    }
}

static mut TRACE_LEFT: u32 = 400;
/// The diagnostic log (the phone can fetch it: `LOG`); past `LOG_KEEP` bytes at start it moves
/// to `LOG_PREVIOUS`, so it holds the last few boots.
pub const LOG_FILE: &str = "log.txt";
const LOG_PREVIOUS: &str = "log-previous.txt";
const LOG_KEEP: u32 = 96 * 1024;

fn rotate_log() {
    if Fs::stat(LOG_FILE).is_some_and(|st| st.size as u32 > LOG_KEEP) {
        Fs::remove(LOG_PREVIOUS);
        Fs::rename(LOG_FILE, LOG_PREVIOUS);
    }
}

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
    Fs::append(LOG_FILE, &text);
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
            | (r.idle_active as u32) << 4
            | (p.direct as u32) << 5
            | ((r.direct_checked >= DIRECT_CHECKS) as u32) << 6
            | (r.direct_broken as u32) << 7
            | (v.attached() as u32) << 1
            | (v.visible() as u32) << 2
            | (r.loader.is_some() as u32) << 3
            | (r.boot_error.unsigned_abs().min(255)) << 8
            | r.selftest.map(|t| t.flags).unwrap_or(0),
        ui_ms: r.refresh_ms,
        selftest: r.selftest.map_or(0, |t| t.passed),
        idle_ms: p.idle_ms,
        late_ms: p.late_ms,
        flush_ms: p.flush_ms,
        redraws: p.redraws,
        gpu_resets: r.gpu_resets,
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

pub fn fluent_on() -> bool {
    options() & bal2d_proto::options::FLUENT != 0
}

pub fn direct_on() -> bool {
    options() & bal2d_proto::options::CLASSIC == 0
}

/// Flips direct drawing (app page button).
pub fn toggle_direct() {
    let o = options() ^ bal2d_proto::options::CLASSIC;
    if Fs::write(OPTIONS_FILE, &[o]) {
        options_changed();
    }
}

/// Flips "smoothness first" (app page button).
pub fn toggle_fluent() {
    let o = options() ^ bal2d_proto::options::FLUENT;
    if Fs::write(OPTIONS_FILE, &[o]) {
        options_changed();
    }
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
        // Where a frame's time goes, in the order it passes: animation, drawing (of which the
        // driver and waiting for the GPU to catch up), the GPU finishing, LVGL's redraw, the
        // pause the pacing keeps, and how late the timer fired after it.
        text += &alloc::format!(
            "{}.{} fps · 画质{} · {}\n动画 {} · 绘制 {}（驱动 {} 等GPU {}）\nGPU收尾 {} · 屏幕刷新 {} · 空闲 {} · 迟到 {} ms\n{} 三角形 · {} 次绘制{}\n",
            r.perf.fps_x10 / 10,
            r.perf.fps_x10 % 10,
            quality,
            if r.perf.direct { "直接绘制" } else { "缓冲绘制" },
            r.perf.update_ms,
            r.perf.draw_ms,
            r.perf.submit_ms,
            r.perf.flush_ms,
            r.perf.gpu_ms,
            r.refresh_ms,
            r.perf.idle_ms,
            r.perf.late_ms,
            r.perf.triangles,
            r.perf.calls,
            match (r.bake_active, r.idle_active) {
                (_, true) => " · 待机已预烘焙",
                (true, false) => " · 背景已烘焙",
                _ => "",
            },
        );
        if r.perf.redraws > 1 {
            text += &alloc::format!("重绘 {} 次/秒（{} ms）\n", r.perf.redraws, r.perf.redraw_ms);
        }
    }
    if r.skipped != 0 {
        text += &alloc::format!("跳过退化绘制 {} 次（开机以来）\n", r.skipped);
    }
    if r.gpu_resets != 0 {
        text += &alloc::format!("GPU 超时复位 {} 次（开机以来）\n", r.gpu_resets);
    }
    if let Some(b) = r.bench {
        let ms = |v: u32| alloc::format!("{}.{}", v / 10, v % 10);
        text += &alloc::format!(
            "GPU 每屏耗时 ms：清屏 {} · 32位贴图 {}/{} · 8565 {}/{} · 背景 {}\n",
            ms(b.clear),
            ms(b.tex32),
            ms(b.tex32_nc),
            ms(b.tex24),
            ms(b.tex24_nc),
            ms(b.blit)
        );
    }
    if r.selftest.is_some() {
        text += &alloc::format!("GPU 功能：已开 {}", trust::names(r.features));
        if r.trust.trying != 0 {
            text += &alloc::format!("（正在试 {}）", trust::names(r.trust.trying));
        }
        if r.trust.bad != 0 {
            text += &alloc::format!("；会超时 {}", trust::names(r.trust.bad));
        }
        text += "\n";
    }
    if r.direct_broken {
        text += "直接绘制自检未通过（GPU 没写进系统缓冲区），已改用缓冲绘制\n";
    } else if r.direct_checked >= DIRECT_CHECKS && r.perf.direct {
        text += "直接绘制：本机读回校验通过\n";
    }
    if r.loader.is_some() {
        text += "正在加载角色…\n";
    }
    if let Some(t) = r.selftest {
        text += &selftest_text(t.flags);
        let failed = probe_names(selftest::probe::ALL & !t.passed);
        if t.flags & selftest::flag::RAN != 0 && !failed.is_empty() {
            text += &alloc::format!("未通过的探针：{failed}\n");
            // What the read-backs found (hex probe masks): at once after each probe's own wait,
            // a while after all of them, through the cache; and the first failed call.
            text += &alloc::format!("自检读回：立即 {:x} · 稍后 {:x} · 缓存 {:x}\n", t.now, t.late, t.cached);
            if t.first_error != 0 {
                text += &alloc::format!("自检返回码：第 {} 项 {}\n", t.first_error >> 24, t.first_error & 0xFFFF);
            }
        }
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
    if flags & flag::NO_STREAM != 0 {
        t += "GPU 自检：批量绘制异常，已改为逐次绘制\n";
    }
    t
}

/// The self-test probes in `mask`, by short name (for the status page and its screenshots).
pub fn probe_names(mask: u32) -> String {
    const NAMES: [&str; 20] = [
        "FP32", "S16", "大纹理", "4444", "8565", "索引色", "边缘", "着色", "叠加", "正片叠底", "滤色", "透明度", "贴图",
        "直写小", "直写大", "越界", "区域贴图", "批量1", "批量2", "右端",
    ];
    let mut t = String::new();
    for (i, n) in NAMES.iter().enumerate() {
        if mask & (1 << i) != 0 {
            if !t.is_empty() {
                t += " ";
            }
            t += n;
        }
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
        trace(&alloc::format!("loading {name}; heap: free {} KB, largest {} KB", heap_free_kb(), largest_block_kb()));
        let Some(file) = Fs::open(&alloc::format!("{PACK_DIR}/{name}"), canopus_abi::FS_READ) else { return Some(-5) };
        r.loader = Some(Loader { name, src: FileSource { file }, t0: lv::now_ms(), lobby: None, pages: Vec::new(), per_layer: Vec::new(), next: 0 });
        return None;
    };
    let Some(lobby) = l.lobby.as_ref() else {
        match Lobby::load_from(&mut l.src) {
            Ok((mut lobby, pages)) => {
                // The background image is drawn with a blit: without a working one (self-test)
                // or before the watch has shown it draws it without timeouts (`trust`), the
                // baked slots are drawn as they are.
                if !r.bake_allowed {
                    lobby.bake = None;
                }
                trace(&alloc::format!("parsed in {} ms; {} pages; heap: free {} KB, largest {} KB", lv::now_ms().wrapping_sub(l.t0), pages.len(), heap_free_kb(), largest_block_kb()));
                early_idle(&mut lobby, &pages, &mut l.src);
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
        // No room after all (the firmware resets on a failed malloc, so this is checked first):
        // the baked idles loaded early go, the pages come first.
        if !heap_allows(page_bytes(p) + 64 * 1024) {
            if let Some(lobby) = l.lobby.as_mut() {
                drop_idles(lobby, "a page");
            }
        }
        if !heap_allows(page_bytes(p) + 64 * 1024) {
            trace(&alloc::format!("page {}: {} KB do not fit; heap: free {} KB, largest {} KB", l.next, page_bytes(p) / 1024, heap_free_kb(), largest_block_kb()));
            return Some(-12);
        }
        let up = gpu::Page::from_pack(&mut l.src, p);
        trace(&alloc::format!("page {}/{} {}x{} {:?}: {}; heap: free {} KB", p.layer, p.index, p.header.width, p.header.height, p.header.format, up.as_ref().err().copied().unwrap_or(0), heap_free_kb()));
        match up {
            Ok(page) => l.per_layer[p.layer as usize].push(page),
            Err(e) => return Some(e),
        }
        l.next += 1;
        return None;
    }
    // The background image, like the pages: room first (early idles go, else the image does).
    let bake_need = l.lobby.as_ref().and_then(|lb| lb.bake.as_ref()).map_or(0, |b| b.header.width as usize * b.header.height as usize * 2 + 64);
    if bake_need > 0 && !heap_allows(bake_need + 64 * 1024) {
        if let Some(lb) = l.lobby.as_mut() {
            drop_idles(lb, "background image");
            if !heap_allows(bake_need + 64 * 1024) {
                trace(&alloc::format!("background image: {} KB do not fit, drawn as its slots", bake_need / 1024));
                lb.bake = None;
            }
        }
    }
    let Some(lobby) = l.lobby.as_ref() else { return Some(-22) };
    let bake = match &lobby.bake {
        Some(b) => match gpu::BakeImage::from_pack(&mut l.src, b) {
            Ok(img) => {
                trace(&alloc::format!("bake {}x{} at ({}, {}){}; heap: free {} KB", b.header.width, b.header.height, b.header.x, b.header.y, if b.after_intro { " after the intro" } else { "" }, heap_free_kb()));
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
    // Their room first too (vertex and index lists, batches, the renderer's grouping, outline
    // and stream buffers: an estimate on the generous side).
    let buffers = verts * 16 + idx * 2 + 32 * 1024 + max_tris * 480 + 128 * 1024;
    if !heap_allows(buffers + 64 * 1024) {
        if let Some(lb) = l.lobby.as_mut() {
            drop_idles(lb, "working buffers");
        }
        if !heap_allows(buffers + 64 * 1024) {
            trace(&alloc::format!("buffers: about {} KB do not fit; heap: free {} KB", buffers / 1024, heap_free_kb()));
            return Some(-12);
        }
    }
    let ok = r.list.positions.try_reserve_exact(verts * 2).is_ok()
        && r.list.uvs.try_reserve_exact(verts * 2).is_ok()
        && r.list.indices.try_reserve_exact(idx).is_ok()
        && r.list.batches.try_reserve_exact(512).is_ok()
        && r.renderer.reserve(max_tris);
    trace(&alloc::format!("buffers: {verts} vertices, {idx} indices, {max_tris} triangles max: {ok}; heap: free {} KB", heap_free_kb()));
    if !ok {
        return Some(-12);
    }
    // The baked idle last, only into memory to spare (it is an optimization: without it the
    // layer is animated). Reading a compressed section holds it twice for a moment.
    let Some(l) = r.loader.as_mut() else { return Some(-22) };
    if let Some(lobby) = l.lobby.as_mut() {
        for li in 0..lobby.layers.len() {
            let Some(Some(e)) = lobby.idle_entries.get(li).copied() else { continue };
            if lobby.idle.get(li).is_some_and(|i| i.is_some()) || skip_idle() & (1 << li.min(31)) != 0 {
                continue;
            }
            let need = e.raw_len as usize + e.stored_len as usize + e.raw_len as usize / 4;
            let loaded = heap_allows(need + IDLE_HEADROOM) && lobby.load_idle(&mut l.src, li);
            trace(&alloc::format!("baked idle {li}: {} KB, {}; heap: free {} KB", e.raw_len / 1024, if loaded { "loaded" } else { "skipped" }, heap_free_kb()));
        }
    }
    let Some(l) = r.loader.take() else { return Some(-22) };
    // Each lobby starts at the bakes' own tolerance (1 px) and adapts from there (a light one
    // shown after a heavy one would otherwise start where the heavy one left it; live frames on
    // the watch are heavy, and at 0.75 px a talk drew a quarter more calls).
    r.renderer.paths.tolerance = TOLERANCES[1];
    r.quality_changed = lv::now_ms();
    r.clip_ab = ClipAb::new();
    r.pack = l.name;
    Fs::remove(BANS_FILE);
    r.lobby = l.lobby;
    r.pages = l.per_layer;
    r.bake = bake;
    Some(0)
}

/// Frees the baked idles loaded early (they come last, into what is left: what else a pack
/// needs comes first).
fn drop_idles(lobby: &mut Lobby, for_what: &str) {
    if lobby.idle.iter().any(|i| i.is_some()) {
        lobby.idle.iter_mut().for_each(|i| *i = None);
        trace(&alloc::format!("not enough room for {for_what} next to the baked idles: dropped them; heap: free {} KB", heap_free_kb()));
    }
}

/// Bytes a page takes uploaded (widened to BGRA8888 when its format is one the GPU gets wrong
/// or the drawing does not trust yet).
fn page_bytes(p: &bal2d_core::Page) -> usize {
    let bad = gpu::BAD_FORMATS.load(Ordering::Relaxed);
    let f = p.header.format;
    let bpp = if f != bal2d_pack::PageFormat::Bgra8888 && bad & (1 << f as u32) != 0 { 4 } else { f.bytes_per_pixel() };
    p.header.width as usize * p.header.height as usize * bpp + 64
}

/// Loads baked idles before the pages when what the rest of the pack needs (pages as they will
/// be uploaded, the background image, working buffers) still fits after them: loaded last, the
/// transient copy of a compressed section did not fit next to the pages on the watch (Hoshino's
/// 1.4 MB background idle was skipped and animated live, the frame's largest CPU cost).
/// Development (`dev-skip-idle`, a hex layer mask): layers whose baked idle is not loaded (to
/// time a layer animated live).
fn skip_idle() -> u32 {
    Fs::read("dev-skip-idle", 8)
        .and_then(|b| u32::from_str_radix(core::str::from_utf8(&b).ok()?.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(0)
}

fn early_idle(lobby: &mut Lobby, pages: &[bal2d_core::Page], src: &mut FileSource) {
    let skip = skip_idle();
    let page_bytes: usize = pages.iter().map(page_bytes).sum();
    let bake_bytes = lobby.bake.as_ref().map_or(0, |b| b.header.width as usize * b.header.height as usize * 2 + 64);
    // Vertex, path and outline buffers, the GPU stream (a few hundred KB).
    let rest = page_bytes + bake_bytes + 640 * 1024;
    for li in 0..lobby.layers.len() {
        let Some(Some(e)) = lobby.idle_entries.get(li).copied() else { continue };
        if skip & (1 << li.min(31)) != 0 {
            continue;
        }
        // Held for good: the bake and what is derived from it; for a moment also the stored
        // (compressed) section. Each needs a block of its own, the sum the free total.
        let (raw, stored) = (e.raw_len as usize, e.stored_len as usize);
        let kept = raw + raw / 4;
        let (free, largest) = fw::heap_info();
        let after = (free as usize).saturating_sub(kept + rest);
        if after < HEAP_RESERVE as usize + 256 * 1024 || (largest as usize) < raw.max(stored) + 64 * 1024 {
            trace(&alloc::format!("baked idle {li}: {} KB, not before the pages (free {} KB, largest {} KB)", raw / 1024, free / 1024, largest / 1024));
            continue;
        }
        let loaded = lobby.load_idle(src, li);
        trace(&alloc::format!("baked idle {li}: {} KB, {} before the pages; heap: free {} KB", raw / 1024, if loaded { "loaded" } else { "not loaded" }, heap_free_kb()));
    }
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
    r.fx = fx::Fx::new();
    bubble::bubble().clear();
    view::view().set_hidden(true);
    trace(&alloc::format!("unloaded; heap: free {} KB", heap_free_kb()));
}

/// Starts the runtime: loads the pack, allocates the target, starts the frame timer.
pub fn start() -> i32 {
    let r = rt();
    if !r.timer.is_null() {
        return 0;
    }
    r.screen = screen_size();
    rotate_log();
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
    r.started_at = r.last_ms;
    r.link_changed_at = r.last_ms;
    if Fs::stat(KEEPAWAKE_FLAG).is_some() {
        let ctx = crate::module::context();
        let lock = canopus_runtime::display::acquire_wake_lock(ctx, canopus_abi::WAKELOCK_SCREEN);
        trace(&alloc::format!("keep-awake wake lock: {lock:?}"));
    }
    trace(&alloc::format!("start: screen {:?} heap free {} KB, largest {} KB", r.screen, heap_free_kb(), largest_block_kb()));
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
            let o = options();
            r.overlay = o & bal2d_proto::options::OVERLAY != 0 || Fs::stat(OVERLAY_FLAG).is_some();
            if r.fluent != (o & bal2d_proto::options::FLUENT != 0) {
                r.fluent = !r.fluent;
                trace(&alloc::format!("smoothness first: {}", r.fluent));
            }
            let single = o & bal2d_proto::options::SINGLE != 0;
            r.renderer.want_stream(!single, r.lobby.is_some());
            if single != r.single {
                r.single = single;
                trace(&alloc::format!("draws one by one: {single}"));
            }
            let classic = o & bal2d_proto::options::CLASSIC != 0 || Fs::stat("dev-classic").is_some();
            if r.direct_off != classic {
                r.direct_off = classic;
                trace(&alloc::format!("own buffer (no direct drawing): {classic}"));
            }
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
/// heavy, one step back when there is room, at most every 3 seconds. Only frames with a layer
/// drawn live count (the clustering is theirs; baked frames are light and would step it back
/// before every talk, which then started at the costliest grouping).
fn adapt_quality(now: u32) {
    let r = rt();
    if now.wrapping_sub(r.quality_changed) < 3000 {
        return;
    }
    let live = r.lobby.as_ref().is_some_and(|l| {
        let all = (1u32 << l.layers.len().min(31)) - 1;
        r.baked_mask & all != all
    });
    if !live {
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
    // beyond its own work (mostly waiting for the panel's TE pulse and transfer; in direct mode
    // the frame's drawing too, measured apart). A sixth of frame + redraw then stays idle (at
    // least 4 ms), so the watch's own UI and Bluetooth always get their turn, however heavy the
    // character is; "smoothness first" (options::FLUENT) keeps a twelfth. While the phone may be
    // connecting, breathers leave longer gaps now and then (scripts/btstress.py is the test).
    if r.settling {
        r.settling = false;
        // A direct frame is drawn inside LVGL's redraw: that part is the frame's own.
        let refresh = now.wrapping_sub(r.frame_end).min(1000).saturating_sub(r.direct_frame_ms);
        r.refresh_ms = (r.refresh_ms * 3 + refresh) / 4;
        let own = r.frame_end.wrapping_sub(r.frame_start) + r.direct_frame_ms;
        let busy = own + refresh;
        r.busy_ms = (r.busy_ms * 7 + busy) / 8;
        adapt_quality(now);
        let mut rest = if r.fluent {
            (busy / FLUENT_IDLE_DIV).max(FLUENT_IDLE_MIN_MS)
        } else {
            (busy / IDLE_DIV).max(IDLE_MIN_MS)
        };
        let link = BT_LINK.load(Ordering::Relaxed);
        if link == 1 && r.bt_was != 1 {
            r.bt_up_at = now;
        }
        if link != r.bt_was {
            trace(&alloc::format!("phone link {link}"));
            r.link_changed_at = now;
        }
        r.bt_was = link;
        let settled = link == 1 && now.wrapping_sub(r.bt_up_at) >= LINK_SETTLE_MS;
        let connecting = now.wrapping_sub(r.started_at) < BREATHER_WINDOW_MS || now.wrapping_sub(r.link_changed_at) < BREATHER_WINDOW_MS;
        if !settled && connecting && r.busy_ms >= BREATHER_BUSY_MS && now.wrapping_sub(r.breather_at) >= BREATHER_EVERY_MS {
            r.breather_at = now;
            rest = rest.max(BREATHER_MS);
        }
        let mut until = now.wrapping_add(rest);
        // And no faster than the frame rate (as the timer will really fire).
        let earliest = r.frame_start.wrapping_add(FRAME_MS.saturating_sub(r.late_avg.min(FRAME_MS / 4)));
        if (earliest.wrapping_sub(until) as i32) > 0 {
            until = earliest;
        }
        r.idle_until = until;
        r.window.idle_ms += until.wrapping_sub(now);
        if r.pace_trace > 0 {
            r.pace_trace -= 1;
            trace(&alloc::format!("pace start {} end {} settle {} until {} rest {} late {}", r.frame_start, r.frame_end, now, until, rest, r.late_avg));
        }
    }
    if (now.wrapping_sub(r.idle_until) as i32) < 0 {
        let left = r.idle_until.wrapping_sub(now);
        set_timer_period(timer, if left > TIMER_EARLY_MS + 1 { left - TIMER_EARLY_MS } else { 1 });
        return;
    }
    set_timer_period(timer, FRAME_MS);
    let late = now.wrapping_sub(r.idle_until).min(1000);
    r.late_avg = (r.late_avg * 7 + late) / 8;
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
        // The self-test reads back what the GPU wrote: through the non-cached alias.
        gpu::UNCACHED.store(true, Ordering::Relaxed);
        let t = selftest::run(&mut r.renderer);
        r.idle_off = Fs::read("dev-noidle", 4).is_some();
        if Fs::read("dev-nostream", 4).is_some() {
            gpu::STREAM_OFF.store(true, Ordering::Relaxed);
        }
        // What the self-test allows; the features on are the ones the watch has shown it
        // draws without timeouts.
        r.st_formats = gpu::BAD_FORMATS.load(Ordering::Relaxed);
        r.st_stream = !gpu::STREAM_OFF.load(Ordering::Relaxed);
        r.st_pad = t.passed & selftest::probe::PAD != 0;
        r.st_blit = !(t.flags & selftest::flag::RAN != 0 && t.passed & selftest::probe::BLIT == 0);
        r.force_timeout = Fs::read("dev-gpu-timeout", 8)
            .and_then(|b| u8::from_str_radix(core::str::from_utf8(&b).ok()?.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        // How fast this GPU fills: what a submit may carry so it stays well within the driver's
        // 100 ms (the pack is not loaded yet: the heap has room for the measurement).
        r.bench = bench::run();
        if let Some(b) = r.bench {
            gpu::SPLIT_PX.store(b.split_px(), Ordering::Relaxed);
            trace(&alloc::format!(
                "gpu bench (ms/screen x10): clear {} tex32 {} (non-cached {}) tex8565 {} (non-cached {}) blit565 {}; timeouts {}; submit {} px",
                b.clear, b.tex32, b.tex32_nc, b.tex24, b.tex24_nc, b.blit, b.timeouts, b.split_px()
            ));
        }
        if let Some(b) = Fs::read("dev-gpu-hang", 16) {
            let t = core::str::from_utf8(&b).unwrap_or("").trim();
            r.fake_auto = t == "auto";
            r.renderer.hangs.fake = u32::from_str_radix(t.trim_start_matches("0x"), 16).unwrap_or(0);
        }
        r.trust = trust::Trust::load();
        r.features = r.trust.good;
        apply_features(r);
        trace(&alloc::format!(
            "gpu features: on {}; bad {}",
            trust::names(r.features),
            trust::names(r.trust.bad)
        ));
        r.pace_traced = Fs::read("dev-pacetrace", 4).is_none();
        trace(&alloc::format!("self-test flags {:#x}, region blits {}, baked idle off {}, batched draws {}", t.flags, r.renderer.paths.blits, r.idle_off, !gpu::STREAM_OFF.load(Ordering::Relaxed)));
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
        trace(&alloc::format!("load result: {rc}"));
        return;
    }
    if !screen_on() {
        return;
    }
    ensure_attached(now);
    let r = rt();
    let v = view::view();
    if r.lobby.is_none() || r.target.is_none() {
        return;
    }
    let (shown, paged, covered) = (v.visible(), v.paged_over(), v.covered(now));
    let gate = !shown as u8 | (paged as u8) << 1 | (covered as u8) << 2;
    if gate != r.gate_was {
        r.gate_was = gate;
        trace(&alloc::format!("gate: visible {shown}, paged over {paged}, covered {covered}"));
    }
    if gate != 0 {
        // Pressed state belongs to the screen that went away.
        v.touches.len = 0;
        bubble::bubble().clear();
        r.last_ms = now;
        // A page sliding over makes LVGL redraw the layer at its own rate: from the buffer, not
        // by drawing the scene again each time.
        if r.direct {
            r.direct = false;
            draw_own(r, v.rect);
            v.set_direct(false);
            trace("own buffer while away");
        }
        return;
    }
    // GPU timeouts: a rest (no new frame; what is on the screen stays).
    if (r.gpu_cool_until.wrapping_sub(now) as i32) > 0 {
        r.last_ms = now;
        return;
    }
    // A gap (the layer away, the screen off) is no frame rate: the stats window starts over.
    if now.wrapping_sub(r.last_frame_at) > 500 {
        r.window = Perf::default();
        r.window_start = now;
    }
    r.last_frame_at = now;
    let Some(lobby) = r.lobby.as_mut() else { return };
    // Touches, in layer pixels. A finger ends the baked idle: the skeletons catch up first.
    let q = core::mem::take(&mut v.touches);
    if q.len > 0 {
        lobby.resume();
    }
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
        r.fx.touch(e.kind, e.x as f32, e.y as f32);
    }
    let t0 = lv::now_ms();
    // Layers that only play their idle loop are drawn from their bakes, not animated.
    let baked = lobby.update_with(dt_ms as f32 / 1000.0, !r.idle_off);
    r.idle_active = baked != 0;
    r.baked_mask = baked;
    // A talk with lines: its speech bubble (LVGL draws it over the layer).
    let b = bubble::bubble();
    for n in &lobby.notices {
        if let bal2d_core::Notice::TalkStarted(i) = n {
            match lobby.lines.get(*i).filter(|l| !l.is_empty()) {
                Some(lines) => b.start(lines),
                None => b.stop(),
            }
        }
    }
    lobby.notices.clear();
    if b.active() {
        let anchor = lobby.grab_box().map(|(x, y, w, _)| (x + w * 0.5, y));
        b.update(dt_ms as f32, bubble_parent(v.obj, r.screen), r.screen, anchor);
    }
    r.fx.update(dt_ms as f32 / 1000.0);
    let t1 = lv::now_ms();
    // Straight into LVGL's layer while it stays where it is framed (it is drawn there in the
    // redraw this frame asks for); through the module's buffer while it scrolls.
    let direct = !r.direct_off
        && !r.direct_broken
        && r.direct_failures < DIRECT_FAILURES
        && now.wrapping_sub(r.direct_moved_at) >= DIRECT_SETTLE_MS
        && (r.direct_backoff_until.wrapping_sub(now) as i32) <= 0;
    let t3 = if direct {
        if !r.direct {
            trace("direct drawing");
        }
        r.direct = true;
        r.direct_pending = true;
        v.set_direct(true);
        t1
    } else {
        if r.direct {
            r.direct = false;
            trace(if r.direct_failures >= DIRECT_FAILURES { "own buffer: no usable layer" } else { "own buffer" });
        }
        let (draw, gpu) = draw_own(r, v.rect);
        v.set_direct(false);
        count_draw(r, draw, gpu);
        t1.wrapping_add(draw + gpu)
    };
    r.frame_start = now;
    r.frame_end = t3;
    r.direct_frame_ms = 0;
    // Development: trace the pacing of 90 frames once the lobby has played a while.
    if !r.pace_traced && r.perf.frames > 900 {
        r.pace_traced = true;
        r.pace_trace = 90;
    }
    r.settling = true;
    r.idle_until = t3;
    set_timer_period(timer, 1);
    v.present();
    // Stats over one-second windows.
    let w = &mut r.window;
    w.frames += 1;
    w.update_ms += t1.wrapping_sub(t0);
    w.late_ms += late;
    w.direct = r.direct;
    let span = now.wrapping_sub(r.window_start);
    if span >= 1000 {
        clip_ab_window(r);
        let w = &mut r.window;
        let n = w.frames.max(1);
        let d = w.drawn.max(1);
        r.perf = Perf {
            frames: r.perf.frames + w.frames,
            update_ms: w.update_ms / n,
            draw_ms: w.draw_ms / d,
            gpu_ms: w.gpu_ms / d,
            submit_ms: w.submit_ms / d,
            flush_ms: w.flush_ms / d,
            triangles: w.triangles,
            calls: w.calls,
            errors: r.perf.errors + w.errors,
            last_error: if w.errors != 0 { w.last_error } else { r.perf.last_error },
            fps_x10: w.frames * 10_000 / span,
            idle_ms: w.idle_ms / n,
            late_ms: w.late_ms / n,
            drawn: w.drawn,
            redraws: w.redraws,
            redraw_ms: w.redraw_ms / w.redraws.max(1),
            direct: w.direct,
        };
        r.perf_traced += 1;
        if r.perf_traced <= 4 || r.perf_traced % 10 == 0 {
            trace(&alloc::format!(
                "perf fps {}.{} update {} draw {} (driver {} flush {}) gpu {} ui {} idle {} late {} tris {} calls {} direct {} redraws {} errors {} last {}",
                r.perf.fps_x10 / 10,
                r.perf.fps_x10 % 10,
                r.perf.update_ms,
                r.perf.draw_ms,
                r.perf.submit_ms,
                r.perf.flush_ms,
                r.perf.gpu_ms,
                r.refresh_ms,
                r.perf.idle_ms,
                r.perf.late_ms,
                r.perf.triangles,
                r.perf.calls,
                r.perf.direct,
                r.perf.redraws,
                r.perf.errors,
                r.perf.last_error
            ));
        }
        if r.perf.direct && r.perf.redraws > r.perf.drawn.max(1) * DIRECT_REDRAWS_PER_FRAME {
            r.direct_backoff_until = now.wrapping_add(DIRECT_BACKOFF_MS);
            trace(&alloc::format!("{} redraws for {} frames: own buffer for a while", r.perf.redraws, r.perf.drawn));
        }
        r.window = Perf::default();
        r.window_start = now;
    }
}

/// Where the speech bubble goes: over all of the watchface's widgets, inside its page. The
/// layer's parent holds the widgets of its level; stock watchfaces keep more widgets in later
/// siblings of that parent (the complications at the bottom), so the grandparent, when it spans
/// the screen, is above them all.
fn bubble_parent(obj: *mut c_void, screen: (i32, i32)) -> *mut c_void {
    let parent = unsafe { lv::parent(obj) };
    let grand = unsafe { lv::parent(parent) };
    if !grand.is_null() {
        let (x, y, w, h) = unsafe { lv::measured(grand) };
        if (x, y) == (0, 0) && w >= screen.0 && h >= screen.1 && !unsafe { lv::parent(grand) }.is_null() {
            return grand;
        }
    }
    parent
}

/// Direct DRAW_MAIN calls that may fail in a row before frames go through the module's buffer.
const DIRECT_FAILURES: u32 = 8;
/// LVGL redrawing the layer more often than this per drawn frame (an animated watchface under
/// it invalidates its own areas all the time; each redraw draws the scene again in direct
/// mode, unseen by the pacing): frames go through the module's buffer for `DIRECT_BACKOFF_MS`,
/// whose redraws are cheap copies.
const DIRECT_REDRAWS_PER_FRAME: u32 = 1;
const DIRECT_BACKOFF_MS: u32 = 10_000;
/// Direct frames checked by reading pixels back: before drawing, the CPU marks a few pixels of
/// the area in LVGL's layer; the frame clears or covers every one of them, so a mark still
/// there after the GPU finished means its writes did not reach the layer (another address the
/// GPU sees, a cache in the way) and the frames go through the module's buffer from then on.
const DIRECT_CHECKS: u32 = 8;

/// Pixels marked for the read-back check, inside `clip` (buffer coordinates): byte offsets into
/// the layer, their marks, and the bytes per pixel.
struct Marks {
    at: [usize; 4],
    mark: [[u8; 4]; 4],
    bpp: usize,
}

impl Marks {
    fn new(layer: &lv::DrawLayer, clip: [i32; 4]) -> Option<Marks> {
        let bpp = match layer.cf {
            lv::cf::ARGB8888 | lv::cf::XRGB8888 => 4,
            lv::cf::RGB565 => 2,
            lv::cf::RGB888 => 3,
            _ => return None,
        };
        let (w, h) = (clip[2] - clip[0], clip[3] - clip[1]);
        if w < 4 || h < 4 || layer.data.is_null() {
            return None;
        }
        let mut m = Marks { at: [0; 4], mark: [[0; 4]; 4], bpp };
        for (i, (fx, fy)) in [(1, 1), (3, 1), (1, 3), (3, 3)].into_iter().enumerate() {
            let (x, y) = (clip[0] + w * fx / 4, clip[1] + h * fy / 4);
            m.at[i] = y as usize * layer.stride as usize + x as usize * bpp;
            let k = i as u8;
            m.mark[i] = [0x5A ^ k, 0xC3 ^ (k << 4), 0x1E ^ k, 0xA5];
        }
        Some(m)
    }

    fn write(&self, data: *mut u8) {
        for (at, mark) in self.at.iter().zip(&self.mark) {
            for (k, b) in mark[..self.bpp].iter().enumerate() {
                unsafe { core::ptr::write_volatile(data.add(at + k), *b) };
            }
        }
        unsafe { core::arch::asm!("dsb sy") };
    }

    /// How many marks are still there.
    fn left(&self, data: *const u8) -> usize {
        let same = |at: usize, mark: &[u8; 4]| mark[..self.bpp].iter().enumerate().all(|(k, b)| unsafe { core::ptr::read_volatile(data.add(at + k)) } == *b);
        self.at.iter().zip(&self.mark).filter(|(at, mark)| same(**at, mark)).count()
    }
}

/// Timeouts within `GPU_WINDOW_MS` with only trusted features on that turn them all off, and
/// then (with none on) that make the module rest longer.
const GPU_STEP_RESETS: u32 = 3;
const GPU_WINDOW_MS: u32 = 10_000;
const GPU_COOL_MS: u32 = 1000;

/// Turns the GPU features in `r.features` on and the rest off, as far as the self-test allows
/// them. Page formats and the background blit take a reload of the shown pack.
fn apply_features(r: &mut Runtime) {
    use bal2d_pack::PageFormat;
    let f = r.features;
    let uncached = f & trust::UNCACHED != 0;
    let readdress = gpu::UNCACHED.swap(uncached, Ordering::Relaxed) != uncached;
    if readdress {
        if let Some(t) = r.target.as_mut() {
            t.buffer.readdress();
        }
    }
    gpu::STREAM_OFF.store(f & trust::STREAM == 0 || !r.st_stream, Ordering::Relaxed);
    r.renderer.want_stream(!r.single, r.lobby.is_some());
    r.renderer.set_pad(f & trust::PAD != 0 && r.st_pad);
    let wide = 1 << PageFormat::Argb4444 as u32 | 1 << PageFormat::Argb8565 as u32 | 1 << PageFormat::Index8 as u32;
    let formats = if f & trust::NATIVE != 0 { r.st_formats } else { r.st_formats | wide };
    let old = gpu::BAD_FORMATS.swap(formats, Ordering::Relaxed);
    let bake = f & trust::BAKE != 0 && r.st_blit;
    // Pages and the background image keep the address they were made with: made again.
    let reload = old != formats || bake != r.bake_allowed || readdress;
    r.bake_allowed = bake;
    if reload && (r.lobby.is_some() || r.loader.is_some()) {
        RELOAD.store(true, Ordering::Release);
    }
}

/// A frame drawn without a timeout: trials start and pass (see `trust`).
fn trust_tick(r: &mut Runtime) {
    r.clean_frames += 1;
    let t = &mut r.trust;
    if t.trying != 0 {
        r.trial_frames += 1;
        if r.trial_frames >= trust::CLEAN_FRAMES {
            trace(&alloc::format!("gpu trust: {} drew {} frames clean: good", trust::names(t.trying), r.trial_frames));
            t.good |= t.trying;
            t.trying = 0;
            t.save();
            r.clean_frames = 0;
        }
        return;
    }
    if r.clean_frames < trust::CLEAN_FRAMES || r.loader.is_some() {
        return;
    }
    let Some(f) = t.next() else { return };
    // Into the file before it is on: a watch that freezes over it marks it bad when it restarts.
    t.trying = f;
    t.save();
    trace(&alloc::format!("gpu trust: trying {}", trust::names(f)));
    r.trial_frames = 0;
    r.trial_resets = 0;
    r.features |= f;
    apply_features(r);
}

/// Counts the frame's GPU timeouts (`gpu::Stats::resets`); the first few go to the log. Each
/// costs 100 ms with the LVGL thread blocked and loses part of the frame, and a few in a row
/// froze the watch: the module rests, and turns off the feature under trial, or all of them.
fn note_resets(r: &mut Runtime) {
    if r.force_timeout & r.features != 0 {
        r.renderer.stats.resets += 1;
    }
    let s = r.renderer.stats;
    if s.skipped != 0 {
        if r.skipped < 16 {
            if let Some((det, b)) = s.first_skip {
                trace(&alloc::format!("degenerate draws left out: {} in this frame; first det {det:e} bounds {b:?}", s.skipped));
            }
        }
        r.skipped += s.skipped;
    }
    note_hangs(r);
    if s.resets == 0 {
        return;
    }
    let now = lv::now_ms();
    r.gpu_resets += s.resets;
    r.clean_frames = 0;
    if r.gpu_resets <= 24 {
        let mut waits = alloc::string::String::new();
        for (at, ms) in s.long_waits.iter().take(s.resets as usize) {
            waits += &alloc::format!(" {at}:{ms}");
        }
        trace(&alloc::format!(
            "gpu timeout: {} in this frame ({} since start), waits (where:ms){waits}; calls {} batches {}; features {}",
            s.resets, r.gpu_resets, s.calls, s.batches, trust::names(r.features)
        ));
    }
    r.gpu_cool_until = now.wrapping_add(GPU_COOL_MS);
    let t = &mut r.trust;
    if t.trying != 0 {
        r.trial_resets += s.resets;
        if r.trial_resets < trust::BAD_RESETS {
            return;
        }
        trace(&alloc::format!("gpu trust: {} timed out {} times: bad", trust::names(t.trying), r.trial_resets));
        t.bad |= t.trying;
        r.features &= !t.trying;
        t.trying = 0;
        t.save();
        apply_features(r);
        return;
    }
    if now.wrapping_sub(r.gpu_window_at) > GPU_WINDOW_MS {
        r.gpu_window_at = now;
        r.gpu_window_resets = 0;
    }
    r.gpu_window_resets += s.resets;
    if r.gpu_window_resets < GPU_STEP_RESETS {
        return;
    }
    r.gpu_window_resets = 0;
    r.gpu_window_at = now;
    if r.features != 0 {
        // Trusted ones time out after all: all off, to be tried again one by one.
        trace(&alloc::format!("gpu trust: timeouts with {} on: all turned off", trust::names(r.features)));
        r.features = 0;
        t.good = 0;
        t.save();
        apply_features(r);
    } else {
        // Nothing left to turn off: a longer rest, and on (the watch is still being tested).
        trace("gpu timeouts with every feature off: resting 5 s");
        r.gpu_cool_until = now.wrapping_add(5 * GPU_COOL_MS);
    }
}

/// The draws an earlier build left out for hanging the GPU (removed: they are drawn again).
const BANS_FILE: &str = "gpu-banned.bin";

/// The draws of submits that timed out this frame, and culprits found, to the log (and bans to
/// the file).
fn note_hangs(r: &mut Runtime) {
    let h = &mut r.renderer.hangs;
    if r.fake_auto && h.fake == 0 && r.perf.frames > 300 && !h.seen.is_empty() {
        h.fake = h.seen[h.seen.len() / 2];
        trace(&alloc::format!("dev: faking hangs on draw {:08x}", h.fake));
    }
    if !h.hung.is_empty() {
        let hung = core::mem::take(&mut h.hung);
        if r.hang_dumps < 8 {
            r.hang_dumps += 1;
            trace(&alloc::format!("hung submit(s): {} draws; sig/page/blend/len/bounds/det:", hung.len()));
            for chunk in hung.chunks(5) {
                let mut line = alloc::string::String::from(" ");
                for d in chunk {
                    let b = d.bounds;
                    line += &alloc::format!(" {:08x}/{}/{}/{}/{},{},{},{}/{:.3e}", d.sig, d.page, d.blend, d.len, b[0], b[1], b[2], b[3], d.det);
                }
                trace(&line);
            }
        }
    }
}

/// Clipping baked shapes to what stays visible saves GPU fill and costs CPU; which wins depends
/// on which of the two the watch waits for. Once a lobby plays nothing but its bakes, one-second
/// windows alternate it off and on (`WINDOWS` with frames of only baked layers), then the one
/// with the shorter draw (and GPU wait) per frame stays for that lobby.
pub struct ClipAb {
    /// Draw + GPU-wait milliseconds and frames, clipping off and on.
    sum: [u32; 2],
    frames: [u32; 2],
    windows: u32,
    done: bool,
}

impl ClipAb {
    const WINDOWS: u32 = 12;

    const fn new() -> ClipAb {
        ClipAb { sum: [0; 2], frames: [0; 2], windows: 0, done: false }
    }
}

/// A drawn frame for the clipping trial.
fn clip_ab_frame(r: &mut Runtime, cost: u32) {
    let Some(lobby) = r.lobby.as_ref() else { return };
    let all = (1u32 << lobby.layers.len().min(31)) - 1;
    if r.clip_ab.done || r.baked_mask & all != all {
        return;
    }
    let k = lobby.idle_clip as usize;
    r.clip_ab.sum[k] += cost;
    r.clip_ab.frames[k] += 1;
}

/// A stats window ended: the trial switches sides, or decides.
fn clip_ab_window(r: &mut Runtime) {
    let Some(lobby) = r.lobby.as_mut() else { return };
    if r.clip_ab.done {
        return;
    }
    if !lobby.idle_clips() {
        r.clip_ab.done = true;
        return;
    }
    let a = &mut r.clip_ab;
    let k = lobby.idle_clip as usize;
    // Only windows that drew baked frames count; the first one starts the trial (off).
    if a.frames[k] == 0 && a.windows == 0 {
        lobby.idle_clip = false;
        return;
    }
    if a.frames[k] > 0 {
        a.windows += 1;
        lobby.idle_clip = !lobby.idle_clip;
    }
    if a.windows >= ClipAb::WINDOWS && a.frames[0] > 0 && a.frames[1] > 0 {
        let (off, on) = (a.sum[0] * 10 / a.frames[0], a.sum[1] * 10 / a.frames[1]);
        a.done = true;
        // Clipping must win by a little (0.3 ms): the windows fall on different moments of the
        // loops, some heavier than others.
        lobby.idle_clip = on + 3 < off;
        trace(&alloc::format!("idle clip: off {}.{} ms, on {}.{} ms a frame: {}", off / 10, off % 10, on / 10, on % 10, if lobby.idle_clip { "on" } else { "off" }));
    }
}

/// Adds a drawn frame's cost to the stats window (`draw`: building and submitting, `gpu`:
/// waiting for the GPU to finish).
fn count_draw(r: &mut Runtime, draw: u32, gpu: u32) {
    clip_ab_frame(r, draw + gpu);
    let s = r.renderer.stats;
    if s.resets == 0 {
        trust_tick(r);
    }
    let w = &mut r.window;
    w.drawn += 1;
    w.draw_ms += draw;
    w.gpu_ms += gpu;
    w.submit_ms += s.submit_ms;
    w.flush_ms += s.flush_ms;
    // Triangles drawn (on screen), not the whole draw list.
    w.triangles = s.triangles;
    w.calls = s.calls;
    w.errors += s.errors;
    if s.errors != 0 {
        w.last_error = s.last_error;
    }
}

/// Draws the frame into the module's buffer (LVGL copies it to the screen) and waits for it.
/// Returns the time drawing and the time waiting for the GPU (ms).
fn draw_own(r: &mut Runtime, rect: (i32, i32, i32, i32)) -> (u32, u32) {
    let Some(mut target) = r.target.take() else { return (0, 0) };
    let t1 = lv::now_ms();
    let (w, h) = (target.buffer.width as i32, target.buffer.height as i32);
    render(r, &mut target.buffer, (0, 0), [0, 0, w, h], rect);
    let t2 = lv::now_ms();
    r.renderer.finish();
    note_resets(r);
    // Whatever reads the frame with the CPU (LVGL's software path) must not see cached lines of
    // an earlier one: the GPU writes memory behind the cache.
    fw::cache_flush_invalidate(target.pixels, target.buffer.stride as usize * target.buffer.height as usize);
    let t3 = lv::now_ms();
    r.target = Some(target);
    (t2.wrapping_sub(t1), t3.wrapping_sub(t2))
}

/// Draws the lobby as it is now into `target`: the layer framed at `rect` (pack screen
/// pixels) has its top-left corner at target pixel `offset`, and only `clip` (target pixels:
/// x, y, right, bottom) is drawn.
fn render(r: &mut Runtime, target: &mut gpu::VgBuffer, offset: (i32, i32), clip: [i32; 4], rect: (i32, i32, i32, i32)) {
    let Runtime { lobby, pages, bake, renderer, list, baked_mask, bake_active, fx, .. } = r;
    let Some(lobby) = lobby.as_mut() else { return };
    let (w, h) = (rect.2, rect.3);
    let (fw_w, fw_h) = (lobby.width, lobby.height);
    // The baked bottom replaces its slots (and the clear, when it covers the whole layer).
    let bake = bake.as_mut().filter(|_| lobby.bake_active());
    *bake_active = bake.is_some();
    let covers = bake.as_ref().is_some_and(|b| {
        b.x <= rect.0 && b.y <= rect.1 && b.x + b.buffer.width as i32 >= rect.0 + w && b.y + b.buffer.height as i32 >= rect.1 + h
    });
    let clear = [clip[0], clip[1], clip[2] - clip[0], clip[3] - clip[1]];
    renderer.begin(target, offset, clip, (!covers).then_some((clear, 0xFF00_0000)));
    if let Some(b) = bake {
        renderer.draw_bake(target, b, (rect.0, rect.1), clip);
        // The GPU copies the background while the CPU builds the paths.
        renderer.flush();
    }
    // The pack frames the whole screen; the layer shows its own rectangle of it.
    let cam = lobby.camera;
    let (ox, oy) = (rect.0 as f32, rect.1 as f32);
    let cx = cam.cx + (ox + w as f32 * 0.5 - fw_w * 0.5) / cam.scale;
    let cy = cam.cy - (oy + h as f32 * 0.5 - fw_h * 0.5) / cam.scale;
    let size = (w as f32, h as f32);
    // A partial redraw: baked calls outside it build no path (a pixel of margin).
    let view = [(clip[0] - offset.0 - 1) as f32, (clip[1] - offset.1 - 1) as f32, (clip[2] - offset.0 + 1) as f32, (clip[3] - offset.1 + 1) as f32];
    lobby.idle_view = (view[0] > 0.0 || view[1] > 0.0 || view[2] < size.0 || view[3] < size.1).then_some(view);
    for li in 0..lobby.layers.len() {
        let Some(pages) = pages.get_mut(li) else { break };
        renderer.hangs.layer = li as u32;
        if *baked_mask & (1 << li.min(31)) != 0 {
            renderer.draw_calls(target, pages, |sizes, emit| lobby.draw_idle(li, (ox, oy), size, sizes, emit));
            continue;
        }
        lobby.draw_list(li, list);
        renderer.draw(target, size, li, list, pages, (cx, cy, cam.scale));
    }
    renderer.hangs.layer = 7;
    fx.draw(renderer, target);
}

/// LVGL draws the view (its DRAW_MAIN, after the image's own, which draws nothing in direct
/// mode): the frame goes straight into LVGL's layer, in the order LVGL draws (the watchface
/// under the layer is already in the GPU's queue, what is above comes after).
pub fn on_draw_main(e: *mut c_void) {
    let r = rt();
    if !r.direct || r.lobby.is_none() {
        return;
    }
    // Resting after GPU timeouts: nothing touches the GPU (the layer keeps what it showed).
    if (r.gpu_cool_until.wrapping_sub(lv::now_ms()) as i32) > 0 {
        return;
    }
    let v = view::view();
    // A layer without a buffer yet (an ancestor fading is drawn into a layer of its own, whose
    // buffer comes with its first task) or of another format: nothing drawn this time, through
    // the module's buffer for a while (LVGL draws that as an image), for good after a few.
    let fail = |r: &mut Runtime| {
        r.direct_failures += 1;
        r.direct_moved_at = lv::now_ms();
    };
    let Some(layer) = (unsafe { lv::draw_layer(e) }) else {
        fail(r);
        return;
    };
    let format = match layer.cf {
        lv::cf::ARGB8888 => gpu::VG_FMT_BGRA8888,
        lv::cf::XRGB8888 => gpu::VG_FMT_BGRX8888,
        lv::cf::RGB565 => gpu::VG_FMT_BGR565,
        lv::cf::RGB888 => gpu::VG_FMT_BGR888,
        _ => {
            fail(r);
            return;
        }
    };
    let rect = v.rect;
    let (x, y, _, _) = unsafe { lv::measured(v.obj) };
    if (x, y) != (rect.0, rect.1) {
        // Scrolling: drawn right this time, through the buffer from the next frame on.
        r.direct_moved_at = lv::now_ms();
    }
    let offset = (x - layer.area[0], y - layer.area[1]);
    // LVGL draws this area from the layer up (it answered COVER for it): nothing under it is
    // drawn in this pass, so what the frame drew there earlier still stands. LVGL refreshes the
    // screen in parts (two halves): the first part draws the whole layer, the others skip it.
    let c = layer.clip;
    let from_here = v.covered_area.take().is_some_and(|a| c[0] >= a[0] && c[1] >= a[1] && c[2] <= a[2] && c[3] <= a[3]);
    if !r.direct_pending && r.direct_whole {
        let again = r.direct_passed.iter().any(|p| c[0] <= p[2] && p[0] <= c[2] && c[1] <= p[3] && p[1] <= c[3]);
        if from_here && !again && r.direct_passed.len() < 8 {
            r.direct_passed.push(c);
            return;
        }
        r.direct_whole = false;
    }
    let whole = r.direct_pending && from_here;
    let area = if whole { [i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2] } else { layer.clip };
    let clip = [
        (area[0] - layer.area[0]).max(offset.0).max(0),
        (area[1] - layer.area[1]).max(offset.1).max(0),
        (area[2] + 1 - layer.area[0]).min(offset.0 + rect.2).min(layer.width as i32),
        (area[3] + 1 - layer.area[1]).min(offset.1 + rect.3).min(layer.height as i32),
    ];
    if r.direct_traced < 24 {
        r.direct_traced += 1;
        trace(&alloc::format!(
            "direct draw: area {:?} clip {:?} cf {:#x} stride {} obj ({x}, {y}) pending {} at {}",
            layer.area,
            layer.clip,
            layer.cf,
            layer.stride,
            r.direct_pending,
            lv::now_ms()
        ));
    }
    if clip[2] <= clip[0] || clip[3] <= clip[1] {
        return;
    }
    let mut buffer = match r.layer_buffer.take() {
        Some(mut b) => {
            b.rebind(layer.data, layer.width, layer.height, layer.stride, format);
            b
        }
        None => gpu::VgBuffer::wrap(layer.data, layer.width, layer.height, layer.stride, format),
    };
    let marks = if r.direct_checked < DIRECT_CHECKS { Marks::new(&layer, clip) } else { None };
    if let Some(m) = &marks {
        m.write(layer.data);
    }
    // The screen's layer is in SRAM, which no cache sits in front of. A layer of its own (an
    // ancestor fading or zooming) is allocated on the heap in cached PSRAM: what the CPU wrote
    // there (LVGL's software drawing, the marks) is pushed out first, so later evictions
    // cannot land on the GPU's output, and the lines are dropped again after it, so the CPU
    // reads what the GPU wrote. Mirage has no cache: only the real watch needs this.
    let rows = (layer.data as usize + clip[1] as usize * layer.stride as usize, (clip[3] - clip[1]) as usize * layer.stride as usize);
    let cached = gpu::uncached(layer.data as usize).is_some();
    if cached {
        fw::cache_flush_invalidate(rows.0 as *const u8, rows.1);
    }
    let t1 = lv::now_ms();
    render(r, &mut buffer, offset, clip, rect);
    r.renderer.end_scissor(&buffer);
    let t2 = lv::now_ms();
    // Done before LVGL goes on: what it draws above comes after in the GPU's queue anyway, but
    // it must not flush the screen with the frame half drawn.
    r.renderer.finish();
    note_resets(r);
    if cached {
        fw::cache_flush_invalidate(rows.0 as *const u8, rows.1);
    }
    let t3 = lv::now_ms();
    if let Some(m) = &marks {
        if m.left(layer.data) == m.at.len() {
            r.direct_broken = true;
            trace(&alloc::format!("direct check: the GPU's frame is not in LVGL's layer at {:#x} (cf {:#x}): own buffer from now on", layer.data as usize, layer.cf));
        } else {
            r.direct_checked += 1;
            if r.direct_checked == DIRECT_CHECKS {
                trace(&alloc::format!("direct check: {DIRECT_CHECKS} frames read back fine (layer {:#x})", layer.data as usize));
            }
        }
    }
    r.layer_buffer = Some(buffer);
    r.direct_failures = 0;
    if r.direct_pending {
        r.direct_pending = false;
        r.direct_whole = whole;
        r.direct_passed.clear();
        r.direct_passed.push(c);
        r.direct_frame_ms = t3.wrapping_sub(t1);
        count_draw(r, t2.wrapping_sub(t1), t3.wrapping_sub(t2));
    } else {
        r.window.redraws += 1;
        r.window.redraw_ms += t3.wrapping_sub(t1);
    }
}

//! The native app ("BA 大厅" in the launcher): one page with the installed characters (tap one
//! to show it), the drawing options and the status. Stage 1 registers the page with the
//! Supervisor; stage 2 (on the UI owner) starts the player.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use canopus_abi::{AppEntryV1, ContextV1};
use canopus_runtime::launcher;
use canopus_target_private::*;

use super::lv::{self, Obj};

pub const PAGE_STATUS: usize = 0;
pub const PAGE_COUNT: usize = 1;

const DISPLAY_NAME: &[u8] = "BA 大厅\0".as_bytes();
pub const ICON_PATH: &[u8] = b"/data/canopus/m.ba_l2d/appicon.bin\0";
const PAGE_NAMES: [&[u8]; PAGE_COUNT] = [b"bal2d_status\0"];

static APP_ID: AtomicU32 = AtomicU32::new(0);
static APP_READY: AtomicU32 = AtomicU32::new(0);

static mut PAGE_DESCRIPTORS: core::mem::MaybeUninit<[firmware_page_descriptor; PAGE_COUNT]> = core::mem::MaybeUninit::uninit();

/// Layout (Band 10 Pro, 336 x 480): side margin, rows of the character list and option buttons.
const SIDE: i32 = 24;
const ROW_H: i32 = 64;
const ROW_STEP: i32 = 74;
const LIST_TOP: i32 = 116;
/// Row colours: another character, the one shown, the one loading; the option buttons.
const ROW_BG: u32 = 0x1C_2433;
const ROW_CURRENT: u32 = 0x2A_4A8C;
const ROW_LOADING: u32 = 0x3D_68C2;
const OPTION_BG: u32 = 0x26_2B36;
const MUTED: u32 = 0xB8_C4D8;

/// A character in the list: its row, the row's label, its pack file and name, and what the row
/// shows (colour and text), rewritten only when that changes.
struct Row {
    obj: Obj,
    label: Obj,
    file: String,
    name: String,
    shown: (u32, String),
}

/// The page's objects (LVGL deletes them with the page).
struct Ui {
    column: Obj,
    title: Obj,
    hint: Obj,
    rows: Vec<Row>,
    files: Vec<String>,
    fluent: (Obj, Obj),
    direct: (Obj, Obj),
    status: Obj,
}

static mut UI: Option<Ui> = None;

fn ui() -> Option<&'static mut Ui> {
    unsafe { (*core::ptr::addr_of_mut!(UI)).as_mut() }
}

pub fn app_id() -> u32 {
    APP_ID.load(Ordering::Acquire)
}

fn descriptor_ptr(index: usize) -> *mut firmware_page_descriptor {
    unsafe { core::ptr::addr_of_mut!(PAGE_DESCRIPTORS).cast::<firmware_page_descriptor>().add(index) }
}

fn descriptor_init(index: usize) {
    let d = descriptor_ptr(index);
    unsafe {
        core::ptr::write_bytes(d.cast::<u8>(), 0, core::mem::size_of::<firmware_page_descriptor>());
        (*d).page_name = PAGE_NAMES[index].as_ptr() as *mut core::ffi::c_void;
        (*d).page_id = index as u16;
        (*d).on_signal = on_signal as *const () as *mut core::ffi::c_void;
        (*d).on_create = on_create as *const () as *mut core::ffi::c_void;
        (*d).on_resume = on_resume as *const () as *mut core::ffi::c_void;
        // Band 10 Pro: +0x58 START (right after create), +0x5c PAUSE, +0x60 STOP, +0x64 UI_DESTROY.
        (*d).on_pause = on_pause as *const () as *mut core::ffi::c_void;
        (*d).on_ui_destroy = on_destroy as *const () as *mut core::ffi::c_void;
    }
}

// The firmware dispatches by offset: the SDK's names must stay on these slots.
const _: () = assert!(core::mem::offset_of!(firmware_page_descriptor, on_pause) == 0x5c);
const _: () = assert!(core::mem::offset_of!(firmware_page_descriptor, on_ui_destroy) == 0x64);

/// # Safety
/// Called by the Supervisor on the serialized UI owner with the live context.
pub unsafe fn publish_stage(ctx: *const ContextV1, stage: u32) -> Result<(), i32> {
    match stage {
        1 => {
            if APP_ID.load(Ordering::Acquire) != 0 {
                return Ok(());
            }
            for index in 0..PAGE_COUNT {
                descriptor_init(index);
            }
            let pages: [*mut core::ffi::c_void; PAGE_COUNT] = core::array::from_fn(|i| descriptor_ptr(i).cast());
            let entry = AppEntryV1 {
                struct_size: core::mem::size_of::<AppEntryV1>() as u32,
                label: DISPLAY_NAME.as_ptr().cast(),
                icon: ICON_PATH.as_ptr().cast(),
                pages: pages.as_ptr(),
                page_count: PAGE_COUNT as u32,
            };
            let id = unsafe { launcher::register_app(ctx.cast(), &entry) }?;
            APP_ID.store(u32::from(id), Ordering::Release);
            Ok(())
        }
        2 => {
            if APP_ID.load(Ordering::Acquire) == 0 {
                return Err(-1);
            }
            APP_READY.store(1, Ordering::Release);
            match super::start() {
                0 => Ok(()),
                e => Err(e),
            }
        }
        _ => Err(-22),
    }
}

extern "C" fn on_signal(_page: *mut firmware_page_descriptor, _event: u32, _payload: *mut core::ffi::c_void) -> i32 {
    0
}

/// A rounded box with a label (centred, or left-aligned for list rows); `CLICKED` calls `action`
/// with `cookie`. Returns the box and the label.
unsafe fn button(parent: Obj, y: i32, w: i32, bg: u32, centred: bool, action: extern "C" fn(*mut core::ffi::c_void), cookie: usize) -> (Obj, Obj) {
    let b = unsafe { lv::container(parent, SIDE, y, w - 2 * SIDE, ROW_H) };
    if b.is_null() {
        return (b, b);
    }
    unsafe {
        lv::style(b, &[(lv::prop::BG_COLOR, bg), (lv::prop::BG_OPA, 255), (lv::prop::RADIUS, 32)]);
        lv::clear_flag(b, lv::flag::SCROLLABLE);
        lv::add_flag(b, lv::flag::CLICKABLE);
        lv::on_event(b, action, lv::event::CLICKED, cookie);
    }
    let (x, align) = if centred { (0, lv::text_align::CENTER) } else { (24, lv::text_align::LEFT) };
    let l = unsafe { lv::label(b, x, 14, w - 2 * SIDE - 2 * x, false, 0xFFFFFF, align, lv::LONG_DOT) };
    (b, l)
}

extern "C" fn row_clicked(e: *mut core::ffi::c_void) {
    let index = unsafe { lvx_event_get_user_data(e) };
    let Some(file) = ui().and_then(|u| u.rows.get(index)).map(|r| r.file.clone()) else { return };
    super::choose_pack(&file);
    refresh();
}

extern "C" fn toggle_fluent(_e: *mut core::ffi::c_void) {
    super::toggle_fluent();
    refresh();
}

extern "C" fn toggle_direct(_e: *mut core::ffi::c_void) {
    super::toggle_direct();
    refresh();
}

extern "C" fn on_create(_page: *mut firmware_page_descriptor, root: *mut core::ffi::c_void, _data: *mut core::ffi::c_void) -> i32 {
    super::start();
    let (w, h) = super::rt().screen;
    unsafe {
        // One scrolling column: the title, the characters, the options, the status (which grows
        // with errors and hints).
        let column = lv::container(root, 0, 0, w, h);
        if column.is_null() {
            return -1;
        }
        lvx_object_set_scroll_dir(column, SCROLL_VERTICAL);
        lvx_object_set_scrollbar_mode(column, SCROLLBAR_OFF);
        lv::quiet_scroll_bar(column);
        lv::clear_flag(column, lv::flag::CLICKABLE);
        let title = lv::label(column, SIDE, 24, w - 2 * SIDE, true, 0xFFFFFF, lv::text_align::LEFT, lv::LONG_DOT);
        let hint = lv::label(column, SIDE, 76, w - 2 * SIDE, false, MUTED, lv::text_align::LEFT, lv::LONG_DOT);
        let fluent = button(column, LIST_TOP, w, OPTION_BG, true, toggle_fluent, 0);
        let direct = button(column, LIST_TOP + ROW_STEP, w, OPTION_BG, true, toggle_direct, 0);
        let status = lv::label(column, SIDE, LIST_TOP + 2 * ROW_STEP + 8, w - 2 * SIDE, false, MUTED, lv::text_align::LEFT, lv::LONG_WRAP);
        *core::ptr::addr_of_mut!(UI) = Some(Ui { column, title, hint, rows: Vec::new(), files: Vec::new(), fluent, direct, status });
    }
    refresh();
    0
}

/// `LV_DIR_VER` and `LV_SCROLLBAR_MODE_OFF`.
const SCROLL_VERTICAL: u32 = 12;
const SCROLLBAR_OFF: u32 = 0;

extern "C" fn on_resume(_page: *mut firmware_page_descriptor) -> i32 {
    refresh();
    0
}

extern "C" fn on_pause(_page: *mut firmware_page_descriptor) -> i32 {
    0
}

extern "C" fn on_destroy(_page: *mut firmware_page_descriptor) -> i32 {
    // The objects went with the page.
    unsafe { *core::ptr::addr_of_mut!(UI) = None };
    0
}

/// Makes the list's rows anew for `files` and moves the options and the status below them.
fn rebuild(ui: &mut Ui, files: Vec<String>) {
    let (w, _) = super::rt().screen;
    for row in ui.rows.drain(..) {
        unsafe { lv::delete(row.obj) };
    }
    for (i, file) in files.iter().enumerate() {
        let (obj, label) = unsafe { button(ui.column, LIST_TOP + i as i32 * ROW_STEP, w, ROW_BG, false, row_clicked, i) };
        if obj.is_null() {
            break;
        }
        let name = super::connect::pack_name(file);
        ui.rows.push(Row { obj, label, file: file.clone(), name, shown: (0, String::new()) });
    }
    let below = LIST_TOP + ui.rows.len() as i32 * ROW_STEP + if ui.rows.is_empty() { 0 } else { 12 };
    unsafe {
        lv::place(ui.fluent.0, SIDE, below);
        lv::place(ui.direct.0, SIDE, below + ROW_STEP);
        lv::place(ui.status, SIDE, below + 2 * ROW_STEP + 8);
    }
    ui.files = files;
}

/// Rewrites the page (called from the frame timer about once a second, and after a tap).
pub fn refresh() {
    let Some(ui) = ui() else { return };
    let files = super::connect::pack_files();
    if files != ui.files {
        rebuild(ui, files);
    }
    let r = super::rt();
    let switching = super::switching();
    let shown = r.lobby.as_ref().map(|_| r.pack.clone());
    let current = super::pick_pack();
    let failed = r.boot_error != 0 && switching.is_none();
    for row in &mut ui.rows {
        let file = Some(&row.file);
        let (bg, state) = if switching.as_ref() == file {
            (ROW_LOADING, "  加载中…")
        } else if shown.as_ref() == file {
            (ROW_CURRENT, "")
        } else if failed && current.as_ref() == file {
            (ROW_CURRENT, "  加载失败")
        } else {
            (ROW_BG, "")
        };
        let text = alloc::format!("{}{state}", row.name);
        if row.shown.0 != bg || row.shown.1 != text {
            unsafe {
                lv::set_style(row.obj, lv::prop::BG_COLOR, bg);
                lv::set_text(row.label, &text);
            }
            row.shown = (bg, text);
        }
    }
    let title = match &switching {
        Some(f) => alloc::format!("正在加载 {}", super::connect::pack_name(f)),
        None => super::pack_title(),
    };
    let hint = if ui.rows.is_empty() { "还没有角色" } else { "点一个角色来显示它" };
    unsafe {
        lv::set_text(ui.title, &title);
        lv::set_text(ui.hint, hint);
        lv::set_text(ui.fluent.1, if super::fluent_on() { "流畅优先：开" } else { "流畅优先：关" });
        lv::set_text(ui.direct.1, if super::direct_on() { "直接绘制：开" } else { "直接绘制：关" });
        lv::set_text(ui.status, &super::status_text());
    }
}

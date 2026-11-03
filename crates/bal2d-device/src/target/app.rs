//! The native app ("BA 大厅" in the launcher): one status page. Stage 1 registers the page with
//! the Supervisor; stage 2 (on the UI owner) starts the player.

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
static mut STATUS_LABEL: Obj = lv::NULL;
static mut NAME_LABEL: Obj = lv::NULL;
static mut OVERLAY_LABEL: Obj = lv::NULL;
static mut FLUENT_LABEL: Obj = lv::NULL;
static mut DIRECT_LABEL: Obj = lv::NULL;

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

/// A rounded button with a centred label; `CLICKED` calls `action`.
unsafe fn button(parent: Obj, y: i32, w: i32, text: &str, action: extern "C" fn(*mut core::ffi::c_void)) -> Obj {
    let b = unsafe { lv::container(parent, 24, y, w - 48, 64) };
    if b.is_null() {
        return b;
    }
    unsafe {
        lv::style(b, &[(lv::prop::BG_COLOR, 0x2A_4A8C), (lv::prop::BG_OPA, 255), (lv::prop::RADIUS, 32)]);
        lv::add_flag(b, lv::flag::CLICKABLE);
        lv::on_event(b, action, lv::event::CLICKED, 0);
    }
    let l = unsafe { lv::label(b, 0, 14, w - 48, false, 0xFFFFFF, lv::text_align::CENTER, lv::LONG_DOT) };
    unsafe { lv::set_text(l, text) };
    l
}

extern "C" fn next_pack(_e: *mut core::ffi::c_void) {
    super::next_pack();
    refresh();
}

extern "C" fn toggle_overlay(_e: *mut core::ffi::c_void) {
    super::toggle_overlay();
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
    let (w, _h) = super::rt().screen;
    unsafe {
        // Title and buttons at fixed places; the status (which grows with errors and hints) below.
        NAME_LABEL = lv::label(root, 24, 24, w - 48, true, 0xFFFFFF, lv::text_align::LEFT, lv::LONG_DOT);
        button(root, 80, w, "下一个角色", next_pack);
        OVERLAY_LABEL = button(root, 156, w, "", toggle_overlay);
        FLUENT_LABEL = button(root, 232, w, "", toggle_fluent);
        DIRECT_LABEL = button(root, 308, w, "", toggle_direct);
        STATUS_LABEL = lv::label(root, 24, 388, w - 48, false, 0xB8C4D8, lv::text_align::LEFT, lv::LONG_WRAP);
    }
    refresh();
    0
}

extern "C" fn on_resume(_page: *mut firmware_page_descriptor) -> i32 {
    refresh();
    0
}

extern "C" fn on_pause(_page: *mut firmware_page_descriptor) -> i32 {
    0
}

extern "C" fn on_destroy(_page: *mut firmware_page_descriptor) -> i32 {
    unsafe {
        STATUS_LABEL = lv::NULL;
        NAME_LABEL = lv::NULL;
        OVERLAY_LABEL = lv::NULL;
        FLUENT_LABEL = lv::NULL;
        DIRECT_LABEL = lv::NULL;
    }
    0
}

/// Rewrites the status text (called from the frame timer about once a second).
pub fn refresh() {
    let label = unsafe { STATUS_LABEL };
    if label.is_null() {
        return;
    }
    let text = super::status_text();
    unsafe {
        lv::set_text(label, &text);
        lv::set_text(NAME_LABEL, &super::pack_title());
        lv::set_text(OVERLAY_LABEL, if super::overlay_on() { "全屏显示：开" } else { "全屏显示：关" });
        lv::set_text(FLUENT_LABEL, if super::fluent_on() { "流畅优先：开" } else { "流畅优先：关" });
        lv::set_text(DIRECT_LABEL, if super::direct_on() { "直接绘制：开" } else { "直接绘制：关" });
    }
}

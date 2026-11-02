//! Firmware entry points this module binds itself (docs/re/firmware-p0.md), listed in
//! `Canopus.toml`'s `allowed_constants`. Everything else comes from the target packs.
//!
//! The firmware is built hard-float: only functions without floating-point arguments are called
//! (matrices and paths are written as structs).

use core::ffi::c_void;

use canopus_target_generated as fwgen;

/// Xiaomi Smart Band 10 Pro 3.101.043.
mod addr {
    // Flash veneers into the driver's SRAM execution copy.
    pub const VG_FINISH: usize = 0x0C18_7BB1;
    pub const VG_FLUSH: usize = 0x0C18_77E9;
    pub const VG_CLEAR: usize = 0x0C18_7AB1;
    pub const VG_BLIT_RECT: usize = 0x0C18_7A59;
    pub const VG_DRAW_PATTERN: usize = 0x0C18_7F01;
    pub const VG_SET_SCISSOR: usize = 0x0C18_76F9;
    pub const VG_ENABLE_SCISSOR: usize = 0x0C18_7A49;
    pub const VG_SET_CLUT: usize = 0x0C18_7D21;
    pub const CACHE_FLUSH_INVALIDATE: usize = 0x0C58_8449;
    pub const WFIMG_CLASS: usize = 0x2CDB_DC40;
    /// `disp_def` (the literal in `lv_display_get_layer_top`: 0x20103174 + 0x18).
    pub const DISPLAY_DEFAULT: usize = 0x2010_318C;
    pub const MALLINFO: usize = 0x0C1F_8029;
    pub const USER_HEAP: usize = 0x200F_4C28;
}

pub use addr::*;

macro_rules! call {
    ($addr:expr, fn($($t:ty),*) -> $r:ty, $($a:expr),*) => {{
        let f: extern "C" fn($($t),*) -> $r = unsafe { core::mem::transmute($addr as usize) };
        f($($a),*)
    }};
}

pub fn vg_finish() -> i32 {
    call!(VG_FINISH, fn() -> i32,)
}

/// Submits the commands so far without waiting (the GPU works while the CPU goes on).
pub fn vg_flush() -> i32 {
    call!(VG_FLUSH, fn() -> i32,)
}

pub fn vg_clear(target: *mut u8, rect: *const i32, color: u32) -> i32 {
    call!(VG_CLEAR, fn(*mut u8, *const i32, u32) -> i32, target, rect, color)
}

#[allow(clippy::too_many_arguments)]
pub fn vg_blit_rect(target: *mut u8, source: *mut u8, rect: *const i32, matrix: *const f32, blend: u32, color: u32, filter: u32) -> i32 {
    call!(VG_BLIT_RECT, fn(*mut u8, *mut u8, *const i32, *const f32, u32, u32, u32) -> i32, target, source, rect, matrix, blend, color, filter)
}

#[allow(clippy::too_many_arguments)]
pub fn vg_draw_pattern(
    target: *mut u8,
    path: *mut u8,
    fill_rule: u32,
    path_matrix: *const f32,
    image: *mut u8,
    image_matrix: *const f32,
    blend: u32,
    pattern_mode: u32,
    pattern_color: u32,
    color: u32,
    filter: u32,
) -> i32 {
    call!(
        VG_DRAW_PATTERN,
        fn(*mut u8, *mut u8, u32, *const f32, *mut u8, *const f32, u32, u32, u32, u32, u32) -> i32,
        target,
        path,
        fill_rule,
        path_matrix,
        image,
        image_matrix,
        blend,
        pattern_mode,
        pattern_color,
        color,
        filter
    )
}

pub fn vg_set_scissor(x: i32, y: i32, right: i32, bottom: i32) -> i32 {
    call!(VG_SET_SCISSOR, fn(i32, i32, i32, i32) -> i32, x, y, right, bottom)
}

pub fn vg_enable_scissor() -> i32 {
    call!(VG_ENABLE_SCISSOR, fn() -> i32,)
}

pub fn vg_set_clut(count: u32, colors: *const u32) -> i32 {
    call!(VG_SET_CLUT, fn(u32, *const u32) -> i32, count, colors)
}

/// Makes CPU writes to `[addr, addr + len)` visible to the GPU.
/// Writes back and invalidates the PSRAM data cache over [addr, addr + len): later CPU reads
/// fetch what the GPU wrote (above 32 KB the firmware does the whole cache). Flash veneer of the
/// firmware's `hal_cache` sync-and-invalidate (RAM 0x0029D36C), next to `cache_clean`'s.
pub fn cache_flush_invalidate(addr: *const u8, len: usize) {
    unsafe { core::arch::asm!("dsb sy") };
    call!(CACHE_FLUSH_INVALIDATE, fn(u32, *const u8, u32) -> i32, 1, addr, len as u32);
    unsafe { core::arch::asm!("dsb sy", "isb sy") };
}

pub fn cache_clean(addr: *const u8, len: usize) {
    unsafe { core::arch::asm!("dsb sy") };
    call!(fwgen::CANOPUS_FW_P67_BLUR_CACHE_CLEAN_CALLABLE, fn(u32, *const u8, u32) -> i32, 1, addr, len as u32);
    unsafe { core::arch::asm!("dsb sy", "isb sy") };
}

/// (free bytes, largest free block) of the heap `malloc` uses. Walks the heap: call sparingly.
/// The firmware asserts (resets) when `malloc` fails, so large allocations check this first.
pub fn heap_info() -> (u32, u32) {
    let heap = unsafe { core::ptr::read_volatile(USER_HEAP as *const *mut c_void) };
    if heap.is_null() {
        return (0, 0);
    }
    let mut info = [0u32; 7];
    call!(MALLINFO, fn(*mut u32, *mut c_void) -> *mut u32, info.as_mut_ptr(), heap);
    (info[5], info[3])
}

/// Screen coordinates of the input device behind an LVGL event.
pub fn event_point(event: *mut c_void) -> Option<(i32, i32)> {
    let get_indev: extern "C" fn(*mut c_void) -> *mut c_void = unsafe { core::mem::transmute(fwgen::CANOPUS_FW_LV_EVENT_GET_INDEV_CALLABLE) };
    let indev = get_indev(event);
    if indev.is_null() {
        return None;
    }
    let mut point = [0i32; 2];
    let get_point: extern "C" fn(*mut c_void, *mut i32) = unsafe { core::mem::transmute(fwgen::CANOPUS_FW_LV_INDEV_GET_POINT_CALLABLE) };
    get_point(indev, point.as_mut_ptr());
    Some((point[0], point[1]))
}

/// `lv_obj_tree_walk(start, cb, user_data)`; the callback returns 0 to continue, 1 to skip
/// children, 2 to stop.
pub fn tree_walk(start: *mut c_void, cb: extern "C" fn(*mut c_void, *mut c_void) -> u32, user: *mut c_void) {
    call!(
        fwgen::CANOPUS_FW_LV_OBJ_TREE_WALK_CALLABLE,
        fn(*mut c_void, extern "C" fn(*mut c_void, *mut c_void) -> u32, *mut c_void) -> (),
        start,
        cb,
        user
    )
}

/// `lv_image_class`: the base class of `wfimg` (`lv_obj_class_t::base_class` is its first field).
pub fn image_class() -> usize {
    unsafe { core::ptr::read_volatile(WFIMG_CLASS as *const usize) }
}

/// `lv_display_t::act_scr` (LVGL 9.0: `screens, act_scr, prev_scr, scr_to_load, bottom_layer,
/// top_layer`; `top_layer` is at +0x2C8 on both firmwares).
const DISPLAY_ACT_SCR: usize = 0x2B8;

/// The active screen of the default display: the watchface lives below it while it is shown.
pub fn active_screen() -> *mut c_void {
    unsafe {
        let disp = core::ptr::read_volatile(DISPLAY_DEFAULT as *const *const u8);
        if disp.is_null() {
            return core::ptr::null_mut();
        }
        core::ptr::read_volatile(disp.add(DISPLAY_ACT_SCR) as *const *mut c_void)
    }
}

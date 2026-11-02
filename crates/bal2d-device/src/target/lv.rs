//! Thin helpers over the firmware's LVGL v9.0 entry points (style ids, flags and event codes
//! are LVGL 9.0's; the same values Lyra Player and the Supervisor's UI host use).

use alloc::vec::Vec;
use core::ffi::c_void;

use canopus_target_private::*;


pub type Obj = *mut c_void;
pub const NULL: Obj = core::ptr::null_mut();

pub mod flag {
    pub const HIDDEN: u32 = 1 << 0;
    pub const CLICKABLE: u32 = 1 << 1;
    pub const CLICK_FOCUSABLE: u32 = 1 << 2;
    pub const SCROLLABLE: u32 = 1 << 4;
    pub const SCROLL_CHAIN: u32 = (1 << 8) | (1 << 9);
    pub const PRESS_LOCK: u32 = 1 << 13;
    pub const EVENT_BUBBLE: u32 = 1 << 14;
    pub const GESTURE_BUBBLE: u32 = 1 << 15;
}

pub mod prop {
    pub const RADIUS: u32 = 12;
    pub const PAD_TOP: u32 = 16;
    pub const PAD_BOTTOM: u32 = 17;
    pub const PAD_LEFT: u32 = 18;
    pub const PAD_RIGHT: u32 = 19;
    pub const PAD_ROW: u32 = 20;
    pub const PAD_COLUMN: u32 = 21;
    pub const LAYOUT: u32 = 22;
    pub const BG_COLOR: u32 = 28;
    pub const BG_OPA: u32 = 29;
    pub const BG_GRAD_DIR: u32 = 32;
    pub const BG_GRAD_COLOR: u32 = 35;
    pub const CLIP_CORNER: u32 = 45;
    pub const BORDER_WIDTH: u32 = 48;
    pub const BORDER_COLOR: u32 = 49;
    pub const BORDER_OPA: u32 = 50;
    pub const SHADOW_WIDTH: u32 = 60;
    pub const SHADOW_COLOR: u32 = 61;
    pub const SHADOW_OPA: u32 = 62;
    pub const SHADOW_OFFSET_Y: u32 = 65;
    pub const SHADOW_SPREAD: u32 = 66;
    pub const IMAGE_OPA: u32 = 68;
    pub const IMAGE_RECOLOR: u32 = 69;
    pub const IMAGE_RECOLOR_OPA: u32 = 70;
    pub const TEXT_COLOR: u32 = 88;
    pub const TEXT_OPA: u32 = 89;
    pub const TEXT_LINE_SPACE: u32 = 92;
    pub const TEXT_ALIGN: u32 = 94;
    pub const OPA: u32 = 95;
    pub const TRANSLATE_Y: u32 = 107;
    pub const TRANSFORM_SCALE_X: u32 = 108;
    pub const TRANSFORM_SCALE_Y: u32 = 109;
    pub const TRANSFORM_PIVOT_X: u32 = 111;
    pub const TRANSFORM_PIVOT_Y: u32 = 112;
}

pub mod align {
    pub const TOP_LEFT: u32 = 1;
}

pub mod event {
    pub const PRESSED: u32 = 1;
    pub const PRESSING: u32 = 2;
    pub const PRESS_LOST: u32 = 3;
    pub const CLICKED: u32 = 7;
    pub const RELEASED: u32 = 8;
}

pub mod text_align {
    pub const LEFT: u32 = 1;
    pub const CENTER: u32 = 2;
    pub const RIGHT: u32 = 3;
}

pub const SIZE_CONTENT: i32 = (1 << 29) | 2001;
pub const LONG_WRAP: u32 = 0;
pub const LONG_DOT: u32 = 1;

pub fn c_string(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 1);
    bytes.extend(text.bytes().filter(|b| *b != 0));
    bytes.push(0);
    bytes
}

pub unsafe fn set_style(object: Obj, id: u32, value: u32) {
    if !object.is_null() {
        unsafe { lvx_object_set_local_style_u32(object, id, value, 0) };
    }
}

pub unsafe fn style(object: Obj, props: &[(u32, u32)]) {
    for &(id, value) in props {
        unsafe { set_style(object, id, value) };
    }
}

/// A plain container (Band 10 Pro: the stock page-content widget, made transparent and
/// stripped of its padding and layout, so children sit where they are placed).
pub unsafe fn container(parent: Obj, x: i32, y: i32, w: i32, h: i32) -> Obj {
    let object = unsafe { lvx_object_create(parent) };
    if object.is_null() {
        return object;
    }
    unsafe {
        lvx_object_remove_style_all(object);
        {
            style(
                object,
                &[
                    (prop::PAD_TOP, 0),
                    (prop::PAD_BOTTOM, 0),
                    (prop::PAD_LEFT, 0),
                    (prop::PAD_RIGHT, 0),
                    (prop::PAD_ROW, 0),
                    (prop::PAD_COLUMN, 0),
                    (prop::LAYOUT, 0),
                    (prop::BG_OPA, 0),
                    (prop::BORDER_WIDTH, 0),
                    (prop::SHADOW_WIDTH, 0),
                ],
            );
        }
        lvx_object_set_size(object, w, h);
        lvx_object_align(object, align::TOP_LEFT, x, y);
    }
    object
}

/// Keeps Band 10 Pro's stock scroll bar off a scrolling container (Lyra Player's finding: its
/// containers are the stock page-content widget, which draws its own bar while a mode byte at +60
/// is 2; scroll bar styles do not reach it).
pub unsafe fn quiet_scroll_bar(object: Obj) {
    if !object.is_null() {
        unsafe { object.cast::<u8>().add(60).write(0) };
    }
}

/// A transparent box that does not take touches (a layout group).
pub unsafe fn group(parent: Obj, x: i32, y: i32, w: i32, h: i32) -> Obj {
    let object = unsafe { container(parent, x, y, w, h) };
    unsafe { clear_flag(object, flag::CLICKABLE | flag::SCROLLABLE) };
    object
}

pub unsafe fn place(object: Obj, x: i32, y: i32) {
    if !object.is_null() {
        unsafe { lvx_object_align(object, align::TOP_LEFT, x, y) };
    }
}

pub unsafe fn resize(object: Obj, w: i32, h: i32) {
    if !object.is_null() {
        unsafe { lvx_object_set_size(object, w, h) };
    }
}

pub unsafe fn show(object: Obj, visible: bool) {
    if !object.is_null() {
        unsafe { lvx_set_hidden(object, u32::from(!visible)) };
    }
}

pub unsafe fn add_flag(object: Obj, flags: u32) {
    if !object.is_null() {
        unsafe { lvx_object_add_flag(object, flags) };
    }
}

pub unsafe fn clear_flag(object: Obj, flags: u32) {
    if !object.is_null() {
        unsafe { lvx_object_clear_flag(object, flags) };
    }
}

pub unsafe fn on_event(object: Obj, callback: LvxEventCallback, code: u32, cookie: usize) {
    if !object.is_null() {
        unsafe { lvx_event_add(object, callback, code, cookie as *mut c_void) };
    }
}

pub unsafe fn delete(object: Obj) {
    if !object.is_null() {
        unsafe { lvx_object_delete(object) };
    }
}

/// (x, y, w, h) after the last layout pass.
pub unsafe fn measured(object: Obj) -> (i32, i32, i32, i32) {
    if object.is_null() {
        return (0, 0, 0, 0);
    }
    let [x1, y1, x2, y2] = unsafe { lvx_object_coords(object) };
    (x1, y1, x2 - x1 + 1, y2 - y1 + 1)
}

/// Small text is the body font drawn at this scale (256 = 1).
pub const SMALL_SCALE: i32 = 208;

/// A label in one of the firmware's MiSans styles. A height of `SIZE_CONTENT` grows with the
/// (wrapped) text.
pub unsafe fn label(parent: Obj, x: i32, y: i32, w: i32, large: bool, color: u32, align_text: u32, long_mode: u32) -> Obj {
    let label = unsafe { lvx_label_create(parent) };
    if label.is_null() {
        return label;
    }
    unsafe {
        let font = if large { STYLE_MISANS_DEMIBOLD_32 } else { STYLE_MISANS_REGULAR_24 };
        lvx_style_apply(label, font as *const c_void, 255, 0);
        style(label, &[(prop::TEXT_COLOR, color & 0xFF_FFFF), (prop::TEXT_OPA, 255), (prop::TEXT_ALIGN, align_text)]);
        lvx_label_set_long_mode(label, long_mode);
        lvx_object_set_size(label, w, SIZE_CONTENT);
        lvx_object_align(label, align::TOP_LEFT, x, y);
        lvx_object_clear_flag(label, flag::CLICKABLE);
    }
    label
}

pub unsafe fn set_text(label: Obj, text: &str) {
    if label.is_null() {
        return;
    }
    let text = c_string(text);
    unsafe { lvx_label_set_text(label, text.as_ptr()) };
}

pub unsafe fn image(parent: Obj) -> Obj {
    let image = unsafe { lvx_image_create(parent) };
    unsafe { clear_flag(image, flag::CLICKABLE | flag::SCROLLABLE) };
    image
}

/// Points an image at an absolute file (`path` NUL-terminated); LVGL copies the path.
pub unsafe fn image_src(image: Obj, path: &[u8]) {
    if !image.is_null() {
        unsafe { lvx_image_set_src(image, path.as_ptr().cast()) };
    }
}

/// Scales an image of natural size (w, h) to fit a `box_w`×`box_h` box centred on (cx, cy).
pub unsafe fn fit_image(image: Obj, natural: (u16, u16), cx: i32, cy: i32, box_w: i32, box_h: i32, cover: bool) {
    if image.is_null() || natural.0 == 0 || natural.1 == 0 {
        return;
    }
    let (w, h) = (natural.0 as i32, natural.1 as i32);
    let sx = box_w * 256 / w;
    let sy = box_h * 256 / h;
    let scale = if cover { sx.max(sy) } else { sx.min(sy) }.clamp(16, 2048);
    unsafe {
        lvx_object_set_size(image, w, h);
        lvx_object_align(image, align::TOP_LEFT, cx - w / 2, cy - h / 2);
        lvx_image_set_scale(image, scale, scale);
    }
}

/// Milliseconds on the firmware's monotonic clock.
pub fn now_ms() -> u32 {
    let ctx = crate::module::context();
    unsafe { ((*(*ctx).time).monotonic_ms)(ctx as *const _) }
}

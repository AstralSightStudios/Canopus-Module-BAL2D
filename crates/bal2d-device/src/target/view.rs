//! Where the frame is shown.
//!
//! * Marker mode: a patched watchface carries a placeholder Image record with a known UID; the
//!   firmware instantiates it as a `wfimg` (an `lv_image` subclass) at the layer's z position
//!   in the watchface. We point that object at our frame.
//! * Overlay mode ("全屏显示", for watchfaces without a layer): our own full-screen `lv_image`
//!   as the last child of the watchface's container (the parent of its first `wfimg`). It is
//!   part of the watchface like a marker: it scrolls away with it (the launcher and apps are
//!   never covered), swipes bubble to the home pages, and LVGL's cover check skips drawing the
//!   watchface under it (it is opaque). Watchfaces without `wfimg` objects (Lua) get none.
//!
//! The source is an in-memory LVGL 9 image descriptor over the render target. Two descriptors
//! alternate so each frame is a new source for LVGL (and its cache entry is dropped).

use alloc::boxed::Box;
use core::ffi::c_void;

use canopus_target_private::*;

use super::fw;
use super::lv::{self, Obj, event, flag};

/// LVGL's DELETE event.
const EVENT_DELETE: u32 = 36;
/// LVGL's DRAW_MAIN event (twelve before DELETE in LVGL 9.2's list): LVGL drew the object.
const EVENT_DRAW_MAIN: u32 = 24;
/// After a frame is handed over, LVGL must draw the object within this long (ms), or it counts
/// as covered (another page or window drawn over the watchface).
const COVER_GRACE_MS: u32 = 300;
/// While covered, the object is offered to LVGL again this often (ms) to notice it uncovered.
const COVER_PROBE_MS: u32 = 500;
/// Pages followed at once (home has four, plus the launcher list and the control centre).
const PAGE_SLOTS: usize = 16;
const LV_IMAGE_HEADER_MAGIC: u32 = 0x19;
const LV_COLOR_FORMAT_RGB565: u32 = 0x12;

/// Placeholder Image resource UID the watchface patcher writes (`bal2d_face::MARKER_UID`); the
/// color index bits 19..22 are ignored when matching.
pub const MARKER_UID: u32 = 0x0207_BA12;
const UID_COLOR_BITS: u32 = 0x0078_0000;
/// `wfimg`: resource UID at +136, resource record (`{uid, off, cf, flags, _, w: u16, h: u16, ..}`)
/// at +148.
const WFIMG_UID_OFFSET: usize = 136;
const WFIMG_RECORD_OFFSET: usize = 148;
/// `lv_image_t` (LVGL 9.2): `src` at +52, `src_type` in the low two bits of +96 (1: a file path).
const IMAGE_SRC_OFFSET: usize = 52;
const IMAGE_FLAGS_OFFSET: usize = 96;
/// Any LVGL image showing this file becomes the layer (Lua watchfaces, apps): the module writes
/// a small transparent image there so it decodes until the module takes the object over.
pub const LAYER_FILE: &str = "/data/canopus/m.ba_l2d/layer.bin";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    None,
    Marker,
    Overlay,
}

/// `lv_image_dsc_t` / `lv_draw_buf_t` prefix (zero-padded).
#[repr(C)]
struct ImageDsc {
    header: [u32; 3],
    data_size: u32,
    data: *const u8,
    rest: [u32; 3],
}

pub struct View {
    pub obj: Obj,
    pub mode: Mode,
    dsc: [Box<ImageDsc>; 2],
    flip: usize,
    /// The object's screen rectangle (x, y, w, h) at the last attach.
    pub rect: (i32, i32, i32, i32),
    /// The display size (set at attach): an object scrolled off it is not drawn.
    screen: (i32, i32),
    /// Full-screen objects drawn after ours (later siblings of it and of its ancestors) last seen
    /// off the screen or hidden, or covering it after that: pages that can slide or appear over
    /// the watchface (compared, never read). Rebuilt from the live siblings at every check.
    pages: [usize; PAGE_SLOTS],
    pages_len: usize,
    /// When LVGL last drew the object, the last frame was handed over, and the last probe (ms).
    drawn_at: u32,
    presented_at: u32,
    probed_at: u32,
    pub touches: heapless_touch::Queue,
    /// A press on a drag area holds the scroll chain and gesture bubbling back.
    grabbed: bool,
    /// Marker mode: a transparent object on top of the watchface's widgets over the drag areas
    /// (the head), so a pat reaches the lobby even where the time or a widget is drawn above it.
    touch: Obj,
    touch_rect: (i32, i32, i32, i32),
    /// Hidden while no pack is loaded (the last frame would otherwise stay up).
    hidden: bool,
}

/// A tiny fixed queue of touch events (filled by LVGL callbacks, drained by the frame).
pub mod heapless_touch {
    #[derive(Clone, Copy, Debug)]
    pub struct Ev {
        pub kind: u8,
        pub x: i32,
        pub y: i32,
    }

    pub struct Queue {
        pub items: [Ev; 16],
        pub len: usize,
    }

    impl Queue {
        pub const fn new() -> Queue {
            Queue { items: [Ev { kind: 0, x: 0, y: 0 }; 16], len: 0 }
        }

        pub fn push(&mut self, e: Ev) {
            if self.len < self.items.len() {
                self.items[self.len] = e;
                self.len += 1;
            } else if e.kind != 1 {
                // Keep press/release edges; drop the oldest move.
                self.items.copy_within(1.., 0);
                self.items[self.len - 1] = e;
            }
        }
    }

    impl Default for Queue {
        fn default() -> Self {
            Self::new()
        }
    }
}

static mut VIEW: Option<View> = None;

pub fn view() -> &'static mut View {
    unsafe {
        let slot = &mut *core::ptr::addr_of_mut!(VIEW);
        if slot.is_none() {
            let empty = || {
                Box::new(ImageDsc { header: [0; 3], data_size: 0, data: core::ptr::null(), rest: [0; 3] })
            };
            *slot = Some(View {
                obj: lv::NULL,
                mode: Mode::None,
                dsc: [empty(), empty()],
                flip: 0,
                rect: (0, 0, 0, 0),
                screen: (0, 0),
                pages: [0; PAGE_SLOTS],
                pages_len: 0,
                drawn_at: 0,
                presented_at: 0,
                probed_at: 0,
                touches: heapless_touch::Queue::new(),
                grabbed: false,
                touch: lv::NULL,
                touch_rect: (0, 0, 0, 0),
                hidden: false,
            });
        }
        slot.as_mut().unwrap()
    }
}

impl View {
    /// Describes the render target in both descriptors.
    pub fn bind(&mut self, data: *const u8, width: u32, height: u32, stride: u32) {
        for d in &mut self.dsc {
            d.header[0] = LV_IMAGE_HEADER_MAGIC | (LV_COLOR_FORMAT_RGB565 << 8);
            d.header[1] = width | (height << 16);
            d.header[2] = stride;
            d.data_size = stride * height;
            d.data = data;
        }
    }

    pub fn attached(&self) -> bool {
        !self.obj.is_null()
    }

    /// Places the touch proxy over `rect` (screen coordinates), creating it on first use.
    pub fn place_touch(&mut self, rect: (i32, i32, i32, i32)) {
        if self.mode == Mode::None || self.obj.is_null() || rect.2 <= 0 || rect.3 <= 0 {
            return;
        }
        if self.touch.is_null() {
            let parent = unsafe { core::ptr::read_volatile((self.obj as *const *mut c_void).add(1)) };
            if parent.is_null() {
                return;
            }
            let t = unsafe { lvx_image_create(parent) };
            if t.is_null() {
                return;
            }
            unsafe {
                lv::clear_flag(
                    t,
                    flag::SCROLLABLE | flag::SCROLL_CHAIN | flag::GESTURE_BUBBLE | flag::EVENT_BUBBLE | flag::CLICK_FOCUSABLE,
                );
                lv::add_flag(t, flag::CLICKABLE | flag::PRESS_LOCK);
                for code in [event::PRESSED, event::PRESSING, event::RELEASED, event::PRESS_LOST] {
                    lv::on_event(t, proxy_event, code, code as usize);
                }
                lv::on_event(t, proxy_deleted, EVENT_DELETE, 0);
            }
            self.touch = t;
            self.touch_rect = (0, 0, 0, 0);
        }
        if rect == self.touch_rect {
            return;
        }
        let parent = unsafe { core::ptr::read_volatile((self.touch as *const *mut c_void).add(1)) };
        let (px, py, _, _) = unsafe { lv::measured(parent) };
        unsafe {
            lvx_object_set_size(self.touch, rect.2, rect.3);
            lvx_object_align(self.touch, lv::align::TOP_LEFT, rect.0 - px, rect.1 - py);
            lvx_object_move_to_index(self.touch, -1);
        }
        self.touch_rect = rect;
    }

    /// Re-reads the marker's rectangle. Right after the watchface is built the object has no
    /// layout yet; then the placeholder's own size (resource record: width +12, height +14) is
    /// used until LVGL lays it out.
    pub fn remeasure(&mut self) {
        if self.mode == Mode::None || self.obj.is_null() {
            return;
        }
        let (x, y, w, h) = unsafe { lv::measured(self.obj) };
        if w > 1 && h > 1 {
            self.rect = (x, y, w, h);
            return;
        }
        if self.rect.2 > 1 && self.rect.3 > 1 {
            return;
        }
        let class = unsafe { core::ptr::read_volatile(self.obj as *const usize) };
        if class != fw::WFIMG_CLASS {
            return;
        }
        let rec = unsafe { core::ptr::read_volatile((self.obj as *const u8).add(WFIMG_RECORD_OFFSET) as *const *const u16) };
        if !rec.is_null() {
            let (rw, rh) = unsafe { (core::ptr::read_volatile(rec.add(6)), core::ptr::read_volatile(rec.add(7))) };
            self.rect = (x, y, rw as i32, rh as i32);
        }
    }

    /// Whether the object is on the active screen (a watchface left in the background, under an
    /// app or the launcher, is not drawn).
    pub fn visible(&self) -> bool {
        if self.obj.is_null() {
            return false;
        }
        // Apps and the launcher do not change the active screen on this firmware: they are
        // sibling windows of the watchface's, shown and hidden with LV_OBJ_FLAG_HIDDEN, so a
        // hidden ancestor means the watchface is not on screen (our own flag is ours to set).
        let mut o = self.obj;
        for _ in 0..64 {
            let parent = unsafe { core::ptr::read_volatile((o as *const *mut c_void).add(1)) };
            if parent.is_null() {
                return o == fw::active_screen() && self.on_display();
            }
            if unsafe { lv::flags(parent) } & flag::HIDDEN != 0 {
                return false;
            }
            o = parent;
        }
        false
    }

    /// Whether the object overlaps the display (its coordinates are absolute; a `wfimg` that
    /// reports no size has the size of its resource record, kept in `rect`).
    fn on_display(&self) -> bool {
        let (sw, sh) = self.screen;
        if sw <= 0 || sh <= 0 {
            return true;
        }
        let (x, y, w, h) = unsafe { lv::measured(self.obj) };
        let (w, h) = if w > 1 && h > 1 { (w, h) } else { (self.rect.2, self.rect.3) };
        x < sw && y < sh && x + w > 0 && y + h > 0
    }

    pub fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
        if self.obj.is_null() {
            return;
        }
        unsafe {
            if hidden {
                lv::add_flag(self.obj, flag::HIDDEN);
            } else {
                lv::clear_flag(self.obj, flag::HIDDEN);
            }
        }
    }

    /// Shows the latest frame.
    pub fn present(&mut self) {
        if self.obj.is_null() {
            return;
        }
        if self.hidden {
            self.set_hidden(false);
        }
        self.flip ^= 1;
        let src = &*self.dsc[self.flip] as *const ImageDsc as *const c_void;
        unsafe { lvx_image_set_src(self.obj, src) };
        self.presented_at = lv::now_ms();
    }

    /// Calls `f` for every object drawn after ours: the later siblings of the object and of each
    /// of its ancestors (all alive while the object is).
    fn later_siblings(&self, mut f: impl FnMut(Obj) -> bool) -> bool {
        let mut o = self.obj;
        for _ in 0..32 {
            let parent = unsafe { lv::parent(o) };
            if parent.is_null() {
                break;
            }
            let kids = unsafe { lv::children(parent) };
            if let Some(i) = kids.iter().position(|k| *k == o) {
                for &k in &kids[i + 1..] {
                    if f(k) {
                        return true;
                    }
                }
            }
            o = parent;
        }
        false
    }

    /// Whether `o` is shown over most of the display.
    fn over_screen(&self, o: Obj) -> bool {
        let (sw, sh) = self.screen;
        if o.is_null() || sw <= 0 || sh <= 0 || unsafe { lv::flags(o) } & flag::HIDDEN != 0 {
            return false;
        }
        let (x, y, w, h) = unsafe { lv::measured(o) };
        let ow = (x + w).min(sw) - x.max(0);
        let oh = (y + h).min(sh) - y.max(0);
        ow > 0 && oh > 0 && (ow as i64 * oh as i64) * 5 >= (sw as i64 * sh as i64) * 4
    }

    /// Whether `o` is the size of most of the display, wherever it is.
    fn screen_sized(&self, o: Obj) -> bool {
        let (sw, sh) = self.screen;
        let (_, _, w, h) = unsafe { lv::measured(o) };
        sw > 0 && sh > 0 && (w.min(sw) as i64 * h.min(sh) as i64) * 5 >= (sw as i64 * sh as i64) * 4
    }

    /// Whether a page slid or appeared over the watchface. The launcher list, the control centre
    /// and the other home pages are screen-sized siblings drawn after it, kept hidden or off the
    /// screen until shown, and LVGL keeps drawing the watchface under them. A screen-sized object
    /// first seen already over the screen is the watchface's own (a Lua face builds its
    /// full-screen parts after the layer exists) and never counts: only one seen away first does.
    pub fn paged_over(&mut self) -> bool {
        if self.obj.is_null() {
            self.pages_len = 0;
            return false;
        }
        let known = self.pages;
        let known = &known[..self.pages_len];
        let (mut next, mut n, mut paged) = ([0usize; PAGE_SLOTS], 0, false);
        self.later_siblings(|o| {
            if !self.screen_sized(o) {
                return false;
            }
            let over = self.over_screen(o);
            let page = known.contains(&(o as usize));
            paged |= over && page;
            if (!over || page) && n < PAGE_SLOTS {
                next[n] = o as usize;
                n += 1;
            }
            false
        });
        self.pages = next;
        self.pages_len = n;
        paged
    }

    /// Debug: the ancestor chain (class [index/count]).
    pub fn chain_debug(&self) -> alloc::string::String {
        let mut s = alloc::string::String::new();
        let mut o = self.obj;
        for _ in 0..32 {
            let parent = unsafe { lv::parent(o) };
            if parent.is_null() {
                break;
            }
            let kids = unsafe { lv::children(parent) };
            let i = kids.iter().position(|k| *k == o).map_or(-1, |i| i as i32);
            let class = unsafe { core::ptr::read_volatile(parent as *const usize) };
            s += &alloc::format!(" {:x}[{i}/{}]", class & 0xffff, kids.len());
            o = parent;
        }
        s += &alloc::format!("; pages {}", self.pages_len);
        s
    }

    /// Whether LVGL has stopped drawing the object although it is visible by the tree: the
    /// launcher, the control centre and other pages are drawn over the watchface without hiding
    /// or moving it. While covered, the frame is offered again every `COVER_PROBE_MS` (no new
    /// rendering) and drawing resumes once LVGL draws it.
    pub fn covered(&mut self, now: u32) -> bool {
        if self.obj.is_null() || self.presented_at == 0 {
            return false;
        }
        let drawn_since = self.drawn_at.wrapping_sub(self.presented_at) as i32 >= 0;
        if drawn_since || now.wrapping_sub(self.presented_at) < COVER_GRACE_MS {
            return false;
        }
        if now.wrapping_sub(self.probed_at) >= COVER_PROBE_MS {
            self.probed_at = now;
            self.present();
            // A probe is not a frame: the grace period does not start again.
            self.presented_at = now.wrapping_sub(COVER_GRACE_MS);
        }
        true
    }

    pub fn detach(&mut self) {
        if self.mode == Mode::Overlay {
            unsafe { lv::delete(self.obj) };
        }
        self.obj = lv::NULL;
        self.mode = Mode::None;
    }

    fn hook_events(&mut self, obj: Obj) {
        unsafe {
            lv::add_flag(obj, flag::CLICKABLE | flag::PRESS_LOCK);
            for code in [event::PRESSED, event::PRESSING, event::RELEASED, event::PRESS_LOST] {
                lv::on_event(obj, touch_event, code, code as usize);
            }
            lv::on_event(obj, deleted, EVENT_DELETE, 0);
            lv::on_event(obj, drawn, EVENT_DRAW_MAIN, 0);
        }
        self.drawn_at = 0;
        self.presented_at = 0;
    }

    /// The full-screen layer on top of a watchface without one (see the module docs).
    pub fn attach_overlay(&mut self, width: i32, height: i32) -> bool {
        if self.mode == Mode::Overlay && !self.obj.is_null() {
            return true;
        }
        let root = fw::active_screen();
        if root.is_null() {
            return false;
        }
        let mut image: Obj = lv::NULL;
        fw::tree_walk(root, find_wfimg, (&mut image) as *mut Obj as *mut c_void);
        if image.is_null() {
            return false;
        }
        let parent = unsafe { core::ptr::read_volatile((image as *const *mut c_void).add(1)) };
        if parent.is_null() {
            return false;
        }
        let obj = unsafe { lvx_image_create(parent) };
        if obj.is_null() {
            return false;
        }
        unsafe {
            // Like a marker: not scrollable itself, so drags scroll the home pages.
            lv::clear_flag(obj, flag::SCROLLABLE | flag::CLICK_FOCUSABLE);
            lvx_object_set_size(obj, width, height);
            lvx_object_align(obj, lv::align::TOP_LEFT, 0, 0);
        }
        self.hook_events(obj);
        self.obj = obj;
        self.mode = Mode::Overlay;
        self.screen = (width, height);
        self.pages_len = 0;
        self.paged_over();
        super::trace(&alloc::format!("attach chain:{}", self.chain_debug()));
        self.rect = (0, 0, width, height);
        true
    }

    /// Finds the marker image in the current watchface and takes it over.
    pub fn attach_marker(&mut self, screen: (i32, i32)) -> bool {
        if self.mode == Mode::Marker && !self.obj.is_null() {
            // Still alive (the delete hook clears it); a watchface switch builds a new one.
            return true;
        }
        let root = fw::active_screen();
        if root.is_null() {
            return false;
        }
        let mut found: Obj = lv::NULL;
        fw::tree_walk(root, find_marker, (&mut found) as *mut Obj as *mut c_void);
        if found.is_null() {
            return false;
        }
        if self.mode == Mode::Overlay && !self.obj.is_null() {
            // A patched watchface came up: the development overlay goes away.
            unsafe { lv::delete(self.obj) };
        }
        self.hook_events(found);
        self.obj = found;
        self.mode = Mode::Marker;
        self.screen = screen;
        self.pages_len = 0;
        self.paged_over();
        super::trace(&alloc::format!("attach chain:{}", self.chain_debug()));
        let (x, y, w, h) = unsafe { lv::measured(found) };
        let class = unsafe { core::ptr::read_volatile(found as *const usize) };
        if class != fw::WFIMG_CLASS && w <= 16 && h <= 16 {
            // An image path without a size (it shows the 16 x 16 placeholder): the whole screen.
            unsafe { lvx_object_set_size(found, screen.0, screen.1) };
            self.rect = (x, y, screen.0, screen.1);
            return true;
        }
        self.rect = (x, y, w, h);
        true
    }
}

/// The first `wfimg` of a watchface (its container hosts the overlay).
extern "C" fn find_wfimg(obj: *mut c_void, user: *mut c_void) -> u32 {
    if obj.is_null() {
        return 0;
    }
    if unsafe { core::ptr::read_volatile(obj as *const usize) } == fw::WFIMG_CLASS {
        unsafe { *(user as *mut Obj) = obj };
        return 2; // stop
    }
    0
}

extern "C" fn find_marker(obj: *mut c_void, user: *mut c_void) -> u32 {
    if obj.is_null() {
        return 0;
    }
    let class = unsafe { core::ptr::read_volatile(obj as *const usize) };
    if class == fw::WFIMG_CLASS {
        let uid = unsafe { core::ptr::read_volatile((obj as *const u8).add(WFIMG_UID_OFFSET) as *const u32) };
        if uid & !UID_COLOR_BITS == MARKER_UID {
            unsafe { *(user as *mut Obj) = obj };
            return 2; // stop
        }
    } else if class == fw::image_class() && shows_layer_file(obj) {
        unsafe { *(user as *mut Obj) = obj };
        return 2;
    }
    0
}

/// Whether an `lv_image` shows [`LAYER_FILE`] (with or without an LVGL drive letter).
fn shows_layer_file(obj: *mut c_void) -> bool {
    let flags = unsafe { core::ptr::read_volatile((obj as *const u8).add(IMAGE_FLAGS_OFFSET) as *const u32) };
    if flags & 3 != 1 {
        return false;
    }
    let src = unsafe { core::ptr::read_volatile((obj as *const u8).add(IMAGE_SRC_OFFSET) as *const *const u8) };
    if src.is_null() {
        return false;
    }
    let want = LAYER_FILE.as_bytes();
    // The path may carry a drive prefix ("A:"): compare the tail.
    let mut n = 0;
    while n < 96 && unsafe { *src.add(n) } != 0 {
        n += 1;
    }
    if n < want.len() || n >= 96 {
        return false;
    }
    let tail = unsafe { core::slice::from_raw_parts(src.add(n - want.len()), want.len()) };
    tail == want
}

extern "C" fn proxy_event(e: *mut c_void) {
    let code = unsafe { lvx_event_get_user_data(e) } as u32;
    let Some((x, y)) = fw::event_point(e) else { return };
    let v = view();
    let kind = match code {
        event::PRESSED => 0,
        event::PRESSING => 1,
        _ => 2,
    };
    v.touches.push(heapless_touch::Ev { kind, x: x - v.rect.0, y: y - v.rect.1 });
}

extern "C" fn proxy_deleted(_e: *mut c_void) {
    let v = view();
    v.touch = lv::NULL;
    v.touch_rect = (0, 0, 0, 0);
}

extern "C" fn drawn(_e: *mut c_void) {
    view().drawn_at = lv::now_ms();
}

extern "C" fn deleted(_e: *mut c_void) {
    let v = view();
    v.grabbed = false;
    v.obj = lv::NULL;
    v.mode = Mode::None;
}

extern "C" fn touch_event(e: *mut c_void) {
    let code = unsafe { lvx_event_get_user_data(e) } as u32;
    let Some((x, y)) = fw::event_point(e) else { return };
    let v = view();
    let kind = match code {
        event::PRESSED => 0,
        event::PRESSING => 1,
        _ => 2,
    };
    if v.mode == Mode::Marker {
        // A press on a drag area (the head) keeps the gesture: the watchface's own page swipe and
        // scrolling stop at this object until release. Elsewhere swipes reach the system as usual.
        if kind == 0 {
            if super::grabs(x, y) {
                unsafe { lv::clear_flag(v.obj, flag::SCROLL_CHAIN | flag::GESTURE_BUBBLE) };
                v.grabbed = true;
            }
        } else if kind == 2 && v.grabbed {
            unsafe { lv::add_flag(v.obj, flag::SCROLL_CHAIN | flag::GESTURE_BUBBLE) };
            v.grabbed = false;
        }
    }
    v.touches.push(heapless_touch::Ev { kind, x: x - v.rect.0, y: y - v.rect.1 });
}

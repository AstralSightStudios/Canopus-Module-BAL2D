//! The speech bubble of a talk: the game's lines for it, one after another, each for as long
//! as the game shows it, in a white bubble at the bottom of the screen whose tail points up at
//! the character's head. Inner thoughts get a softer bubble with a "thinking" tail of dots.
//!
//! LVGL objects in the watchface's container, drawn by LVGL with the watch's own MiSans font;
//! they take no touches. On the watch, a label restyled or moved in its parent after it was
//! first drawn lost its background (and `move_to_index` put it under the watchface's widgets),
//! so the bubble is made anew, last in its parent and fully styled, for every talk, and deleted
//! after it; only its text changes between lines. The rise when it appears is a translation, the
//! text fades in: no transform layers, cheap for LVGL to draw every frame.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;

use bal2d_pack::Line;
use canopus_target_private::*;

use super::lv::{self, Obj, flag, prop};

const EVENT_DELETE: u32 = 36;
const ALIGN_BOTTOM_MID: u32 = 5;
const ALIGN_OUT_TOP_LEFT: u32 = 10;
const LV_IMAGE_HEADER_MAGIC: u32 = 0x19;
const LV_COLOR_FORMAT_ARGB8888: u32 = 0x10;

/// Look (the game's lobby bubble): white, a light-blue edge, navy text.
const BG: u32 = 0xFF_FFFF;
const BG_OPA: u32 = 242;
const EDGE: u32 = 0xB9_D7F0;
const TEXT: u32 = 0x2D_4663;
const THINK_BG: u32 = 0xEE_F5FC;
const THINK_OPA: u32 = 222;
const THINK_TEXT: u32 = 0x4A_6585;
const RADIUS: u32 = 18;
const BORDER: u32 = 2;
const PAD_X: i32 = 16;
const PAD_Y: i32 = 11;
const MAX_W: i32 = 316;
const MIN_W: i32 = 120;
/// MiSans at 24 px: a CJK glyph's advance, and rough ones for the rest (a line estimated a
/// little short still wraps in LVGL; the slack keeps that rare).
const CJK_PX: f32 = 24.0;
const SLACK_PX: i32 = 8;
const MARGIN_BOTTOM: i32 = 14;
const TAIL_W: u32 = 32;
const TAIL_H: u32 = 18;
/// The rise when the bubble appears (ms, px) and the text fade of each line (ms).
const FADE_IN_MS: f32 = 170.0;
const TEXT_FADE_MS: f32 = 130.0;
const RISE_PX: f32 = 10.0;

#[repr(C)]
struct ImageDsc {
    header: [u32; 3],
    data_size: u32,
    data: *const u8,
    rest: [u32; 3],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Hidden,
    Showing,
}

pub struct Bubble {
    obj: Obj,
    tail: Obj,
    /// Tail images: a triangle (speech) and dots (thought), straight-alpha ARGB8888.
    talk_tail: Option<(Box<ImageDsc>, Vec<u8>)>,
    think_tail: Option<(Box<ImageDsc>, Vec<u8>)>,
    lines: Vec<Line>,
    line: usize,
    /// Time in the current line and since the bubble appeared (ms).
    line_ms: f32,
    shown_ms: f32,
    phase: Phase,
    /// The bubble on the screen is a thought bubble.
    thought: bool,
    /// Where the tail points (screen x), and the head's top (screen y).
    anchor: Option<(f32, f32)>,
    /// The styles last written for the rise and the text fade: only changes are written.
    applied: (u8, i8),
    /// The bubble's left edge on the screen and its width.
    bx: i32,
    bw: i32,
}

/// Characters a line must not start with (they stay at the end of the previous one).
const NO_START: &str = "，。！？、；：…—～）」』】〉》”’,.!?;:)]~♪♡";

fn char_px(c: char) -> f32 {
    match c {
        ' ' => 7.0,
        c if c.is_ascii() => 13.0,
        _ => CJK_PX,
    }
}

/// Breaks the game's lines (written for a wider bubble) into lines of about equal length that
/// fit `max` pixels, none starting with closing punctuation. Lines with spaces (English) are left
/// to LVGL's word wrap. Returns the text and the widest line's estimated width.
fn rewrap(text: &str, max: f32) -> (String, f32) {
    let mut out = String::new();
    let mut widest = 0.0f32;
    for (k, line) in text.split('\n').enumerate() {
        if k > 0 {
            out.push('\n');
        }
        let line = line.trim_end_matches('\r');
        let total: f32 = line.chars().map(char_px).sum();
        if total <= max || line.contains(' ') {
            out.push_str(line);
            widest = widest.max(total.min(max));
            continue;
        }
        // Lines needed (rounded up; no f32::ceil without std).
        let q = total / (max * 0.94);
        let n = ((q as i32) + (q > q as i32 as f32) as i32).max(2) as f32;
        let target = total / n;
        let mut acc = 0.0;
        for c in line.chars() {
            let w = char_px(c);
            if acc > 0.0 && acc + w > target + 1.0 && !NO_START.contains(c) {
                out.push('\n');
                widest = widest.max(acc);
                acc = 0.0;
            }
            out.push(c);
            acc += w;
        }
        widest = widest.max(acc);
    }
    (out, widest)
}

static mut BUBBLE: Option<Bubble> = None;

pub fn bubble() -> &'static mut Bubble {
    unsafe {
        let slot = &mut *core::ptr::addr_of_mut!(BUBBLE);
        slot.get_or_insert_with(|| Bubble {
            obj: lv::NULL,
            tail: lv::NULL,
            talk_tail: None,
            think_tail: None,
            lines: Vec::new(),
            line: 0,
            line_ms: 0.0,
            shown_ms: 0.0,
            phase: Phase::Hidden,
            thought: false,
            anchor: None,
            applied: (0, 0),
            bx: 0,
            bw: 0,
        })
    }
}

/// LVGL deleted an object of the bubble (with the watchface, or `remove`): the next update
/// makes it again if a talk is still on.
extern "C" fn deleted(e: *mut c_void) {
    let b = bubble();
    let o = unsafe { lvx_event_get_user_data(e) } as usize;
    if o == 0 {
        b.obj = lv::NULL;
    } else {
        b.tail = lv::NULL;
    }
}

/// A tail image: `f(x, y)` gives (fill, edge) coverage at pixel centres.
fn tail_image(fill: u32, fill_opa: u32, edge: u32, f: impl Fn(f32, f32) -> (f32, f32)) -> (Box<ImageDsc>, Vec<u8>) {
    let (w, h) = (TAIL_W, TAIL_H);
    let mut px = alloc::vec![0u8; (w * h * 4) as usize];
    let rgb = |c: u32| [(c & 0xFF) as f32, ((c >> 8) & 0xFF) as f32, ((c >> 16) & 0xFF) as f32];
    let (fc, ec) = (rgb(fill), rgb(edge));
    for y in 0..h {
        for x in 0..w {
            let (cf, ce) = f(x as f32 + 0.5, y as f32 + 0.5);
            let (cf, ce) = (cf.clamp(0.0, 1.0), ce.clamp(0.0, 1.0));
            // Edge over fill, straight alpha.
            let af = cf * fill_opa as f32 / 255.0;
            let a = ce + af * (1.0 - ce);
            let o = ((y * w + x) * 4) as usize;
            if a > 0.0 {
                for k in 0..3 {
                    px[o + k] = ((ec[k] * ce + fc[k] * af * (1.0 - ce)) / a + 0.5) as u8;
                }
                px[o + 3] = (a * 255.0 + 0.5) as u8;
            }
        }
    }
    let dsc = Box::new(ImageDsc {
        header: [LV_IMAGE_HEADER_MAGIC | (LV_COLOR_FORMAT_ARGB8888 << 8), w | (h << 16), w * 4],
        data_size: w * h * 4,
        data: px.as_ptr(),
        rest: [0; 3],
    });
    (dsc, px)
}

/// A tail pointing up: apex at the top centre, its base (the bottom rows) overlapping the
/// bubble's edge so the two join.
fn talk_tail(fill: u32, opa: u32) -> (Box<ImageDsc>, Vec<u8>) {
    let (w, h) = (TAIL_W as f32, TAIL_H as f32);
    tail_image(fill, opa, EDGE, move |x, y| {
        // Distance outside each slanted side (unit normals), the apex at (w/2, 0.5).
        let (ax, ay) = (w * 0.5, 0.5);
        let side = |sx: f32| {
            // Side from the apex to (sx, h).
            let (dx, dy) = (sx - ax, h - ay);
            let len = bal2d_spine::math::sqrt(dx * dx + dy * dy);
            let (nx, ny) = (dy / len, -dx / len);
            let s = if sx > ax { 1.0 } else { -1.0 };
            ((x - ax) * nx + (y - ay) * ny) * s
        };
        let out = side(w).max(side(0.0));
        let inside = 0.5 - out;
        // The edge along both slanted sides, not along the base (the bubble is there).
        let e = (BORDER as f32 + 0.5 - (out + BORDER as f32).abs()).min(1.0);
        let base = y < h - 3.0;
        (inside, if base { e } else { 0.0 })
    })
}

/// Two small circles rising towards the head.
fn think_tail(fill: u32, opa: u32) -> (Box<ImageDsc>, Vec<u8>) {
    tail_image(fill, opa, EDGE, |x, y| {
        let circle = |cx: f32, cy: f32, r: f32| {
            let d = bal2d_spine::math::sqrt((x - cx) * (x - cx) + (y - cy) * (y - cy));
            (r + 0.5 - d, (1.5 - (d - r + 0.75).abs()).min(1.0))
        };
        let (a, ea) = circle(11.0, 11.0, 6.0);
        let (b, eb) = circle(21.0, 3.5, 3.2);
        (a.max(b), ea.max(eb))
    })
}

impl Bubble {
    /// The bubble for line `i`, made last in `parent`; the tail after it, over its edge.
    fn make(&mut self, parent: Obj, screen: (i32, i32), i: usize) -> bool {
        self.remove();
        let Some(l) = self.lines.get(i) else { return false };
        let thought = l.thought;
        let max = (screen.0 - 20).min(MAX_W);
        let inner = (max - 2 * PAD_X - 2 * BORDER as i32) as f32;
        let (text, widest) = rewrap(&l.text, inner);
        let w = (widest as i32 + 2 * PAD_X + 2 * BORDER as i32 + SLACK_PX).clamp(MIN_W, max);
        // Bottom-centred in a parent as wide as the screen.
        self.bx = (screen.0 - w) / 2;
        self.bw = w;
        unsafe {
            let obj = lv::label(parent, 0, 0, w, false, if thought { THINK_TEXT } else { TEXT }, lv::text_align::CENTER, lv::LONG_WRAP);
            if obj.is_null() {
                return false;
            }
            lv::clear_flag(obj, flag::CLICKABLE | flag::SCROLLABLE | flag::CLICK_FOCUSABLE);
            lv::style(
                obj,
                &[
                    (prop::BG_COLOR, if thought { THINK_BG } else { BG }),
                    (prop::BG_OPA, if thought { THINK_OPA } else { BG_OPA }),
                    (prop::RADIUS, RADIUS),
                    (prop::BORDER_WIDTH, BORDER),
                    (prop::BORDER_COLOR, EDGE),
                    (prop::BORDER_OPA, 255),
                    (prop::PAD_TOP, PAD_Y as u32),
                    (prop::PAD_BOTTOM, PAD_Y as u32),
                    (prop::PAD_LEFT, PAD_X as u32),
                    (prop::PAD_RIGHT, PAD_X as u32),
                    (prop::TEXT_LINE_SPACE, 2),
                ],
            );
            lv::set_text(obj, &text);
            lvx_object_align(obj, ALIGN_BOTTOM_MID, 0, -MARGIN_BOTTOM);
            lv::on_event(obj, deleted, EVENT_DELETE, 0);
            self.obj = obj;
            let slot = if thought { &mut self.think_tail } else { &mut self.talk_tail };
            if slot.is_none() {
                *slot = Some(if thought { think_tail(THINK_BG, THINK_OPA) } else { talk_tail(BG, BG_OPA) });
            }
            let tail = lvx_image_create(parent);
            if !tail.is_null() {
                lv::clear_flag(tail, flag::CLICKABLE | flag::SCROLLABLE | flag::CLICK_FOCUSABLE);
                if let Some((dsc, _)) = slot {
                    lvx_image_set_src(tail, &**dsc as *const ImageDsc as *const c_void);
                }
                lv::on_event(tail, deleted, EVENT_DELETE, 1);
                self.tail = tail;
            }
        }
        self.thought = thought;
        self.applied = (255, 127);
        self.place_tail();
        true
    }

    fn remove(&mut self) {
        let (obj, tail) = (self.obj, self.tail);
        self.obj = lv::NULL;
        self.tail = lv::NULL;
        unsafe {
            lv::delete(tail);
            lv::delete(obj);
        }
    }

    /// A talk started: its lines from the first.
    pub fn start(&mut self, lines: &[Line]) {
        self.lines.clear();
        self.lines.extend(lines.iter().filter(|l| !l.text.trim().is_empty()).cloned());
        self.line = 0;
        self.line_ms = 0.0;
        self.shown_ms = 0.0;
        self.remove();
        // Shown from the next update (which knows where the layer is).
        self.phase = if self.lines.is_empty() { Phase::Hidden } else { Phase::Showing };
    }

    /// The talk ended early: the bubble goes.
    pub fn stop(&mut self) {
        self.clear();
    }

    /// Gone at once (the pack is unloaded, the watchface left).
    /// Ends any talk and frees what talks allocated (their lines, the tail images): called when
    /// the pack goes, so nothing of it stays in the middle of the freed memory.
    pub fn clear(&mut self) {
        self.phase = Phase::Hidden;
        self.remove();
        self.lines = Vec::new();
        self.talk_tail = None;
        self.think_tail = None;
    }

    pub fn active(&self) -> bool {
        self.phase != Phase::Hidden
    }

    /// The tail on the bubble's top edge, under the head (as far as the corners allow).
    fn place_tail(&mut self) {
        if self.obj.is_null() || self.tail.is_null() {
            return;
        }
        unsafe {
            let (bx, bw) = (self.bx, self.bw);
            let lo = RADIUS as i32;
            let hi = (bw - RADIUS as i32 - TAIL_W as i32).max(lo);
            let x = match self.anchor {
                Some((hx, _)) => (hx as i32 - bx - TAIL_W as i32 / 2).clamp(lo, hi),
                None => (bw - TAIL_W as i32) / 2,
            };
            // The tail's bottom rows cover the bubble's edge under it.
            lvx_align_to(self.tail, self.obj, ALIGN_OUT_TOP_LEFT, x, BORDER as i32 + 1);
        }
    }

    /// The rise and the text fade (only what changed is written).
    fn style_at(&mut self, text: f32, rise: f32) {
        let want = ((text.clamp(0.0, 1.0) * 255.0 + 0.5) as u8, rise as i8);
        if want == self.applied {
            return;
        }
        self.applied = want;
        let dy = want.1 as i32 as u32;
        unsafe {
            lv::style(self.obj, &[(prop::TEXT_OPA, want.0 as u32), (prop::TRANSLATE_Y, dy)]);
            lv::style(self.tail, &[(prop::TRANSLATE_Y, dy)]);
        }
    }

    /// Advances by `dt_ms`. `parent`: where the bubble goes (over the watchface's widgets);
    /// `anchor`: the head (screen x, top y) if the lobby knows it.
    pub fn update(&mut self, dt_ms: f32, parent: Obj, screen: (i32, i32), anchor: Option<(f32, f32)>) {
        if self.phase == Phase::Hidden || parent.is_null() {
            return;
        }
        if self.obj.is_null() {
            // A new talk, or the watchface was built again: the bubble for the current line.
            if self.shown_ms == 0.0 {
                self.anchor = anchor;
            }
            if !self.make(parent, screen, self.line) {
                self.clear();
                return;
            }
        }
        self.shown_ms += dt_ms;
        self.line_ms += dt_ms;
        let line_len = self.lines.get(self.line).map_or(0, |l| l.ms) as f32;
        if self.line_ms >= line_len {
            if self.line + 1 >= self.lines.len() {
                self.clear();
                return;
            }
            self.line += 1;
            self.line_ms = 0.0;
            // The next line in a bubble of its own (made fully styled, like the first).
            if !self.make(parent, screen, self.line) {
                self.clear();
                return;
            }
        }
        let t = (self.shown_ms / FADE_IN_MS).min(1.0);
        let ease = 1.0 - (1.0 - t) * (1.0 - t);
        let text = (self.line_ms / TEXT_FADE_MS).min(1.0);
        self.style_at(text, RISE_PX * (1.0 - ease));
    }
}

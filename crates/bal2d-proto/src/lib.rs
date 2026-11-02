//! The protocol between the BAL2D AstroBox plugin and the `ba_l2d` module, carried by
//! CanopusConnect messages (ADR-CAN-009): one binary frame of at most [`FRAME_MAX`] bytes per
//! message, delivered in order while the phone listens (several may be in flight at once, and
//! frames of one burst can swap places on the way).
//!
//! All integers little endian. A frame starts with an opcode:
//!
//! * phone -> watch requests: `[op][req: u16][payload]`, except `DATA`, which is
//!   `[op][offset: u32][bytes]` and has no reply of its own;
//! * watch -> phone replies: `[op | 0x80][req: u16][status: u8][payload]`;
//! * watch -> phone events: `[0xC0 + n][payload]`.
//!
//! A pack (`.bal2d`, already compiled for the watch on the phone) travels as one stream: `BEGIN`
//! names it, `DATA` frames carry it at increasing offsets, the watch confirms with `PROGRESS`
//! events at least every [`PROGRESS_STEP`] bytes and `END` makes it installed once its size and
//! CRC-32 match. The phone never has more than [`WINDOW`] unconfirmed bytes out.
//!
//! The codec is in [`codec`], the watch's half in [`receiver`], the phone's in [`sender`].

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod codec;
pub mod receiver;
pub mod sender;

pub use codec::*;

pub const MODULE_ID: &str = "ba_l2d";
/// The protocol this crate speaks; the module answers `HELLO` with its own.
pub const VERSION: u8 = 1;

/// CanopusConnect carries at most this many bytes per message.
pub const FRAME_MAX: usize = 480;
/// Pack bytes per `DATA` frame.
pub const DATA_MAX: usize = 464;
/// The watch confirms at least every this many bytes.
pub const PROGRESS_STEP: u32 = 4 * 1024;
/// Unconfirmed bytes the phone may have out.
pub const WINDOW: u32 = 16 * 1024;

/// Largest pack the watch accepts.
pub const MAX_PACK_BYTES: u32 = 8 * 1024 * 1024;
/// Display names are cut to this many bytes (at a character boundary).
pub const MAX_NAME: usize = 64;
/// Packs a watch keeps at most.
pub const MAX_PACKS: usize = 32;

/// `HELLO` capability bits.
pub mod caps {
    /// The module runs on a firmware whose Connect service delivers frames without a reply per
    /// frame: the phone may keep several `DATA` frames in flight.
    pub const STREAM: u8 = 1;
}

/// `OPTIONS` bits (persisted by the module).
pub mod options {
    /// Watchfaces without a BAL2D layer show the lobby full screen above the watchface.
    pub const OVERLAY: u8 = 1;
    /// Smoothness first: the watch keeps about a tenth of each frame idle instead of a third (or
    /// half for very heavy frames). Faster animation; whether the phone link and the watch's own
    /// UI stay responsive with that little is what it is for finding out on a real watch.
    pub const FLUENT: u8 = 2;
}

/// The firmware a module build runs on (`HELLO`): the phone compiles packs for it.
pub mod target {
    pub const UNKNOWN: u8 = 0;
    /// Xiaomi Smart Band 10 Pro, firmware 3.101.043 (336 x 480), the one supported watch.
    pub const BAND_10_PRO_043: u8 = 1;
}

/// Incremental CRC-32 (IEEE, the one `.bal2d` sections and zip use).
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub const fn new() -> Self {
        Self(0xffff_ffff)
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.0;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        self.0 = crc;
    }

    pub const fn finish(self) -> u32 {
        !self.0
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finish()
}

/// Cuts `text` to at most `max` bytes at a character boundary.
pub fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_reference_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        let mut c = Crc32::new();
        c.update(b"1234");
        c.update(b"56789");
        assert_eq!(c.finish(), 0xcbf4_3926);
    }

    #[test]
    fn clip_keeps_whole_characters() {
        assert_eq!(clip("白子", 4), "白");
        assert_eq!(clip("abc", 8), "abc");
    }
}

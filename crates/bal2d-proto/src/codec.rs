//! Frame encoding and decoding.
//!
//! | op | request | payload | reply payload (after `status`) |
//! |---|---|---|---|
//! | `0x01` | `HELLO` | `version: u8` | `version: u8, caps: u8, max_data: u16, free_kib: u32, width: u16, height: u16, target: u8, active: u32, packs: u8, options: u8` |
//! | `0x02` | `LIST` | `offset: u8, max: u8` | `total: u8, offset: u8, count: u8, {id: u32, size: u32, name}…` |
//! | `0x03` | `STATS` | — | `Stats` (see [`Stats`]) |
//! | `0x10` | `BEGIN` | `id: u32, size: u32, crc32: u32, name` | `value: u32` |
//! | `0x11` | `DATA` | (no `req`) `offset: u32, bytes` | — |
//! | `0x12` | `END` | — | `value: u32` (the installed id) |
//! | `0x14` | `ABORT` | — | `value: u32` |
//! | `0x20` | `DELETE` | `id: u32` | `value: u32` |
//! | `0x21` | `SELECT` | `id: u32` | `value: u32` |
//! | `0x22` | `OPTIONS` | `options: u8` | `value: u32` (the options now in effect) |
//!
//! Strings are `[len: u8][UTF-8]`. Events: `0xC0 PROGRESS` `next: u32, status: u8`.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{DATA_MAX, FRAME_MAX, MAX_NAME, clip};

pub mod op {
    pub const HELLO: u8 = 0x01;
    pub const LIST: u8 = 0x02;
    pub const STATS: u8 = 0x03;
    pub const BEGIN: u8 = 0x10;
    pub const DATA: u8 = 0x11;
    pub const END: u8 = 0x12;
    pub const ABORT: u8 = 0x14;
    pub const DELETE: u8 = 0x20;
    pub const SELECT: u8 = 0x21;
    pub const OPTIONS: u8 = 0x22;
    /// Set on the opcode of a reply.
    pub const REPLY: u8 = 0x80;
    pub const PROGRESS: u8 = 0xC0;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Ok = 0,
    BadRequest = 1,
    NoSpace = 2,
    Busy = 3,
    Io = 4,
    Crc = 5,
    NotFound = 6,
    Unsupported = 7,
    /// The pack is not one this watch can show (corrupt, wrong screen, too large to load).
    Invalid = 8,
    /// Data arrived at an offset the watch did not expect; it says where to resume.
    Sequence = 9,
}

impl Status {
    pub fn from_u8(v: u8) -> Status {
        match v {
            0 => Status::Ok,
            1 => Status::BadRequest,
            2 => Status::NoSpace,
            3 => Status::Busy,
            4 => Status::Io,
            5 => Status::Crc,
            6 => Status::NotFound,
            7 => Status::Unsupported,
            8 => Status::Invalid,
            _ => Status::Sequence,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Status::Ok => "成功",
            Status::BadRequest => "请求无效",
            Status::NoSpace => "手表空间不足",
            Status::Busy => "手表正忙",
            Status::Io => "手表写入失败",
            Status::Crc => "校验失败",
            Status::NotFound => "手表上没有这个角色",
            Status::Unsupported => "手表模块不支持",
            Status::Invalid => "角色包无法在这块手表上使用",
            Status::Sequence => "数据顺序错乱",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hello {
    pub version: u8,
    pub caps: u8,
    pub max_data: u16,
    /// `u32::MAX` when the firmware does not say.
    pub free_kib: u32,
    pub width: u16,
    pub height: u16,
    pub target: u8,
    /// The pack shown now; 0 for none.
    pub active: u32,
    pub packs: u8,
    pub options: u8,
}

/// What the watch measured over its last one-second window (all `u32`, little endian, in this
/// order on the wire).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames drawn since the pack loaded.
    pub frames: u32,
    /// Frames per second times ten.
    pub fps_x10: u32,
    /// Average milliseconds per frame: animation, drawing (CPU, driver calls included), the
    /// part of that inside the driver's draw calls, and waiting for the GPU at the end.
    pub update_ms: u32,
    pub draw_ms: u32,
    pub driver_ms: u32,
    pub gpu_ms: u32,
    pub triangles: u32,
    pub calls: u32,
    pub errors: u32,
    pub heap_free_kib: u32,
    /// The shown pack, 0 for none.
    pub pack: u32,
    /// Bit 0: the baked background is in use; bit 1: the view is attached; bit 2: visible;
    /// bit 3: a pack is loading; bits 8..15: why the last load failed (errno-like, 0: it did
    /// not: 2 no pack, 5 / 22 unreadable, 12 out of memory); bits 16..23: the GPU self-test
    /// (16 ran, 17 could not check, 18 FP32 paths, 19 non-cached writes, 20 32-bit pages,
    /// 21 no PAD edges, 22 blend results off, 23 other results off).
    pub flags: u32,
    /// Average milliseconds LVGL takes to redraw the screen after a frame (the watch's own
    /// share of every frame). Optional on the wire (0 from older modules).
    pub ui_ms: u32,
    /// The GPU self-test's probes that passed (bit per probe, see the module's selftest.rs; 0
    /// from older modules or before it ran).
    pub selftest: u32,
    /// Average milliseconds per frame left idle on purpose (pacing), and how late the frame
    /// timer fired past that (0 from older modules).
    pub idle_ms: u32,
    pub late_ms: u32,
}

impl Stats {
    const WORDS: usize = 16;
    /// Words every reply has (`ui_ms` came later).
    const REQUIRED: usize = 12;

    fn words(&self) -> [u32; Self::WORDS] {
        [
            self.frames,
            self.fps_x10,
            self.update_ms,
            self.draw_ms,
            self.driver_ms,
            self.gpu_ms,
            self.triangles,
            self.calls,
            self.errors,
            self.heap_free_kib,
            self.pack,
            self.flags,
            self.ui_ms,
            self.selftest,
            self.idle_ms,
            self.late_ms,
        ]
    }

    fn from_words(w: [u32; Self::WORDS]) -> Stats {
        Stats {
            frames: w[0],
            fps_x10: w[1],
            update_ms: w[2],
            draw_ms: w[3],
            driver_ms: w[4],
            gpu_ms: w[5],
            triangles: w[6],
            calls: w[7],
            errors: w[8],
            heap_free_kib: w[9],
            pack: w[10],
            flags: w[11],
            ui_ms: w[12],
            selftest: w[13],
            idle_ms: w[14],
            late_ms: w[15],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackInfo {
    pub id: u32,
    pub size: u32,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Hello { req: u16, version: u8 },
    List { req: u16, offset: u8, max: u8 },
    Stats { req: u16 },
    Begin { req: u16, id: u32, size: u32, crc32: u32, name: String },
    Data { offset: u32, bytes: Vec<u8> },
    End { req: u16 },
    Abort { req: u16 },
    Delete { req: u16, id: u32 },
    Select { req: u16, id: u32 },
    Options { req: u16, options: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Hello { req: u16, status: Status, hello: Hello },
    List { req: u16, status: Status, total: u8, offset: u8, packs: Vec<PackInfo> },
    Stats { req: u16, status: Status, stats: Stats },
    Ack { op: u8, req: u16, status: Status, value: u32 },
}

impl Reply {
    pub fn req(&self) -> u16 {
        match self {
            Reply::Hello { req, .. } | Reply::List { req, .. } | Reply::Stats { req, .. } | Reply::Ack { req, .. } => *req,
        }
    }

    pub fn status(&self) -> Status {
        match self {
            Reply::Hello { status, .. } | Reply::List { status, .. } | Reply::Stats { status, .. } | Reply::Ack { status, .. } => *status,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The watch holds the first `next` bytes; `status` other than `Ok` means: resume at `next`
    /// (`Sequence`) or give up.
    Progress { next: u32, status: Status },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    Reply(Reply),
    Event(Event),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Short,
    Op(u8),
    Long,
}

struct W(Vec<u8>);

impl W {
    fn new(op: u8) -> W {
        let mut v = Vec::with_capacity(32);
        v.push(op);
        W(v)
    }
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn str(&mut self, s: &str) -> &mut Self {
        let s = clip(s, MAX_NAME);
        self.0.push(s.len() as u8);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
}

struct R<'a>(&'a [u8]);

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.0.len() < n {
            return Err(DecodeError::Short);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn str(&mut self) -> Result<String, DecodeError> {
        let n = self.u8()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into())
    }
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let w = match self {
            Request::Hello { req, version } => {
                let mut w = W::new(op::HELLO);
                w.u16(*req).u8(*version);
                w
            }
            Request::List { req, offset, max } => {
                let mut w = W::new(op::LIST);
                w.u16(*req).u8(*offset).u8(*max);
                w
            }
            Request::Stats { req } => {
                let mut w = W::new(op::STATS);
                w.u16(*req);
                w
            }
            Request::Begin { req, id, size, crc32, name } => {
                let mut w = W::new(op::BEGIN);
                w.u16(*req).u32(*id).u32(*size).u32(*crc32).str(name);
                w
            }
            Request::Data { offset, bytes } => {
                let mut w = W::new(op::DATA);
                w.u32(*offset);
                w.0.extend_from_slice(&bytes[..bytes.len().min(DATA_MAX)]);
                w
            }
            Request::End { req } => {
                let mut w = W::new(op::END);
                w.u16(*req);
                w
            }
            Request::Abort { req } => {
                let mut w = W::new(op::ABORT);
                w.u16(*req);
                w
            }
            Request::Delete { req, id } => {
                let mut w = W::new(op::DELETE);
                w.u16(*req).u32(*id);
                w
            }
            Request::Select { req, id } => {
                let mut w = W::new(op::SELECT);
                w.u16(*req).u32(*id);
                w
            }
            Request::Options { req, options } => {
                let mut w = W::new(op::OPTIONS);
                w.u16(*req).u8(*options);
                w
            }
        };
        w.0
    }

    pub fn decode(frame: &[u8]) -> Result<Request, DecodeError> {
        if frame.len() > FRAME_MAX {
            return Err(DecodeError::Long);
        }
        let mut r = R(frame);
        let code = r.u8()?;
        if code == op::DATA {
            let offset = r.u32()?;
            return Ok(Request::Data { offset, bytes: r.0.to_vec() });
        }
        let req = r.u16()?;
        Ok(match code {
            op::HELLO => Request::Hello { req, version: r.u8()? },
            op::LIST => Request::List { req, offset: r.u8()?, max: r.u8()? },
            op::STATS => Request::Stats { req },
            op::BEGIN => Request::Begin { req, id: r.u32()?, size: r.u32()?, crc32: r.u32()?, name: r.str()? },
            op::END => Request::End { req },
            op::ABORT => Request::Abort { req },
            op::DELETE => Request::Delete { req, id: r.u32()? },
            op::SELECT => Request::Select { req, id: r.u32()? },
            op::OPTIONS => Request::Options { req, options: r.u8()? },
            other => return Err(DecodeError::Op(other)),
        })
    }

    /// The opcode and request number a reply to this request carries.
    pub fn op_req(&self) -> (u8, u16) {
        match self {
            Request::Hello { req, .. } => (op::HELLO, *req),
            Request::List { req, .. } => (op::LIST, *req),
            Request::Stats { req } => (op::STATS, *req),
            Request::Begin { req, .. } => (op::BEGIN, *req),
            Request::Data { .. } => (op::DATA, 0),
            Request::End { req } => (op::END, *req),
            Request::Abort { req } => (op::ABORT, *req),
            Request::Delete { req, .. } => (op::DELETE, *req),
            Request::Select { req, .. } => (op::SELECT, *req),
            Request::Options { req, .. } => (op::OPTIONS, *req),
        }
    }
}

impl Reply {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Reply::Hello { req, status, hello: h } => {
                let mut w = W::new(op::HELLO | op::REPLY);
                w.u16(*req)
                    .u8(*status as u8)
                    .u8(h.version)
                    .u8(h.caps)
                    .u16(h.max_data)
                    .u32(h.free_kib)
                    .u16(h.width)
                    .u16(h.height)
                    .u8(h.target)
                    .u32(h.active)
                    .u8(h.packs)
                    .u8(h.options);
                w.0
            }
            Reply::List { req, status, total, offset, packs } => {
                let mut w = W::new(op::LIST | op::REPLY);
                w.u16(*req).u8(*status as u8).u8(*total).u8(*offset);
                let count_at = w.0.len();
                w.u8(0);
                let mut count = 0u8;
                for p in packs {
                    let need = 9 + crate::clip(&p.name, MAX_NAME).len();
                    if w.0.len() + need > FRAME_MAX {
                        break;
                    }
                    w.u32(p.id).u32(p.size).str(&p.name);
                    count += 1;
                }
                w.0[count_at] = count;
                w.0
            }
            Reply::Stats { req, status, stats } => {
                let mut w = W::new(op::STATS | op::REPLY);
                w.u16(*req).u8(*status as u8);
                for x in stats.words() {
                    w.u32(x);
                }
                w.0
            }
            Reply::Ack { op: code, req, status, value } => {
                let mut w = W::new(code | op::REPLY);
                w.u16(*req).u8(*status as u8).u32(*value);
                w.0
            }
        }
    }

    /// How many of `packs` fit in one `LIST` reply.
    pub fn list_fits(packs: &[PackInfo]) -> usize {
        let mut len = 7;
        let mut n = 0;
        for p in packs {
            len += 9 + crate::clip(&p.name, MAX_NAME).len();
            if len > FRAME_MAX {
                break;
            }
            n += 1;
        }
        n
    }
}

impl Event {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Event::Progress { next, status } => {
                let mut w = W::new(op::PROGRESS);
                w.u32(*next).u8(*status as u8);
                w.0
            }
        }
    }
}

impl Incoming {
    pub fn decode(frame: &[u8]) -> Result<Incoming, DecodeError> {
        let mut r = R(frame);
        let code = r.u8()?;
        if code == op::PROGRESS {
            let next = r.u32()?;
            let status = Status::from_u8(r.u8()?);
            return Ok(Incoming::Event(Event::Progress { next, status }));
        }
        if code & op::REPLY == 0 || code >= 0xC0 {
            return Err(DecodeError::Op(code));
        }
        let req = r.u16()?;
        let status = Status::from_u8(r.u8()?);
        let request = code & !op::REPLY;
        Ok(Incoming::Reply(match request {
            op::HELLO if status == Status::Ok => Reply::Hello {
                req,
                status,
                hello: Hello {
                    version: r.u8()?,
                    caps: r.u8()?,
                    max_data: r.u16()?,
                    free_kib: r.u32()?,
                    width: r.u16()?,
                    height: r.u16()?,
                    target: r.u8()?,
                    active: r.u32()?,
                    packs: r.u8()?,
                    options: r.u8()?,
                },
            },
            op::LIST if status == Status::Ok => {
                let total = r.u8()?;
                let offset = r.u8()?;
                let count = r.u8()?;
                let mut packs = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    packs.push(PackInfo { id: r.u32()?, size: r.u32()?, name: r.str()? });
                }
                Reply::List { req, status, total, offset, packs }
            }
            op::STATS if status == Status::Ok => {
                let mut words = [0u32; Stats::WORDS];
                for (i, x) in words.iter_mut().enumerate() {
                    *x = if i < Stats::REQUIRED { r.u32()? } else { r.u32().unwrap_or(0) };
                }
                Reply::Stats { req, status, stats: Stats::from_words(words) }
            }
            op::HELLO | op::LIST | op::STATS => Reply::Ack { op: request, req, status, value: 0 },
            _ => Reply::Ack { op: request, req, status, value: r.u32().unwrap_or(0) },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn requests_round_trip() {
        let all = vec![
            Request::Hello { req: 1, version: 1 },
            Request::List { req: 2, offset: 3, max: 8 },
            Request::Stats { req: 10 },
            Request::Begin { req: 3, id: 0xdead_beef, size: 123_456, crc32: 7, name: "白子（骑行）".into() },
            Request::Data { offset: 4096, bytes: vec![9; DATA_MAX] },
            Request::End { req: 4 },
            Request::Abort { req: 5 },
            Request::Delete { req: 6, id: 1 },
            Request::Select { req: 7, id: 2 },
            Request::Options { req: 8, options: 1 },
        ];
        for r in all {
            let f = r.encode();
            assert!(f.len() <= FRAME_MAX);
            assert_eq!(Request::decode(&f).unwrap(), r);
        }
    }

    #[test]
    fn replies_and_events_round_trip() {
        let hello = Reply::Hello {
            req: 1,
            status: Status::Ok,
            hello: Hello {
                version: 1,
                caps: 1,
                max_data: DATA_MAX as u16,
                free_kib: 12_000,
                width: 336,
                height: 480,
                target: crate::target::BAND_10_PRO_043,
                active: 5,
                packs: 2,
                options: 0,
            },
        };
        assert_eq!(Incoming::decode(&hello.encode()).unwrap(), Incoming::Reply(hello));
        let packs: Vec<PackInfo> =
            (0..40).map(|i| PackInfo { id: i, size: i * 1000, name: alloc::format!("角色 {i}") }).collect();
        let list = Reply::List { req: 2, status: Status::Ok, total: 40, offset: 0, packs: packs.clone() };
        let f = list.encode();
        assert!(f.len() <= FRAME_MAX);
        let fits = Reply::list_fits(&packs);
        match Incoming::decode(&f).unwrap() {
            Incoming::Reply(Reply::List { packs: got, total: 40, .. }) => {
                assert_eq!(got.len(), fits);
                assert_eq!(&got[..], &packs[..fits]);
            }
            other => panic!("{other:?}"),
        }
        let st = Reply::Stats { req: 3, status: Status::Ok, stats: Stats { fps_x10: 287, calls: 300, flags: 5, ..Default::default() } };
        assert_eq!(Incoming::decode(&st.encode()).unwrap(), Incoming::Reply(st));
        let ack = Reply::Ack { op: op::END, req: 9, status: Status::Crc, value: 77 };
        assert_eq!(Incoming::decode(&ack.encode()).unwrap(), Incoming::Reply(ack));
        let refused = Reply::Ack { op: op::HELLO, req: 1, status: Status::Unsupported, value: 0 };
        assert_eq!(Incoming::decode(&refused.encode()).unwrap(), Incoming::Reply(refused));
        let ev = Event::Progress { next: 8192, status: Status::Sequence };
        assert_eq!(Incoming::decode(&ev.encode()).unwrap(), Incoming::Event(ev));
    }
}

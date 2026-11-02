//! The watch's half: turns requests into store operations and reply frames. It does no I/O of
//! its own and no timing (the module drops a stalled transfer with [`Receiver::reset`]).

use alloc::vec::Vec;

use crate::codec::*;
use crate::{Crc32, DATA_MAX, MAX_PACK_BYTES, MAX_PACKS, PROGRESS_STEP, VERSION};

/// Where packs live; implemented by the module over its file service and in memory by the tests.
pub trait Store {
    /// Starts a fresh staging file for a pack of `size` bytes.
    fn stage_begin(&mut self, size: u32) -> Result<(), Status>;
    /// Appends to the staging file.
    fn stage_write(&mut self, bytes: &[u8]) -> Result<(), Status>;
    /// Flushes and closes the staging file.
    fn stage_finish(&mut self) -> Result<(), Status>;
    /// Checks the staged pack and installs it as `id` (replacing one with that id).
    fn stage_commit(&mut self, id: u32, name: &str) -> Result<(), Status>;
    fn stage_abort(&mut self);
    fn delete(&mut self, id: u32) -> Result<(), Status>;
    fn select(&mut self, id: u32) -> Result<(), Status>;
    fn packs(&mut self) -> Vec<PackInfo>;
    fn active(&mut self) -> u32;
    /// `u32::MAX` when unknown.
    fn free_kib(&mut self) -> u32;
    fn options(&mut self) -> u8;
    fn set_options(&mut self, options: u8) -> Result<(), Status>;
    /// The player's latest measurements (`None`: not running).
    fn stats(&mut self) -> Option<Stats> {
        None
    }
}

/// What this build of the module runs on.
#[derive(Clone, Copy, Debug)]
pub struct Platform {
    pub width: u16,
    pub height: u16,
    pub target: u8,
    pub caps: u8,
}

/// What the module has to act on after a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Installed(u32),
    Selected(u32),
    Deleted(u32),
    Options(u8),
}

#[derive(Default, Debug)]
pub struct Outcome {
    pub frames: Vec<Vec<u8>>,
    pub change: Option<Change>,
}

impl Outcome {
    fn ack(&mut self, op: u8, req: u16, status: Status, value: u32) {
        self.frames.push(Reply::Ack { op, req, status, value }.encode());
    }

    fn progress(&mut self, next: u32, status: Status) {
        self.frames.push(Event::Progress { next, status }.encode());
    }
}

/// Frames that may arrive ahead of the expected one before the phone is told to resume: the phone
/// keeps several frames in flight and the host may reorder a burst.
const REORDER_FRAMES: usize = 4;

struct Transfer {
    id: u32,
    name: alloc::string::String,
    size: u32,
    crc32: u32,
    next: u32,
    crc: Crc32,
    confirmed: u32,
    /// Frames ahead of `next`, waiting for the gap to fill.
    pending: Vec<(u32, Vec<u8>)>,
    failed: bool,
}

pub struct Receiver {
    platform: Platform,
    transfer: Option<Transfer>,
}

impl Receiver {
    pub fn new(platform: Platform) -> Self {
        Self { platform, transfer: None }
    }

    pub fn is_receiving(&self) -> bool {
        self.transfer.is_some()
    }

    /// Drops a half-finished transfer (the phone went away).
    pub fn reset<S: Store>(&mut self, store: &mut S) {
        if self.transfer.take().is_some() {
            store.stage_abort();
        }
    }

    pub fn handle<S: Store>(&mut self, request: Request, store: &mut S) -> Outcome {
        let mut out = Outcome::default();
        match request {
            Request::Hello { req, version } => {
                if version == 0 || version > VERSION {
                    out.ack(op::HELLO, req, Status::Unsupported, VERSION as u32);
                } else {
                    let packs = store.packs().len().min(255) as u8;
                    let hello = Hello {
                        version: VERSION,
                        caps: self.platform.caps,
                        max_data: DATA_MAX as u16,
                        free_kib: store.free_kib(),
                        width: self.platform.width,
                        height: self.platform.height,
                        target: self.platform.target,
                        active: store.active(),
                        packs,
                        options: store.options(),
                    };
                    out.frames.push(Reply::Hello { req, status: Status::Ok, hello }.encode());
                }
            }
            Request::List { req, offset, max } => {
                let packs = store.packs();
                let start = (offset as usize).min(packs.len());
                let end = (start + max.max(1) as usize).min(packs.len());
                let reply = Reply::List {
                    req,
                    status: Status::Ok,
                    total: packs.len().min(255) as u8,
                    offset: start as u8,
                    packs: packs[start..end].to_vec(),
                };
                out.frames.push(reply.encode());
            }
            Request::Stats { req } => match store.stats() {
                Some(stats) => out.frames.push(Reply::Stats { req, status: Status::Ok, stats }.encode()),
                None => out.ack(op::STATS, req, Status::Unsupported, 0),
            },
            Request::Begin { req, id, size, crc32, name } => {
                self.reset(store);
                let status = self.begin(store, id, size, crc32, name);
                out.ack(op::BEGIN, req, status, 0);
            }
            Request::Data { offset, bytes } => self.data(store, offset, bytes, &mut out),
            Request::End { req } => {
                let (status, value) = self.end(store);
                out.ack(op::END, req, status, value);
                if status == Status::Ok {
                    out.change = Some(Change::Installed(value));
                }
            }
            Request::Abort { req } => {
                self.reset(store);
                out.ack(op::ABORT, req, Status::Ok, 0);
            }
            Request::Delete { req, id } => {
                let status = if self.transfer.is_some() { Status::Busy } else { status_of(store.delete(id)) };
                out.ack(op::DELETE, req, status, id);
                if status == Status::Ok {
                    out.change = Some(Change::Deleted(id));
                }
            }
            Request::Select { req, id } => {
                let status = status_of(store.select(id));
                out.ack(op::SELECT, req, status, id);
                if status == Status::Ok {
                    out.change = Some(Change::Selected(id));
                }
            }
            Request::Options { req, options } => {
                let status = status_of(store.set_options(options));
                let now = store.options();
                out.ack(op::OPTIONS, req, status, now as u32);
                if status == Status::Ok {
                    out.change = Some(Change::Options(now));
                }
            }
        }
        out
    }

    fn begin<S: Store>(&mut self, store: &mut S, id: u32, size: u32, crc32: u32, name: alloc::string::String) -> Status {
        if id == 0 || size < 32 || size > MAX_PACK_BYTES {
            return Status::BadRequest;
        }
        let packs = store.packs();
        if packs.len() >= MAX_PACKS && !packs.iter().any(|p| p.id == id) {
            return Status::NoSpace;
        }
        let free = store.free_kib();
        // The staged copy and the installed one coexist until the commit.
        if free != u32::MAX && size / 1024 + 64 > free {
            return Status::NoSpace;
        }
        if let Err(status) = store.stage_begin(size) {
            return status;
        }
        self.transfer = Some(Transfer {
            id,
            name,
            size,
            crc32,
            next: 0,
            crc: Crc32::new(),
            confirmed: 0,
            pending: Vec::new(),
            failed: false,
        });
        Status::Ok
    }

    fn data<S: Store>(&mut self, store: &mut S, offset: u32, bytes: Vec<u8>, out: &mut Outcome) {
        let Some(t) = self.transfer.as_mut() else {
            out.progress(0, Status::BadRequest);
            return;
        };
        if t.failed || bytes.is_empty() {
            return;
        }
        let end = offset as u64 + bytes.len() as u64;
        if end > t.size as u64 {
            t.failed = true;
            out.progress(t.next, Status::BadRequest);
            return;
        }
        if end <= t.next as u64 {
            // A repeat of something already written (frames in flight across a resume).
            return;
        }
        if offset > t.next {
            let ahead = offset - t.next;
            let held = t.pending.iter().any(|(o, _)| *o == offset);
            if !held && ahead as usize <= REORDER_FRAMES * DATA_MAX && t.pending.len() < REORDER_FRAMES {
                t.pending.push((offset, bytes));
            } else if !held {
                t.pending.clear();
                out.progress(t.next, Status::Sequence);
            }
            return;
        }
        // Overlaps or starts at `next`: write the new part, then whatever was held for after it.
        let skip = (t.next - offset) as usize;
        if let Err(status) = write(store, t, &bytes[skip..]) {
            t.failed = true;
            out.progress(t.next, status);
            return;
        }
        loop {
            let Some(i) = t.pending.iter().position(|(o, b)| *o <= t.next && *o as usize + b.len() > t.next as usize)
            else {
                break;
            };
            let (o, b) = t.pending.swap_remove(i);
            let skip = (t.next - o) as usize;
            if let Err(status) = write(store, t, &b[skip..]) {
                t.failed = true;
                out.progress(t.next, status);
                return;
            }
        }
        let next = t.next;
        t.pending.retain(|(o, b)| *o as usize + b.len() > next as usize);
        if t.next - t.confirmed >= PROGRESS_STEP || t.next == t.size {
            t.confirmed = t.next;
            out.progress(t.next, Status::Ok);
        }
    }

    fn end<S: Store>(&mut self, store: &mut S) -> (Status, u32) {
        let Some(t) = self.transfer.as_ref() else { return (Status::BadRequest, 0) };
        if t.failed {
            self.reset(store);
            return (Status::Io, 0);
        }
        if t.next != t.size {
            return (Status::Sequence, t.next);
        }
        let t = self.transfer.take().unwrap();
        if t.crc.finish() != t.crc32 {
            store.stage_abort();
            return (Status::Crc, 0);
        }
        if let Err(status) = store.stage_finish() {
            store.stage_abort();
            return (status, 0);
        }
        match store.stage_commit(t.id, &t.name) {
            Ok(()) => (Status::Ok, t.id),
            Err(status) => {
                store.stage_abort();
                (status, 0)
            }
        }
    }
}

fn write<S: Store>(store: &mut S, t: &mut Transfer, bytes: &[u8]) -> Result<(), Status> {
    store.stage_write(bytes)?;
    t.crc.update(bytes);
    t.next += bytes.len() as u32;
    Ok(())
}

fn status_of(r: Result<(), Status>) -> Status {
    match r {
        Ok(()) => Status::Ok,
        Err(s) => s,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;

    /// A store in memory; `commit` checks the BL2D magic the way the module does.
    #[derive(Default)]
    pub struct Memory {
        pub stage: Option<Vec<u8>>,
        pub packs: Vec<(PackInfo, Vec<u8>)>,
        pub active: u32,
        pub options: u8,
        pub writes: usize,
        pub fail_write_at: Option<usize>,
    }

    impl Store for Memory {
        fn stage_begin(&mut self, _: u32) -> Result<(), Status> {
            self.stage = Some(Vec::new());
            Ok(())
        }
        fn stage_write(&mut self, bytes: &[u8]) -> Result<(), Status> {
            self.writes += 1;
            if self.fail_write_at == Some(self.writes) {
                return Err(Status::Io);
            }
            self.stage.as_mut().ok_or(Status::Io)?.extend_from_slice(bytes);
            Ok(())
        }
        fn stage_finish(&mut self) -> Result<(), Status> {
            Ok(())
        }
        fn stage_commit(&mut self, id: u32, name: &str) -> Result<(), Status> {
            let data = self.stage.take().ok_or(Status::Io)?;
            if !data.starts_with(b"BL2D") {
                return Err(Status::Invalid);
            }
            self.packs.retain(|(p, _)| p.id != id);
            self.packs.push((PackInfo { id, size: data.len() as u32, name: String::from(name) }, data));
            Ok(())
        }
        fn stage_abort(&mut self) {
            self.stage = None;
        }
        fn delete(&mut self, id: u32) -> Result<(), Status> {
            let n = self.packs.len();
            self.packs.retain(|(p, _)| p.id != id);
            if self.packs.len() == n {
                return Err(Status::NotFound);
            }
            if self.active == id {
                self.active = 0;
            }
            Ok(())
        }
        fn select(&mut self, id: u32) -> Result<(), Status> {
            if !self.packs.iter().any(|(p, _)| p.id == id) {
                return Err(Status::NotFound);
            }
            self.active = id;
            Ok(())
        }
        fn packs(&mut self) -> Vec<PackInfo> {
            self.packs.iter().map(|(p, _)| p.clone()).collect()
        }
        fn active(&mut self) -> u32 {
            self.active
        }
        fn free_kib(&mut self) -> u32 {
            u32::MAX
        }
        fn options(&mut self) -> u8 {
            self.options
        }
        fn set_options(&mut self, options: u8) -> Result<(), Status> {
            self.options = options;
            Ok(())
        }
    }

    pub fn platform() -> Platform {
        Platform { width: 336, height: 480, target: crate::target::BAND_10_PRO_043, caps: crate::caps::STREAM }
    }

    pub fn pack(len: usize) -> Vec<u8> {
        let mut v: Vec<u8> = (0..len).map(|i| (i * 7 + i / 300) as u8).collect();
        v[..4].copy_from_slice(b"BL2D");
        v
    }

    fn data(offset: usize, p: &[u8]) -> Request {
        Request::Data { offset: offset as u32, bytes: p[offset..(offset + DATA_MAX).min(p.len())].to_vec() }
    }

    fn progress(out: &Outcome) -> Vec<(u32, Status)> {
        out.frames
            .iter()
            .filter_map(|f| match Incoming::decode(f) {
                Ok(Incoming::Event(Event::Progress { next, status })) => Some((next, status)),
                _ => None,
            })
            .collect()
    }

    fn ack(out: &Outcome) -> (Status, u32) {
        match Incoming::decode(&out.frames[0]).unwrap() {
            Incoming::Reply(Reply::Ack { status, value, .. }) => (status, value),
            other => panic!("{other:?}"),
        }
    }

    fn begin(rx: &mut Receiver, m: &mut Memory, p: &[u8]) {
        let out = rx.handle(
            Request::Begin { req: 1, id: 42, size: p.len() as u32, crc32: crate::crc32(p), name: "白子".into() },
            m,
        );
        assert_eq!(ack(&out).0, Status::Ok);
    }

    #[test]
    fn frames_in_order_install_the_pack() {
        let (mut rx, mut m) = (Receiver::new(platform()), Memory::default());
        let p = pack(10_000);
        begin(&mut rx, &mut m, &p);
        let mut events = Vec::new();
        for o in (0..p.len()).step_by(DATA_MAX) {
            events.extend(progress(&rx.handle(data(o, &p), &mut m)));
        }
        assert!(events.iter().all(|(_, s)| *s == Status::Ok));
        assert_eq!(events.last().unwrap().0, 10_000);
        let out = rx.handle(Request::End { req: 2 }, &mut m);
        assert_eq!(ack(&out), (Status::Ok, 42));
        assert_eq!(out.change, Some(Change::Installed(42)));
        assert_eq!(m.packs[0].1, p);
        assert_eq!(m.packs[0].0.name, "白子");
    }

    #[test]
    fn early_frames_wait_for_the_gap_and_repeats_are_ignored() {
        let (mut rx, mut m) = (Receiver::new(platform()), Memory::default());
        let p = pack(5 * DATA_MAX);
        begin(&mut rx, &mut m, &p);
        for o in [2, 1, 3].map(|i| i * DATA_MAX) {
            assert!(progress(&rx.handle(data(o, &p), &mut m)).is_empty());
        }
        rx.handle(data(0, &p), &mut m);
        assert!(progress(&rx.handle(data(DATA_MAX, &p), &mut m)).is_empty());
        let last = progress(&rx.handle(data(4 * DATA_MAX, &p), &mut m));
        assert_eq!(last, vec![(p.len() as u32, Status::Ok)]);
        assert_eq!(ack(&rx.handle(Request::End { req: 2 }, &mut m)).0, Status::Ok);
        assert_eq!(m.packs[0].1, p);
    }

    #[test]
    fn a_gap_too_far_asks_the_phone_to_resume() {
        let (mut rx, mut m) = (Receiver::new(platform()), Memory::default());
        let p = pack(20 * DATA_MAX);
        begin(&mut rx, &mut m, &p);
        rx.handle(data(0, &p), &mut m);
        let out = rx.handle(data(10 * DATA_MAX, &p), &mut m);
        assert_eq!(progress(&out), vec![(DATA_MAX as u32, Status::Sequence)]);
        // END before everything arrived is refused with where the watch is.
        assert_eq!(ack(&rx.handle(Request::End { req: 2 }, &mut m)), (Status::Sequence, DATA_MAX as u32));
    }

    #[test]
    fn a_wrong_crc_or_a_bad_pack_is_not_installed() {
        let (mut rx, mut m) = (Receiver::new(platform()), Memory::default());
        let p = pack(1000);
        rx.handle(Request::Begin { req: 1, id: 7, size: 1000, crc32: 1, name: String::new() }, &mut m);
        rx.handle(data(0, &p), &mut m);
        rx.handle(data(DATA_MAX, &p), &mut m);
        rx.handle(data(2 * DATA_MAX, &p), &mut m);
        assert_eq!(ack(&rx.handle(Request::End { req: 2 }, &mut m)).0, Status::Crc);
        assert!(m.packs.is_empty() && m.stage.is_none());
        let mut q = p.clone();
        q[0] = b'X';
        rx.handle(Request::Begin { req: 3, id: 7, size: 1000, crc32: crate::crc32(&q), name: String::new() }, &mut m);
        for o in (0..1000).step_by(DATA_MAX) {
            rx.handle(data(o, &q), &mut m);
        }
        assert_eq!(ack(&rx.handle(Request::End { req: 4 }, &mut m)).0, Status::Invalid);
        assert!(m.packs.is_empty());
    }

    #[test]
    fn a_write_error_fails_the_transfer() {
        let (mut rx, mut m) = (Receiver::new(platform()), Memory { fail_write_at: Some(2), ..Default::default() });
        let p = pack(2000);
        begin(&mut rx, &mut m, &p);
        rx.handle(data(0, &p), &mut m);
        let out = rx.handle(data(DATA_MAX, &p), &mut m);
        assert_eq!(progress(&out), vec![(DATA_MAX as u32, Status::Io)]);
        assert_eq!(ack(&rx.handle(Request::End { req: 2 }, &mut m)).0, Status::Io);
        assert!(!rx.is_receiving());
    }

    #[test]
    fn hello_list_select_delete_options() {
        let (mut rx, mut m) = (Receiver::new(platform()), Memory::default());
        let p = pack(600);
        begin(&mut rx, &mut m, &p);
        rx.handle(data(0, &p), &mut m);
        rx.handle(data(DATA_MAX, &p), &mut m);
        rx.handle(Request::End { req: 2 }, &mut m);
        let out = rx.handle(Request::Select { req: 3, id: 42 }, &mut m);
        assert_eq!(out.change, Some(Change::Selected(42)));
        match Incoming::decode(&rx.handle(Request::Hello { req: 4, version: 1 }, &mut m).frames[0]).unwrap() {
            Incoming::Reply(Reply::Hello { hello, .. }) => {
                assert_eq!((hello.active, hello.packs, hello.width), (42, 1, 336));
            }
            other => panic!("{other:?}"),
        }
        match Incoming::decode(&rx.handle(Request::List { req: 5, offset: 0, max: 10 }, &mut m).frames[0]).unwrap() {
            Incoming::Reply(Reply::List { packs, total: 1, .. }) => assert_eq!(packs[0].size, 600),
            other => panic!("{other:?}"),
        }
        assert_eq!(ack(&rx.handle(Request::Hello { req: 6, version: 9 }, &mut m)).0, Status::Unsupported);
        assert_eq!(ack(&rx.handle(Request::Select { req: 7, id: 5 }, &mut m)).0, Status::NotFound);
        let out = rx.handle(Request::Options { req: 8, options: 1 }, &mut m);
        assert_eq!((ack(&out), out.change), ((Status::Ok, 1), Some(Change::Options(1))));
        let out = rx.handle(Request::Delete { req: 9, id: 42 }, &mut m);
        assert_eq!(out.change, Some(Change::Deleted(42)));
        assert_eq!(m.active, 0);
    }
}

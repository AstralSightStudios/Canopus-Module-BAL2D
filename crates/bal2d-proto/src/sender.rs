//! The phone's half of an install: turns a pack into the frames the watch expects and follows
//! the watch's replies and progress events. It does no I/O and no timing: the host sends what
//! [`Sender::poll`] returns, feeds back what arrives with [`Sender::on_frame`], calls
//! [`Sender::on_timeout`] when the watch has been quiet for a while, and decides when to give up.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::codec::*;
use crate::{Crc32, DATA_MAX, WINDOW};

/// Random-access bytes of the pack.
pub trait Source: Send {
    fn len(&self) -> u32;
    /// Reads up to `buf.len()` bytes at `offset`; 0 only at the end.
    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<usize, String>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Source for Vec<u8> {
    fn len(&self) -> u32 {
        self.as_slice().len() as u32
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<usize, String> {
        let start = (offset as usize).min(self.as_slice().len());
        let n = buf.len().min(self.as_slice().len() - start);
        buf[..n].copy_from_slice(&self[start..start + n]);
        Ok(n)
    }
}

fn crc_of(source: &mut dyn Source) -> Result<u32, String> {
    let mut crc = Crc32::new();
    let mut offset = 0;
    let mut buf = vec![0u8; 4096];
    loop {
        let n = source.read(offset, &mut buf)?;
        if n == 0 {
            return Ok(crc.finish());
        }
        crc.update(&buf[..n]);
        offset += n as u32;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// `BEGIN` is next.
    Start,
    AwaitBegin,
    Streaming,
    AwaitEnd,
    AwaitSelect,
    Done,
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The watch refused with this status.
    Refused(Status),
    /// The pack could not be read.
    Source,
    /// The watch answered something that does not fit the transfer.
    Protocol,
}

pub struct Sender {
    id: u32,
    name: String,
    size: u32,
    crc32: u32,
    source: Box<dyn Source>,
    select: bool,
    state: State,
    /// Next byte to send.
    cursor: u32,
    /// Bytes the watch has confirmed.
    confirmed: u32,
    req: u16,
    expected: (u8, u16),
    queue: VecDeque<Vec<u8>>,
    resumes: u32,
    /// After a resume, repeats of the same complaint (frames already in flight) are ignored.
    rewound_to: Option<u32>,
    selected: bool,
}

impl Sender {
    /// Installs `source` as pack `id`; with `select` it also becomes the pack the watch shows.
    pub fn new(id: u32, name: &str, mut source: Box<dyn Source>, select: bool) -> Result<Sender, String> {
        let crc32 = crc_of(source.as_mut())?;
        Ok(Sender {
            id,
            name: name.into(),
            size: source.len(),
            crc32,
            source,
            select,
            state: State::Start,
            cursor: 0,
            confirmed: 0,
            req: 0,
            expected: (0, 0),
            queue: VecDeque::new(),
            resumes: 0,
            rewound_to: None,
            selected: false,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Done | State::Failed(_))
    }

    /// (bytes the watch confirmed, pack size).
    pub fn progress(&self) -> (u32, u32) {
        (self.confirmed, self.size)
    }

    /// How often the watch asked to resume from an earlier offset (or the host timed out).
    pub fn resumes(&self) -> u32 {
        self.resumes
    }

    /// Whether the pack became the one shown (when asked for).
    pub fn selected(&self) -> bool {
        self.selected
    }

    pub fn crc32(&self) -> u32 {
        self.crc32
    }

    fn request(&mut self, make: impl FnOnce(u16) -> Request) {
        self.req = self.req.wrapping_add(1);
        let r = make(self.req);
        self.expected = r.op_req();
        self.queue.push_back(r.encode());
    }

    fn fail(&mut self, failure: Failure) {
        self.state = State::Failed(failure);
        self.queue.clear();
    }

    /// The next frame to send, if any is due now.
    pub fn poll(&mut self) -> Option<Vec<u8>> {
        if let Some(f) = self.queue.pop_front() {
            return Some(f);
        }
        match self.state {
            State::Start => {
                let (id, size, crc32, name) = (self.id, self.size, self.crc32, self.name.clone());
                self.request(|req| Request::Begin { req, id, size, crc32, name });
                self.state = State::AwaitBegin;
                self.queue.pop_front()
            }
            State::Streaming => {
                if self.cursor < self.size && self.cursor - self.confirmed < WINDOW {
                    let n = (self.size - self.cursor).min(DATA_MAX as u32) as usize;
                    let mut bytes = vec![0u8; n];
                    let mut got = 0;
                    while got < n {
                        match self.source.read(self.cursor + got as u32, &mut bytes[got..]) {
                            Ok(0) | Err(_) => {
                                self.fail(Failure::Source);
                                return None;
                            }
                            Ok(k) => got += k,
                        }
                    }
                    let offset = self.cursor;
                    self.cursor += n as u32;
                    return Some(Request::Data { offset, bytes }.encode());
                }
                if self.confirmed >= self.size {
                    self.request(|req| Request::End { req });
                    self.state = State::AwaitEnd;
                    return self.queue.pop_front();
                }
                None
            }
            _ => None,
        }
    }

    /// A frame from the watch.
    pub fn on_frame(&mut self, frame: &[u8]) {
        if self.is_finished() {
            return;
        }
        match Incoming::decode(frame) {
            Ok(Incoming::Event(Event::Progress { next, status })) => self.on_progress(next, status),
            Ok(Incoming::Reply(reply)) => self.on_reply(reply),
            Err(_) => {}
        }
    }

    fn on_progress(&mut self, next: u32, status: Status) {
        if !matches!(self.state, State::Streaming | State::AwaitEnd) {
            return;
        }
        match status {
            Status::Ok => {
                if next > self.confirmed {
                    self.confirmed = next.min(self.size);
                    self.rewound_to = None;
                }
                if self.cursor < self.confirmed {
                    self.cursor = self.confirmed;
                }
            }
            Status::Sequence => {
                if self.rewound_to == Some(next) || next > self.size {
                    return;
                }
                self.rewound_to = Some(next);
                self.resumes += 1;
                self.confirmed = next;
                self.cursor = next;
            }
            other => self.fail(Failure::Refused(other)),
        }
    }

    fn on_reply(&mut self, reply: Reply) {
        let Reply::Ack { op: code, req, status, value } = reply else { return };
        if (code, req) != self.expected {
            return;
        }
        match (self.state, code) {
            (State::AwaitBegin, op::BEGIN) => {
                if status == Status::Ok {
                    self.state = State::Streaming;
                } else {
                    self.fail(Failure::Refused(status));
                }
            }
            (State::AwaitEnd, op::END) => match status {
                Status::Ok if self.select => {
                    let id = self.id;
                    self.request(|req| Request::Select { req, id });
                    self.state = State::AwaitSelect;
                }
                Status::Ok => self.state = State::Done,
                // The watch is missing data: resume where it is.
                Status::Sequence if value < self.size => {
                    self.state = State::Streaming;
                    self.resumes += 1;
                    self.confirmed = value;
                    self.cursor = value;
                    self.rewound_to = Some(value);
                }
                other => self.fail(Failure::Refused(other)),
            },
            (State::AwaitSelect, op::SELECT) => {
                self.selected = status == Status::Ok;
                self.state = State::Done;
            }
            _ => self.fail(Failure::Protocol),
        }
    }

    /// The watch has been quiet: frames may have been lost at the very end of the window, where
    /// no later frame makes the watch complain. Resend from what it confirmed.
    pub fn on_timeout(&mut self) {
        if self.state == State::Streaming && self.cursor > self.confirmed {
            self.cursor = self.confirmed;
            self.rewound_to = None;
            self.resumes += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receiver::tests::{Memory, pack, platform};
    use crate::receiver::{Change, Receiver};

    /// Runs a transfer; `drop(i)` loses the i-th phone frame, `depth` frames are in flight and
    /// each burst arrives reversed when `shuffle` (the worst reorder the watch tolerates).
    fn run(p: Vec<u8>, select: bool, drop: impl Fn(usize) -> bool, depth: usize, shuffle: bool) -> (Sender, Memory, Vec<Change>) {
        let mut tx = Sender::new(9, "白子", Box::new(p), select).unwrap();
        let (mut rx, mut store) = (Receiver::new(platform()), Memory::default());
        let mut changes = Vec::new();
        let mut sent = 0;
        let mut quiet = 0;
        while !tx.is_finished() {
            let mut burst = Vec::new();
            let mut polled = 0;
            while polled < depth {
                let Some(f) = tx.poll() else { break };
                polled += 1;
                if !drop(sent) {
                    burst.push(f);
                }
                sent += 1;
            }
            if shuffle {
                burst.reverse();
            }
            let mut replies = Vec::new();
            for f in burst {
                let out = rx.handle(Request::decode(&f).unwrap(), &mut store);
                changes.extend(out.change);
                replies.extend(out.frames);
            }
            if replies.is_empty() && polled == 0 {
                quiet += 1;
                assert!(quiet < 50, "stalled at {:?} {:?}", tx.state(), tx.progress());
                tx.on_timeout();
            } else {
                quiet = 0;
            }
            for r in replies {
                tx.on_frame(&r);
            }
        }
        (tx, store, changes)
    }

    #[test]
    fn a_pack_travels_and_is_selected() {
        let p = pack(50_000);
        let (tx, store, changes) = run(p.clone(), true, |_| false, 1, false);
        assert_eq!(tx.state(), State::Done);
        assert!(tx.selected());
        assert_eq!(store.packs[0].1, p);
        assert_eq!(store.active, 9);
        assert_eq!(changes, vec![Change::Installed(9), Change::Selected(9)]);
        assert_eq!(tx.resumes(), 0);
    }

    #[test]
    fn bursts_in_flight_and_reordered_still_deliver_intact() {
        let p = pack(123_457);
        let (tx, store, _) = run(p.clone(), false, |_| false, 4, true);
        assert_eq!(tx.state(), State::Done);
        assert_eq!(store.packs[0].1, p);
    }

    #[test]
    fn lost_frames_are_resent() {
        let p = pack(80_000);
        // Lose a frame in the middle and the very last data frame.
        let last = 1 + 80_000usize.div_ceil(DATA_MAX) - 1;
        let (tx, store, _) = run(p.clone(), false, |i| i == 40 || i == last, 8, false);
        assert_eq!(tx.state(), State::Done, "{:?}", tx.progress());
        assert!(tx.resumes() >= 2);
        assert_eq!(store.packs[0].1, p);
    }

    #[test]
    fn the_window_bounds_unconfirmed_bytes() {
        let mut tx = Sender::new(1, "", Box::new(pack(100_000)), false).unwrap();
        tx.poll();
        tx.on_frame(&Reply::Ack { op: op::BEGIN, req: 1, status: Status::Ok, value: 0 }.encode());
        let mut n = 0;
        while tx.poll().is_some() {
            n += 1;
        }
        let sent = n * DATA_MAX as u32;
        assert!((WINDOW..WINDOW + DATA_MAX as u32).contains(&sent), "{sent}");
    }

    #[test]
    fn a_refusal_fails_the_transfer() {
        let mut tx = Sender::new(1, "", Box::new(pack(1000)), false).unwrap();
        tx.poll();
        tx.on_frame(&Reply::Ack { op: op::BEGIN, req: 1, status: Status::NoSpace, value: 0 }.encode());
        assert_eq!(tx.state(), State::Failed(Failure::Refused(Status::NoSpace)));
        // A pack the watch cannot show is refused at END.
        let mut bad = pack(2000);
        bad[0] = b'X';
        let (tx, store, _) = run(bad, false, |_| false, 1, false);
        assert_eq!(tx.state(), State::Failed(Failure::Refused(Status::Invalid)));
        assert!(store.packs.is_empty());
    }
}

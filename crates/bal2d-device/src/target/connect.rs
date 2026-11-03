//! The import endpoint: the AstroBox plugin talks to the module over CanopusConnect
//! (ADR-CAN-009), one binary frame per message (`bal2d-proto`).
//!
//! Frames arrive in `on_message` on the Supervisor's owner thread and are handled there by the
//! portable [`Receiver`]; its replies go into an outbox that is flushed at once and, when the
//! Supervisor's send queue is full, again by a short background timer. The frame loop picks up
//! what changed (a new or selected pack, options) through [`super::request_reload`].

use alloc::{collections::VecDeque, format, string::String, vec::Vec};
use core::ffi::c_void;

use bal2d_proto::receiver::{Change, Platform, Receiver, Store};
use bal2d_proto::{FRAME_MAX, PackInfo, Request, Status, caps, target};
use canopus_abi::{CONNECT_BINARY, CanopusContext, ContextV1, FS_CREATE, FS_READ, FS_TRUNC, FS_WRITE};
use canopus_runtime::connect::{self, Delivery};

use super::host::{File, Fs};
use super::{CURRENT_FILE, OPTIONS_FILE, PACK_DIR};
use crate::module::context;

const ERR_BUSY: i32 = -16;
/// Received bytes are written in blocks of this size: each small append costs the flash file
/// system nearly as much as a whole block, and the handler holds up the reply meanwhile.
const WRITE_BLOCK: usize = 4 * 1024;
const OUTBOX_MAX: usize = 48;
const FLUSH_MS: u32 = 20;
/// A transfer that has not heard from the phone for this long is dropped.
const STALL_MS: u32 = 60_000;
const STAGE: &str = "packs/stage.tmp";

/// `canopus_services_v1.group_version[CANOPUS_SVC_CONNECT]`: from group version 2 the Supervisor
/// delivers frames that ask for no reply, so the phone may keep several in flight.
fn supports_stream(ctx: *const CanopusContext) -> bool {
    const CONNECT_GROUP: usize = 13;
    const STREAM_VERSION: u8 = 2;
    let services = unsafe { (*ctx).services }.cast::<u8>();
    if services.is_null() {
        return false;
    }
    let size = unsafe { services.cast::<u32>().read_unaligned() } as usize;
    size > 4 + CONNECT_GROUP && unsafe { *services.add(4 + CONNECT_GROUP) } >= STREAM_VERSION
}

/// The id a pack file answers to: `<8 hex digits>.bal2d` names it, any other file (copied by
/// hand) gets the CRC of its name.
pub fn file_id(name: &str) -> u32 {
    let stem = name.strip_suffix(".bal2d").unwrap_or(name);
    if stem.len() == 8 {
        if let Ok(id) = u32::from_str_radix(stem, 16) {
            if id != 0 {
                return id;
            }
        }
    }
    bal2d_proto::crc32(name.as_bytes()) | 1
}

pub fn pack_files() -> Vec<String> {
    let mut v: Vec<String> = Fs::list(PACK_DIR).into_iter().filter(|n| n.ends_with(".bal2d") && !n.starts_with("stage")).collect();
    // A handful of names: insertion sort keeps the generic sort out of the module image.
    for i in 1..v.len() {
        let mut j = i;
        while j > 0 && v[j - 1].as_bytes() > v[j].as_bytes() {
            v.swap(j - 1, j);
            j -= 1;
        }
    }
    v
}

fn file_of(id: u32) -> Option<String> {
    pack_files().into_iter().find(|n| file_id(n) == id)
}

/// Packs below the module directory, through the module's file service.
struct DeviceStore {
    stage: Option<File>,
    pending: Vec<u8>,
    screen: (u16, u16),
}

impl DeviceStore {
    fn write_pending(&mut self, all: bool) -> Result<(), Status> {
        let n = if all { self.pending.len() } else { self.pending.len() / WRITE_BLOCK * WRITE_BLOCK };
        if n == 0 {
            return Ok(());
        }
        let Some(file) = self.stage.as_mut() else { return Err(Status::Io) };
        let ok = file.write_all(&self.pending[..n]);
        self.pending.drain(..n);
        if ok { Ok(()) } else { Err(if file.error == -28 { Status::NoSpace } else { Status::Io }) }
    }

    /// The staged pack must be a `.bal2d` framed for this screen.
    fn check_stage(&self) -> Result<(), Status> {
        let Some(file) = Fs::open(STAGE, FS_READ) else { return Err(Status::Io) };
        let mut src = super::FileSource { file };
        let index = bal2d_pack::Index::read(&mut src).map_err(|_| Status::Invalid)?;
        let h = &index.header;
        if (h.screen_width, h.screen_height) != self.screen {
            return Err(Status::Invalid);
        }
        Ok(())
    }
}

impl Store for DeviceStore {
    fn stage_begin(&mut self, _size: u32) -> Result<(), Status> {
        self.stage = None;
        self.pending.clear();
        Fs::mkdirs(PACK_DIR);
        Fs::remove(STAGE);
        let file = Fs::open(STAGE, FS_WRITE | FS_CREATE | FS_TRUNC).ok_or(Status::Io)?;
        if self.pending.try_reserve_exact(WRITE_BLOCK + bal2d_proto::DATA_MAX).is_err() {
            return Err(Status::NoSpace);
        }
        self.stage = Some(file);
        Ok(())
    }

    fn stage_write(&mut self, bytes: &[u8]) -> Result<(), Status> {
        self.pending.extend_from_slice(bytes);
        self.write_pending(false)
    }

    fn stage_finish(&mut self) -> Result<(), Status> {
        self.write_pending(true)?;
        if let Some(mut file) = self.stage.take() {
            file.sync();
        }
        Ok(())
    }

    fn stage_commit(&mut self, id: u32, name: &str) -> Result<(), Status> {
        self.stage = None;
        self.check_stage()?;
        let stem = format!("{id:08x}");
        // Another file may answer to the same id (a copy by hand): replace it as well.
        if let Some(old) = file_of(id) {
            Fs::remove(&format!("{PACK_DIR}/{old}"));
        }
        let target = format!("{PACK_DIR}/{stem}.bal2d");
        Fs::remove(&target);
        if !Fs::rename(STAGE, &target) {
            return Err(Status::Io);
        }
        Fs::write(&format!("{PACK_DIR}/{stem}.name"), name.as_bytes());
        Ok(())
    }

    fn stage_abort(&mut self) {
        self.stage = None;
        self.pending.clear();
        Fs::remove(STAGE);
    }

    fn delete(&mut self, id: u32) -> Result<(), Status> {
        let file = file_of(id).ok_or(Status::NotFound)?;
        if !Fs::remove(&format!("{PACK_DIR}/{file}")) {
            return Err(Status::Io);
        }
        Fs::remove(&format!("{PACK_DIR}/{}.name", file.trim_end_matches(".bal2d")));
        if self.active() == id {
            Fs::remove(CURRENT_FILE);
        }
        Ok(())
    }

    fn select(&mut self, id: u32) -> Result<(), Status> {
        let file = file_of(id).ok_or(Status::NotFound)?;
        if Fs::write(CURRENT_FILE, file.as_bytes()) { Ok(()) } else { Err(Status::Io) }
    }

    fn packs(&mut self) -> Vec<PackInfo> {
        pack_files()
            .into_iter()
            .map(|file| {
                let stem = file.trim_end_matches(".bal2d");
                let name = Fs::read(&format!("{PACK_DIR}/{stem}.name"), 256)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_else(|| stem.into());
                let size = Fs::stat(&format!("{PACK_DIR}/{file}")).map(|s| s.size as u32).unwrap_or(0);
                PackInfo { id: file_id(&file), size, name }
            })
            .collect()
    }

    fn active(&mut self) -> u32 {
        super::pick_pack().map(|f| file_id(&f)).unwrap_or(0)
    }

    fn free_kib(&mut self) -> u32 {
        u32::MAX
    }

    fn options(&mut self) -> u8 {
        super::options()
    }

    fn set_options(&mut self, options: u8) -> Result<(), Status> {
        if Fs::write(OPTIONS_FILE, &[options]) { Ok(()) } else { Err(Status::Io) }
    }

    fn stats(&mut self) -> Option<bal2d_proto::Stats> {
        Some(super::stats())
    }

    fn read_log(&mut self, offset: u32, out: &mut [u8]) -> Option<(u32, usize)> {
        let total = Fs::stat(super::LOG_FILE)?.size as u32;
        if offset >= total {
            return Some((total, 0));
        }
        let mut file = Fs::open(super::LOG_FILE, FS_READ)?;
        if !file.seek(offset) {
            return None;
        }
        let want = out.len().min((total - offset) as usize);
        let mut n = 0;
        while n < want {
            let got = file.read(&mut out[n..want]);
            if got == 0 {
                break;
            }
            n += got;
        }
        Some((total, n))
    }
}

struct Endpoint {
    ctx: *const CanopusContext,
    receiver: Receiver,
    store: DeviceStore,
    outbox: VecDeque<Vec<u8>>,
    timer: i32,
    last_activity_ms: u32,
}

static mut ENDPOINT: Option<Endpoint> = None;

fn endpoint() -> Option<&'static mut Endpoint> {
    // SAFETY: the message callback and the timer callback both run on the Supervisor's owner
    // thread, serialised.
    unsafe { (*core::ptr::addr_of_mut!(ENDPOINT)).as_mut() }
}

fn now_ms(ctx: *const CanopusContext) -> u32 {
    let time = unsafe { (*ctx).time };
    if time.is_null() {
        return 0;
    }
    unsafe { ((*time).monotonic_ms)(ctx as *const ContextV1) }
}

/// Whether a pack is on its way in (the frame loop draws less meanwhile: the transfer shares
/// the CPU with it).
pub fn receiving() -> bool {
    endpoint().is_some_and(|ep| ep.receiver.is_receiving())
}

/// Registers the endpoint with the Supervisor. Targets without a CanopusConnect backend answer
/// `-38`; the module then simply cannot be fed from the phone.
pub fn start(screen: (i32, i32)) -> i32 {
    let ctx = context();
    if ctx.is_null() {
        return -22;
    }
    if endpoint().is_some() {
        return 0;
    }
    let platform = Platform {
        width: screen.0 as u16,
        height: screen.1 as u16,
        target: target::BAND_10_PRO_043,
        caps: if supports_stream(ctx) { caps::STREAM } else { 0 },
    };
    unsafe {
        ENDPOINT = Some(Endpoint {
            ctx,
            receiver: Receiver::new(platform),
            store: DeviceStore { stage: None, pending: Vec::new(), screen: (platform.width, platform.height) },
            outbox: VecDeque::new(),
            timer: -1,
            last_activity_ms: 0,
        });
    }
    match connect::listen(ctx, on_message, core::ptr::null_mut()) {
        Ok(()) => 0,
        Err(error) => error,
    }
}

unsafe extern "C" fn on_message(_ctx: *const ContextV1, kind: u32, data: *const c_void, len: u32, _user: *mut c_void) {
    if kind != CONNECT_BINARY || data.is_null() || len == 0 || len as usize > FRAME_MAX {
        return;
    }
    let frame = unsafe { core::slice::from_raw_parts(data.cast::<u8>(), len as usize) };
    let Some(ep) = endpoint() else { return };
    ep.last_activity_ms = now_ms(ep.ctx);
    if let Ok(request) = Request::decode(frame) {
        let outcome = ep.receiver.handle(request, &mut ep.store);
        for f in outcome.frames {
            if ep.outbox.len() == OUTBOX_MAX {
                ep.outbox.pop_front();
            }
            ep.outbox.push_back(f);
        }
        if let Some(change) = outcome.change {
            apply(change);
        }
    }
    ep.ensure_timer();
    ep.flush();
}

fn apply(change: Change) {
    super::trace(&format!("connect: {change:?}"));
    match change {
        Change::Installed(id) => {
            // A new pack is shown at once when nothing is selected or it replaced the shown one.
            let active = super::pick_pack().map(|f| file_id(&f));
            if active == Some(id) || !Fs::stat(CURRENT_FILE).is_some() {
                super::request_reload();
            }
        }
        Change::Selected(_) | Change::Deleted(_) => super::request_reload(),
        Change::Options(_) => super::options_changed(),
    }
}

unsafe extern "C" fn flush_timer(_ctx: *const ContextV1, _timer: i32, _user: *mut c_void) {
    let Some(ep) = endpoint() else { return };
    ep.flush();
    if ep.receiver.is_receiving() && now_ms(ep.ctx).wrapping_sub(ep.last_activity_ms) > STALL_MS {
        ep.receiver.reset(&mut ep.store);
        super::trace("connect: stalled transfer dropped");
    }
}

impl Endpoint {
    fn ensure_timer(&mut self) {
        if self.timer >= 0 {
            return;
        }
        let time = unsafe { (*self.ctx).time };
        if time.is_null() {
            return;
        }
        let ctx = self.ctx as *const ContextV1;
        unsafe {
            let id = ((*time).timer_create)(ctx, FLUSH_MS, 1, flush_timer, core::ptr::null_mut());
            if id < 0 {
                return;
            }
            let _ = ((*time).timer_set_background)(ctx, id, 1);
            if ((*time).timer_start)(ctx, id) < 0 {
                let _ = ((*time).timer_destroy)(ctx, id);
                return;
            }
            self.timer = id;
        }
    }

    fn flush(&mut self) {
        while let Some(frame) = self.outbox.front() {
            match connect::send_binary(self.ctx, frame) {
                Ok(Delivery::Queued) => {
                    self.outbox.pop_front();
                }
                // Nobody listens any more: the replies would only pile up.
                Ok(Delivery::NotListening) => {
                    self.outbox.clear();
                    self.receiver.reset(&mut self.store);
                }
                Err(ERR_BUSY) => return,
                Err(_) => {
                    self.outbox.pop_front();
                }
            }
        }
    }
}

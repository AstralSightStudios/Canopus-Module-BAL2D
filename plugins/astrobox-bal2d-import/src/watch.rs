//! Talking to the BAL2D module: `HELLO`, `LIST`, `SELECT`, `DELETE`, `OPTIONS`, and installing a
//! pack with the protocol crate's [`Sender`]. Replies arrive through `on_frame` (the plugin's
//! CanopusConnect export) and update the state; nothing waits for an answer.

use std::{
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU16, Ordering},
    },
    time::{Duration, Instant},
};

use bal2d_proto::sender::{Failure, Sender, State};
use bal2d_proto::{Event, Incoming, Reply, Request, Status, VERSION, op};
use futures::stream::{FuturesUnordered, StreamExt};

use crate::astrobox::psys_host_v4::timer;
use crate::{connect, state};

/// Requests other than the transfer's use numbers from here (the sender counts from 1).
static NEXT_REQ: AtomicU16 = AtomicU16::new(0x8000);

fn next_req() -> u16 {
    NEXT_REQ.fetch_add(1, Ordering::Relaxed) | 0x8000
}

pub const TRANSFER_TIMER: &str = "bal2d.transfer";
pub const STATS_TIMER: &str = "bal2d.stats";
/// The watch confirms at least every 4 KiB; a quiet watch gets the unconfirmed tail again.
const QUIET: Duration = Duration::from_secs(3);
/// Give up when nothing comes back for this long.
const STALL: Duration = Duration::from_secs(45);
/// Frames kept on their way at once; the host lets as many through as the watch's ring allows.
const IN_FLIGHT: usize = 15;

async fn send(addr: &str, request: Request) -> Result<(), String> {
    connect::ensure_listening(addr).await?;
    connect::send(addr, &request.encode()).await
}

fn selected() -> Option<String> {
    let addr = state::snapshot().selected_addr;
    (!addr.is_empty()).then_some(addr)
}

/// Greets the module on the selected watch; its answer fills in the screen and the pack list.
pub async fn hello() {
    let Some(addr) = selected() else { return };
    state::set_status("正在连接手表上的 BAL2D 模块…", false);
    if let Err(e) = send(&addr, Request::Hello { req: next_req(), version: VERSION }).await {
        state::with_state(|s| s.hello = None);
        state::set_status(format!("无法联系手表上的 BAL2D 模块（请确认模块已安装并启用、手表已连接）：{e}"), true);
    }
}

pub async fn list() {
    let Some(addr) = selected() else { return };
    let _ = send(&addr, Request::List { req: next_req(), offset: 0, max: 32 }).await;
}

/// Asks for the watch's measurements; while the watch tab is open, again every 3 s.
pub async fn stats() {
    let Some(addr) = selected() else { return };
    let _ = send(&addr, Request::Stats { req: next_req() }).await;
    if state::snapshot().tab == state::Tab::Watch {
        timer::set_timeout(3000, STATS_TIMER);
    }
}

pub async fn select_pack(id: u32) {
    let Some(addr) = selected() else { return };
    if let Err(e) = send(&addr, Request::Select { req: next_req(), id }).await {
        state::set_status(format!("切换失败：{e}"), true);
    }
}

pub async fn delete_pack(id: u32) {
    let Some(addr) = selected() else { return };
    if let Err(e) = send(&addr, Request::Delete { req: next_req(), id }).await {
        state::set_status(format!("删除失败：{e}"), true);
    }
}

pub async fn set_options(options: u8) {
    let Some(addr) = selected() else { return };
    if let Err(e) = send(&addr, Request::Options { req: next_req(), options }).await {
        state::set_status(format!("设置失败：{e}"), true);
    }
}

/// A frame from the module on `addr`.
pub async fn on_frame(addr: &str, frame: &[u8]) {
    if transfer_frame(addr, frame).await {
        return;
    }
    let Ok(Incoming::Reply(reply)) = Incoming::decode(frame) else { return };
    match reply {
        Reply::Hello { status: Status::Ok, hello, .. } => {
            state::with_state(|s| s.hello = Some(hello));
            state::set_status("", false);
            list().await;
        }
        Reply::List { status: Status::Ok, total, offset, packs, .. } => {
            let got = offset as usize + packs.len();
            state::with_state(|s| {
                if offset == 0 {
                    s.packs.clear();
                }
                s.packs.extend(packs.iter().cloned());
            });
            if got < total as usize && !packs.is_empty() {
                let _ = send(addr, Request::List { req: next_req(), offset: got as u8, max: 32 }).await;
            }
        }
        Reply::Stats { status: Status::Ok, stats, .. } => {
            state::with_state(|s| s.stats = Some(stats));
        }
        Reply::Ack { op: op::HELLO, status, value, .. } => {
            state::set_status(
                format!("手表上的 BAL2D 模块不兼容（{}，模块协议版本 {value}），请更新插件或模块。", status.describe()),
                true,
            );
        }
        Reply::Ack { op: code, status, .. } => {
            let what = match code {
                op::SELECT => "切换角色",
                op::DELETE => "删除角色",
                op::OPTIONS => "保存设置",
                _ => return,
            };
            if status == Status::Ok {
                state::set_status(format!("已{what}。"), false);
            } else {
                state::set_status(format!("{what}失败：{}", status.describe()), true);
            }
            hello().await;
        }
        _ => {}
    }
}

// -- transfer ----------------------------------------------------------------------------------

struct Active {
    addr: String,
    sender: Sender,
    started: Instant,
    last_heard: Instant,
    pumping: bool,
    speed_mark: (Instant, u32),
}

static ACTIVE: OnceLock<Mutex<Option<Active>>> = OnceLock::new();

fn lock() -> std::sync::MutexGuard<'static, Option<Active>> {
    ACTIVE.get_or_init(|| Mutex::new(None)).lock().unwrap_or_else(|e| e.into_inner())
}

/// Installs the built pack on the selected watch and shows it.
pub async fn install() -> Result<(), String> {
    let snap = state::snapshot();
    let Some(built) = snap.built.clone() else { return Err("还没有编译好的角色包。".into()) };
    let Some(addr) = selected() else { return Err("没有连接手表。".into()) };
    if lock().is_some() {
        return Err("已有传输正在进行。".into());
    }
    let sender = Sender::new(built.id, &built.name, Box::new(built.bytes.clone()), true)?;
    *lock() = Some(Active {
        addr: addr.clone(),
        sender,
        started: Instant::now(),
        last_heard: Instant::now(),
        pumping: false,
        speed_mark: (Instant::now(), 0),
    });
    state::with_state(|s| s.transfer = Some(state::Transfer { sent: 0, total: built.bytes.len() as u64, speed: 0 }));
    state::set_status("正在传输到手表…", false);
    if let Err(e) = connect::ensure_listening(&addr).await {
        *lock() = None;
        finish(Err(format!("无法联系手表：{e}"))).await;
        return Ok(());
    }
    timer::set_timeout(QUIET.as_millis() as u64, TRANSFER_TIMER);
    pump().await;
    Ok(())
}

pub async fn cancel() {
    let addr = lock().take().map(|a| a.addr);
    if let Some(addr) = addr {
        let _ = connect::send(&addr, &Request::Abort { req: next_req() }.encode()).await;
    }
    state::with_state(|s| s.transfer = None);
    state::set_status("已取消传输。", false);
}

/// Feeds a frame to the running transfer; `true` when it was one of its frames.
async fn transfer_frame(addr: &str, frame: &[u8]) -> bool {
    {
        let mut guard = lock();
        let Some(active) = guard.as_mut() else { return false };
        if active.addr != addr {
            return false;
        }
        let ours = match Incoming::decode(frame) {
            Ok(Incoming::Event(Event::Progress { .. })) => true,
            Ok(Incoming::Reply(Reply::Ack { req, op, .. })) => req < 0x8000 && matches!(op, op::BEGIN | op::END | op::SELECT),
            _ => false,
        };
        if !ours {
            return false;
        }
        active.last_heard = Instant::now();
        active.sender.on_frame(frame);
    }
    pump().await;
    true
}

/// The transfer timer: resend a quiet tail, give up on a silent watch.
pub async fn on_timer() {
    let stalled = {
        let mut guard = lock();
        let Some(active) = guard.as_mut() else { return };
        let quiet = active.last_heard.elapsed();
        if quiet >= STALL {
            true
        } else {
            if quiet >= QUIET {
                active.sender.on_timeout();
            }
            false
        }
    };
    if stalled {
        *lock() = None;
        finish(Err("手表长时间没有响应，传输已中止。".into())).await;
        return;
    }
    timer::set_timeout(QUIET.as_millis() as u64, TRANSFER_TIMER);
    pump().await;
}

fn failure_text(f: Failure) -> String {
    match f {
        Failure::Refused(s) => format!("手表拒绝了角色包：{}", s.describe()),
        Failure::Source => "读取角色包失败。".into(),
        Failure::Protocol => "手表的回复与传输流程不符，请更新插件与模块。".into(),
    }
}

async fn finish(result: Result<String, String>) {
    state::with_state(|s| s.transfer = None);
    match result {
        Ok(text) => state::set_status(text, false),
        Err(text) => state::set_status(text, true),
    }
    crate::ui::rerender();
}

/// Sends what the sender has ready, [`IN_FLIGHT`] at a time. Only one call pumps at a time;
/// frames that arrive meanwhile are fed in by `transfer_frame` and picked up by the running loop.
async fn pump() {
    {
        let mut guard = lock();
        let Some(active) = guard.as_mut() else { return };
        if active.pumping {
            return;
        }
        active.pumping = true;
    }
    let mut sends = FuturesUnordered::new();
    loop {
        while sends.len() < IN_FLIGHT {
            let next = {
                let mut guard = lock();
                let Some(active) = guard.as_mut() else { return };
                let (done, total) = active.sender.progress();
                let elapsed = active.speed_mark.0.elapsed();
                let speed = (elapsed >= Duration::from_secs(2)).then(|| {
                    let v = (done.saturating_sub(active.speed_mark.1)) as f64 / elapsed.as_secs_f64();
                    active.speed_mark = (Instant::now(), done);
                    v as u64
                });
                state::with_state(|s| {
                    if let Some(t) = &mut s.transfer {
                        t.sent = done as u64;
                        t.total = total as u64;
                        if let Some(v) = speed {
                            t.speed = v;
                        }
                    }
                });
                match active.sender.state() {
                    State::Done => {
                        let secs = active.started.elapsed().as_secs_f64().max(0.001);
                        let text = format!(
                            "已安装到手表{}（{:.0} KB，{:.1} KB/s）。",
                            if active.sender.selected() { "并切换为当前角色" } else { "" },
                            total as f64 / 1024.0,
                            total as f64 / 1024.0 / secs
                        );
                        *guard = None;
                        drop(guard);
                        finish(Ok(text)).await;
                        hello().await;
                        return;
                    }
                    State::Failed(f) => {
                        *guard = None;
                        drop(guard);
                        finish(Err(failure_text(f))).await;
                        return;
                    }
                    _ => {}
                }
                active.sender.poll().map(|frame| (active.addr.clone(), frame))
            };
            match next {
                Some((addr, frame)) => sends.push(async move { connect::send(&addr, &frame).await }),
                None => break,
            }
        }
        if sends.is_empty() {
            if let Some(active) = lock().as_mut() {
                active.pumping = false;
            }
            crate::ui::rerender();
            return;
        }
        if let Some(Err(e)) = sends.next().await {
            *lock() = None;
            finish(Err(format!("发送失败：{e}"))).await;
            return;
        }
    }
}

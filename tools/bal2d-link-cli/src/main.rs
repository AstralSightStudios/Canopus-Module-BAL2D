//! Drives `smoke --stdin` (the Mirage AstroBox bridge) and speaks the BAL2D Connect protocol,
//! with exactly the sender the AstroBox plugin uses.
//!
//!   bal2d-link-cli --smoke BIN --port P --key K --state DIR --band ADDR COMMAND ...
//!
//! Commands: `hello`, `list`, `install PACK [--id N] [--name S] [--select] [--pipeline N]`,
//! `select ID`, `delete ID`, `options N`, `stats`, and
//! `bench [--wait S] [--samples N] [--every S] [--keep] PACK...`: installs and shows each pack in
//! turn, waits for the intro, samples `STATS` and prints a table (the watch must stay awake:
//! `keepawake.flag` in the module directory). `--keep` leaves the packs on the watch.

use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use bal2d_proto::sender::{Sender, Source, State};
use bal2d_proto::{Incoming, MODULE_ID, Reply, Request, VERSION, crc32};

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    results: Receiver<bool>,
    frames: Receiver<Vec<u8>>,
    in_flight: usize,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2).filter_map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()).collect()
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("\"{key}\":");
    let start = line.find(&marker)? + marker.len();
    let rest = &line[start..];
    if let Some(quoted) = rest.strip_prefix('"') {
        Some(&quoted[..quoted.find('"')?])
    } else {
        Some(&rest[..rest.find([',', '}']).unwrap_or(rest.len())])
    }
}

fn verbose() -> bool {
    std::env::var_os("BAL2D_VERBOSE").is_some()
}

impl Bridge {
    fn start(args: &Args) -> Self {
        let mut child = Command::new(&args.smoke)
            .args([&args.port, &args.key, &args.state, "--stdin"])
            .env("MAB_BAND", &args.band)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the bridge");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (result_tx, results) = mpsc::channel();
        let (frame_tx, frames) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut ready = false;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.contains("\"type\":\"ready\"") {
                    ready = true;
                    let _ = ready_tx.send(());
                } else if line.contains("\"type\":\"connect\"") {
                    if let Some(data) = field(&line, "dataHex") {
                        let _ = frame_tx.send(unhex(data));
                    }
                } else if ready && line.contains("\"type\":\"result\"") {
                    let ok = line.contains("\"ok\":true");
                    let _ = result_tx.send(ok);
                    if !ok {
                        eprintln!("bridge: {line}");
                    }
                } else if verbose() {
                    eprintln!("bridge: {line}");
                }
            }
        });
        if ready_rx.recv_timeout(Duration::from_secs(200)).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bridge did not finish connecting");
        }
        Self { child, stdin, results, frames, in_flight: 0 }
    }

    fn call(&mut self, method: &str, params: &str) -> bool {
        writeln!(self.stdin, "{method}={params}").unwrap();
        self.stdin.flush().unwrap();
        self.results.recv_timeout(Duration::from_secs(120)).unwrap_or(false)
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        self.call("connectSend", &format!("{{\"module\":\"{MODULE_ID}\",\"dataHex\":\"{}\"}}", hex(frame)))
    }

    /// Keeps at most `depth` sends in flight (smoke's `&` prefix starts a request without waiting).
    fn send_pipelined(&mut self, frame: &[u8], depth: usize) -> bool {
        while self.in_flight >= depth {
            if !self.results.recv_timeout(Duration::from_secs(120)).unwrap_or(false) {
                return false;
            }
            self.in_flight -= 1;
        }
        writeln!(self.stdin, "&connectSend={{\"module\":\"{MODULE_ID}\",\"dataHex\":\"{}\"}}", hex(frame)).unwrap();
        self.stdin.flush().unwrap();
        self.in_flight += 1;
        true
    }

    fn drain(&mut self) -> bool {
        while self.in_flight > 0 {
            if !self.results.recv_timeout(Duration::from_secs(120)).unwrap_or(false) {
                return false;
            }
            self.in_flight -= 1;
        }
        true
    }

    fn finish(mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

struct Args {
    smoke: String,
    port: String,
    key: String,
    state: String,
    band: String,
    command: Vec<String>,
}

fn parse_args() -> Args {
    let mut values = std::collections::HashMap::new();
    let mut command = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--smoke" | "--port" | "--key" | "--state" | "--band" => {
                values.insert(arg, it.next().expect("value"));
            }
            _ => {
                command.push(arg);
                command.extend(it.by_ref());
            }
        }
    }
    let get = |name: &str| values.get(name).cloned().unwrap_or_else(|| panic!("{name} is required"));
    Args { smoke: get("--smoke"), port: get("--port"), key: get("--key"), state: get("--state"), band: get("--band"), command }
}

struct FileSource(File, u32);

impl Source for FileSource {
    fn len(&self) -> u32 {
        self.1
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<usize, String> {
        self.0.seek(SeekFrom::Start(u64::from(offset))).map_err(|e| e.to_string())?;
        self.0.read(buf).map_err(|e| e.to_string())
    }
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

fn number(text: &str) -> u32 {
    text.strip_prefix("0x").map_or_else(|| text.parse().unwrap(), |h| u32::from_str_radix(h, 16).unwrap())
}

fn request_reply(bridge: &mut Bridge, request: Request) -> Option<Reply> {
    let (op, req) = request.op_req();
    assert!(bridge.send(&request.encode()), "send failed");
    let deadline = Instant::now() + Duration::from_secs(20);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let frame = bridge.frames.recv_timeout(left).ok()?;
        if let Ok(Incoming::Reply(reply)) = Incoming::decode(&frame) {
            let code = match &reply {
                Reply::Hello { .. } => bal2d_proto::op::HELLO,
                Reply::List { .. } => bal2d_proto::op::LIST,
                Reply::Stats { .. } => bal2d_proto::op::STATS,
                Reply::Log { .. } => bal2d_proto::op::LOG,
                Reply::Ack { op, .. } => *op,
            };
            if code == op && reply.req() == req {
                return Some(reply);
            }
        }
    }
    None
}

fn stats(bridge: &mut Bridge) -> Option<bal2d_proto::Stats> {
    match request_reply(bridge, Request::Stats { req: 6 }) {
        Some(Reply::Stats { stats, .. }) => Some(stats),
        other => {
            eprintln!("stats: {other:?}");
            None
        }
    }
}

fn bench(bridge: &mut Bridge, args: &[String]) -> bool {
    let wait: f32 = option(args, "--wait").map(|v| v.parse().unwrap()).unwrap_or(15.0);
    let samples: usize = option(args, "--samples").map(|v| v.parse().unwrap()).unwrap_or(5);
    let every: f32 = option(args, "--every").map(|v| v.parse().unwrap()).unwrap_or(2.0);
    let keep = args.iter().any(|a| a == "--keep");
    let mut packs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if ["--wait", "--samples", "--every"].contains(&args[i].as_str()) {
            i += 2;
            continue;
        }
        if !args[i].starts_with("--") {
            packs.push(args[i].clone());
        }
        i += 1;
    }
    let mut rows = Vec::new();
    for path in &packs {
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().into_owned();
        if !install(bridge, &[path.clone(), "--name".into(), name.clone(), "--select".into(), "--pipeline".into(), "3".into()]) {
            rows.push(format!("{name:24} install failed"));
            continue;
        }
        std::thread::sleep(Duration::from_secs_f32(wait));
        let mut got = Vec::new();
        for _ in 0..samples {
            if let Some(s) = stats(bridge) {
                println!("  {name}: {s:?}");
                got.push(s);
            }
            std::thread::sleep(Duration::from_secs_f32(every));
        }
        if got.is_empty() {
            rows.push(format!("{name:24} no stats"));
        } else {
            let n = got.len() as f64;
            let avg = |f: fn(&bal2d_proto::Stats) -> u32| got.iter().map(|s| f(s) as f64).sum::<f64>() / n;
            let min_fps = got.iter().map(|s| s.fps_x10).min().unwrap_or(0) as f64 / 10.0;
            rows.push(format!(
                "{name:24} {:5.1} fps (min {:4.1})  update {:5.1}  draw {:6.1} (driver {:5.1})  gpu {:5.1} ms  {:5.0} tris {:4.0} calls  errors {}  heap free {:5.0} KB  bake {}",
                avg(|s| s.fps_x10) / 10.0,
                min_fps,
                avg(|s| s.update_ms),
                avg(|s| s.draw_ms),
                avg(|s| s.driver_ms),
                avg(|s| s.gpu_ms),
                avg(|s| s.triangles),
                avg(|s| s.calls),
                got.last().map(|s| s.errors).unwrap_or(0),
                avg(|s| s.heap_free_kib),
                if got.iter().all(|s| s.flags & 1 != 0) { "on" } else if got.iter().any(|s| s.flags & 1 != 0) { "partly" } else { "off" }
            ));
        }
        if !keep {
            let id = crc32(name.as_bytes()) | 1;
            let _ = request_reply(bridge, Request::Delete { req: 7, id });
        }
    }
    println!("\n{:24} results (averages over {samples} samples after {wait} s)", "pack");
    for r in &rows {
        println!("{r}");
    }
    true
}

fn install(bridge: &mut Bridge, args: &[String]) -> bool {
    let path = &args[0];
    let file = File::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let len = file.metadata().unwrap().len() as u32;
    let bytes = std::fs::read(path).unwrap();
    let name = option(args, "--name").unwrap_or_else(|| {
        std::path::Path::new(path).file_stem().unwrap().to_string_lossy().into_owned()
    });
    let id = option(args, "--id").map(|v| number(&v)).unwrap_or_else(|| crc32(name.as_bytes()) | 1);
    let select = args.iter().any(|a| a == "--select");
    let depth: usize = option(args, "--pipeline").map(|v| v.parse().unwrap()).unwrap_or(1);
    let mut sender = Sender::new(id, &name, Box::new(FileSource(file, len)), select).unwrap();
    assert_eq!(sender.crc32(), crc32(&bytes));
    let start = Instant::now();
    let mut last_heard = Instant::now();
    let mut last_report = Instant::now();
    while !sender.is_finished() {
        while let Ok(frame) = bridge.frames.try_recv() {
            sender.on_frame(&frame);
            last_heard = Instant::now();
        }
        match sender.poll() {
            Some(frame) => {
                if !bridge.send_pipelined(&frame, depth) {
                    eprintln!("send failed");
                    return false;
                }
            }
            None => match bridge.frames.recv_timeout(Duration::from_secs(3)) {
                Ok(frame) => {
                    sender.on_frame(&frame);
                    last_heard = Instant::now();
                }
                Err(_) => {
                    if last_heard.elapsed() > Duration::from_secs(45) {
                        eprintln!("stalled at {:?} {:?}", sender.state(), sender.progress());
                        return false;
                    }
                    sender.on_timeout();
                }
            },
        }
        if last_report.elapsed() > Duration::from_secs(5) {
            let (done, total) = sender.progress();
            eprintln!("  {done}/{total} bytes, {:.1} KiB/s", done as f64 / 1024.0 / start.elapsed().as_secs_f64());
            last_report = Instant::now();
        }
    }
    let drained = bridge.drain();
    let secs = start.elapsed().as_secs_f64();
    println!(
        "install {name} id {id:#010x}: {:?} in {secs:.1} s ({:.1} KiB/s), {} resumes, selected {}",
        sender.state(),
        len as f64 / 1024.0 / secs,
        sender.resumes(),
        sender.selected()
    );
    drained && sender.state() == State::Done
}

fn main() {
    let args = parse_args();
    let mut bridge = Bridge::start(&args);
    assert!(bridge.call("connectListen", &format!("{{\"module\":\"{MODULE_ID}\"}}")), "connectListen failed");
    let command = args.command.clone();
    let ok = match command.first().map(String::as_str) {
        Some("hello") => match request_reply(&mut bridge, Request::Hello { req: 1, version: VERSION }) {
            Some(reply) => {
                println!("{reply:?}");
                true
            }
            None => false,
        },
        Some("list") => {
            let mut offset = 0u8;
            loop {
                match request_reply(&mut bridge, Request::List { req: 2, offset, max: 16 }) {
                    Some(Reply::List { total, packs, .. }) => {
                        for p in &packs {
                            println!("{:#010x} {:>8} bytes  {}", p.id, p.size, p.name);
                        }
                        offset += packs.len() as u8;
                        if packs.is_empty() || offset >= total {
                            break true;
                        }
                    }
                    other => {
                        println!("{other:?}");
                        break false;
                    }
                }
            }
        }
        Some("install") => install(&mut bridge, &command[1..]),
        Some(cmd @ ("select" | "delete")) => {
            let id = number(&command[1]);
            let request = if cmd == "select" { Request::Select { req: 3, id } } else { Request::Delete { req: 4, id } };
            let reply = request_reply(&mut bridge, request);
            println!("{reply:?}");
            matches!(reply, Some(Reply::Ack { status: bal2d_proto::Status::Ok, .. }))
        }
        Some("stats") => match stats(&mut bridge) {
            Some(s) => {
                println!("{s:?}");
                true
            }
            None => false,
        },
        Some("bench") => bench(&mut bridge, &command[1..]),
        Some("log") => {
            // The module's log.txt, part by part, to stdout.
            let mut log = Vec::new();
            let mut req = 20u16;
            let ok = loop {
                req = req.wrapping_add(1);
                match request_reply(&mut bridge, Request::Log { req, offset: log.len() as u32 }) {
                    Some(Reply::Log { total, offset, bytes, .. }) if offset as usize == log.len() => {
                        let done = bytes.is_empty() || log.len() + bytes.len() >= total as usize;
                        log.extend_from_slice(&bytes);
                        if done {
                            break true;
                        }
                    }
                    other => {
                        eprintln!("log: {other:?}");
                        break false;
                    }
                }
            };
            print!("{}", String::from_utf8_lossy(&log));
            eprintln!("({} bytes)", log.len());
            ok
        }
        Some("options") => {
            let reply = request_reply(&mut bridge, Request::Options { req: 5, options: number(&command[1]) as u8 });
            println!("{reply:?}");
            reply.is_some()
        }
        _ => {
            eprintln!("commands: hello | list | install PACK [--id N] [--name S] [--select] [--pipeline N] | select ID | delete ID | options N");
            false
        }
    };
    bridge.finish();
    std::process::exit(if ok { 0 } else { 1 });
}

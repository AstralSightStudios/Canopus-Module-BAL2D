//! bal2d-tool: host-side utilities.
//!
//!   bal2d-tool render --skel A.skel [--skel B.skel ...] --scenario s.json --size 336x480
//!                     [--camera auto|cx,cy,scale] [--zoom 1.0] --out DIR [--every N] [--video]
//!
//! Each `--skel` loads `<name>.atlas` and its page PNGs from the same directory; skeletons are
//! drawn in the order given (put background skeletons first). Writes frame PNGs, a contact
//! sheet (`sheet.png`) and, with `--video`, `preview.mp4` (needs ffmpeg).

use std::path::{Path, PathBuf};
use std::process::Command;

use bal2d_render::DrawList;
use bal2d_spine::state::{AnimationState, AnimationStateData};
use bal2d_spine::*;
use bal2d_preview::raster::{self, Camera, Framebuffer, Texture};
use serde_json::Value;

struct Loaded {
    data: SkeletonData,
    pages: Vec<Texture>,
    sk: Skeleton,
    st: AnimationState,
    poses: Vec<(usize, f32, f32, usize)>,
}

fn load(skel: &Path) -> Loaded {
    let mut data = read_skeleton(&std::fs::read(skel).expect("read skel"), 1.0).expect("parse skel");
    let atlas_path = skel.with_extension("atlas");
    let atlas = Atlas::parse(&std::fs::read_to_string(&atlas_path).expect("read atlas"));
    let missing = bind_atlas(&mut data, &atlas);
    if !missing.is_empty() {
        eprintln!("{}: {} missing regions", skel.display(), missing.len());
    }
    let dir = skel.parent().unwrap();
    let pages = atlas
        .pages
        .iter()
        .map(|p| {
            let file = std::fs::File::open(dir.join(&p.name)).expect("open page");
            let mut dec = png::Decoder::new(file);
            dec.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
            let mut reader = dec.read_info().expect("png info");
            let mut buf = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut buf).expect("png frame");
            let rgba: Vec<u8> = match info.color_type {
                png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
                png::ColorType::Rgb => buf[..info.buffer_size()].chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
                png::ColorType::GrayscaleAlpha => buf[..info.buffer_size()].chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
                png::ColorType::Grayscale => buf[..info.buffer_size()].iter().flat_map(|&g| [g, g, g, 255]).collect(),
                other => panic!("unsupported png color type {other:?}"),
            };
            Texture::from_straight_rgba8(info.width as usize, info.height as usize, &rgba)
        })
        .collect();
    let mut sk = Skeleton::new(&data);
    sk.update_world_transform(&data, Physics::Update);
    let st = AnimationState::new(&data, AnimationStateData { default_mix: 0.25, mixes: vec![] });
    Loaded { data, pages, sk, st, poses: vec![] }
}

/// Loads every layer of a compiled pack (the watch's loading path, minus the GPU upload).
fn load_pack(path: &Path) -> (Vec<Loaded>, bal2d_pack::Header) {
    let bytes = std::fs::read(path).expect("read pack");
    let r = bal2d_pack::Reader::new(&bytes).expect("parse pack");
    let mut layers = Vec::new();
    for li in 0..r.header.layer_count as u16 {
        let skel = r.read(r.find(bal2d_pack::KIND_SKEL, li, 0).expect("skel")).unwrap();
        let atlas_text = String::from_utf8(r.read(r.find(bal2d_pack::KIND_ATLAS, li, 0).expect("atlas")).unwrap()).unwrap();
        let mut data = read_skeleton(&skel, 1.0).expect("pack skel");
        let atlas = Atlas::parse(&atlas_text);
        let missing = bind_atlas(&mut data, &atlas);
        assert!(missing.is_empty(), "pack atlas misses {missing:?}");
        let pages = (0..r.count(bal2d_pack::KIND_PAGE, li))
            .map(|pi| {
                let payload = r.read(r.find(bal2d_pack::KIND_PAGE, li, pi as u16).unwrap()).unwrap();
                let h = bal2d_pack::page_header(&payload).unwrap();
                let img = bal2d_compiler::image::decode_page(h.width as u32, h.height as u32, h.format, &payload[8..]);
                Texture::from_premultiplied_rgba8(img.width as usize, img.height as usize, &img.data)
            })
            .collect();
        let mut sk = Skeleton::new(&data);
        sk.update_world_transform(&data, Physics::Update);
        let st = AnimationState::new(&data, AnimationStateData { default_mix: 0.25, mixes: vec![] });
        layers.push(Loaded { data, pages, sk, st, poses: vec![] });
    }
    (layers, r.header)
}

fn apply_actions(l: &mut Loaded, actions: &[Value], frame: usize) {
    let d = &l.data;
    for act in actions.iter().filter(|x| x["frame"].as_u64() == Some(frame as u64)) {
        let track = act["track"].as_u64().unwrap_or(0) as usize;
        let anim = act["anim"].as_str().and_then(|n| d.find_animation(n));
        match act["op"].as_str().unwrap_or("") {
            "set" if anim.is_some() => {
                l.st.set_animation(d, track, anim, act["loop"].as_bool().unwrap_or(false));
            }
            "add" if anim.is_some() => {
                l.st.add_animation(d, track, anim, act["loop"].as_bool().unwrap_or(false), act["delay"].as_f64().unwrap_or(0.0) as f32);
            }
            "setEmpty" => {
                l.st.set_empty_animation(d, track, act["mix"].as_f64().unwrap_or(0.0) as f32);
            }
            "addEmpty" => {
                l.st.add_empty_animation(d, track, act["mix"].as_f64().unwrap_or(0.0) as f32, act["delay"].as_f64().unwrap_or(0.0) as f32);
            }
            "bone" => {
                if let Some(b) = d.find_bone(act["name"].as_str().unwrap_or("")) {
                    l.poses.push((b, act["dx"].as_f64().unwrap() as f32, act["dy"].as_f64().unwrap() as f32, act["until"].as_u64().unwrap() as usize));
                }
            }
            _ => {}
        }
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn args_all(args: &[String], name: &str) -> Vec<String> {
    args.iter().enumerate().filter(|(_, a)| *a == name).filter_map(|(i, _)| args.get(i + 1).cloned()).collect()
}

fn write_png(path: &Path, w: usize, h: usize, rgba: &[u8]) {
    let file = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}

fn render(args: &[String]) {
    let skels: Vec<PathBuf> = args_all(args, "--skel").into_iter().map(PathBuf::from).collect();
    let scenario: Value = serde_json::from_str(&std::fs::read_to_string(arg(args, "--scenario").expect("--scenario")).unwrap()).unwrap();
    let size = arg(args, "--size").unwrap_or_else(|| "336x480".into());
    let (w, h) = size.split_once('x').map(|(a, b)| (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap())).unwrap();
    let out = PathBuf::from(arg(args, "--out").expect("--out"));
    std::fs::create_dir_all(&out).unwrap();
    let every = arg(args, "--every").and_then(|s| s.parse().ok()).unwrap_or(1usize);
    let zoom: f32 = arg(args, "--zoom").and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let mut pack_header = None;
    let mut layers: Vec<Loaded> = match arg(args, "--pack") {
        Some(p) => {
            let (l, h) = load_pack(Path::new(&p));
            pack_header = Some(h);
            l
        }
        None => skels.iter().map(|p| load(p)).collect(),
    };
    let dt = scenario["dt"].as_f64().unwrap() as f32;
    let frames = scenario["frames"].as_u64().unwrap() as usize;
    let actions = scenario["actions"].as_array().cloned().unwrap_or_default();

    // Camera: explicit, or centered on the character's head (Touch_Point) after the setup pose.
    let cam = match arg(args, "--camera").as_deref() {
        _ if pack_header.is_some() && arg(args, "--camera").is_none() => {
            let h = pack_header.unwrap();
            Camera { cx: h.camera_cx, cy: h.camera_cy, scale: h.camera_scale }
        }
        Some(c) if c != "auto" => {
            let v: Vec<f32> = c.split(',').map(|x| x.parse().unwrap()).collect();
            Camera { cx: v[0], cy: v[1], scale: v[2] }
        }
        _ => {
            let main = layers.iter().find(|l| l.data.find_bone("Touch_Point").is_some()).unwrap_or(&layers[0]);
            let (hx, hy) = match main.data.find_bone("Touch_Point") {
                Some(b) => (main.sk.bones[b].world_x, main.sk.bones[b].world_y),
                None => (main.data.x + main.data.width / 2.0, main.data.y + main.data.height / 2.0),
            };
            // Lobby skeletons are laid out for the game's 2960x1440 frame (y 0..1440): keep the
            // game's vertical composition, center horizontally on the head. Zoom keeps the head in view.
            let _ = hy;
            let scale = h as f32 / 1440.0 * zoom;
            let cy = 720.0 + (hy - 720.0) * (1.0 - 1.0 / zoom);
            Camera { cx: hx, cy, scale }
        }
    };
    println!("camera {:.1},{:.1},{:.4}", cam.cx, cam.cy, cam.scale);

    let mut list = DrawList::default();
    let mut saved = Vec::new();
    let t0 = std::time::Instant::now();
    let (mut tris, mut px) = (0usize, 0usize);
    for f in 0..frames {
        for l in layers.iter_mut() {
            apply_actions(l, &actions, f);
            // Skeletons without the scenario's animations (backgrounds) loop their first idle.
            if f == 0 && l.st.current(0).is_none() {
                let idle = l.data.animations.iter().position(|a| a.name.starts_with("Idle")).or_else(|| {
                    l.data.animations.iter().position(|a| a.name != "Dummy" && a.duration > 0.0)
                });
                if idle.is_some() {
                    l.st.set_animation(&l.data, 0, idle, true);
                }
            }
            l.st.update(&l.data, dt);
            l.st.apply(&l.data, &mut l.sk);
            for &(b, dx, dy, until) in &l.poses {
                if f < until {
                    l.sk.bones[b].x = l.data.bones[b].x + dx;
                    l.sk.bones[b].y = l.data.bones[b].y + dy;
                }
            }
            l.sk.update(dt);
            l.sk.update_world_transform(&l.data, Physics::Update);
            l.st.output.clear();
        }
        if f % every != 0 {
            continue;
        }
        let mut fb = Framebuffer::new(w, h, [0.0, 0.0, 0.0, 1.0]);
        for l in &layers {
            bal2d_render::build(&l.data, &l.sk, &mut list);
            let s = raster::draw_as_gpu(&mut fb, &list, &l.pages, cam, gpu_tolerance());
            tris += s.triangles;
            px += s.pixels;
        }
        let path = out.join(format!("frame_{f:05}.png"));
        write_png(&path, w, h, &fb.to_rgba8());
        saved.push(path);
    }
    let n = saved.len().max(1);
    println!(
        "rendered {} frames in {:.1}s; avg {:.0} triangles, {:.0} shaded pixels per frame (overdraw {:.2})",
        saved.len(),
        t0.elapsed().as_secs_f32(),
        tris as f32 / n as f32,
        px as f32 / n as f32,
        px as f32 / n as f32 / (w * h) as f32
    );
    contact_sheet(&saved, &out.join("sheet.png"), w, h);
    if args.iter().any(|a| a == "--video") {
        let fps = (1.0 / (dt * every as f32)).round() as u32;
        let status = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-framerate", &fps.to_string(), "-pattern_type", "glob", "-i"])
            .arg(out.join("frame_*.png"))
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20"])
            .arg(out.join("preview.mp4"))
            .status();
        println!("video: {:?}", status.map(|s| s.success()));
    }
}

/// Up to 12 evenly spaced frames, 4 per row, scaled by 1/2.
fn contact_sheet(frames: &[PathBuf], out: &Path, w: usize, h: usize) {
    if frames.is_empty() {
        return;
    }
    let pick: Vec<&PathBuf> = (0..12.min(frames.len())).map(|i| &frames[i * frames.len() / 12.min(frames.len())]).collect();
    let (tw, th) = (w / 2, h / 2);
    let cols = 4.min(pick.len());
    let rows = pick.len().div_ceil(cols);
    let mut sheet = vec![40u8; cols * tw * rows * th * 4];
    for (i, p) in pick.iter().enumerate() {
        let dec = png::Decoder::new(std::fs::File::open(p).unwrap());
        let mut r = dec.read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size()];
        r.next_frame(&mut buf).unwrap();
        let (ox, oy) = ((i % cols) * tw, (i / cols) * th);
        for y in 0..th {
            for x in 0..tw {
                let s = ((y * 2) * w + x * 2) * 4;
                let d = ((oy + y) * cols * tw + ox + x) * 4;
                sheet[d..d + 4].copy_from_slice(&buf[s..s + 4]);
            }
        }
    }
    write_png(out, cols * tw, rows * th, &sheet);
}

fn compile(args: &[String]) {
    let skels: Vec<PathBuf> = args_all(args, "--skel").into_iter().map(PathBuf::from).collect();
    let size = arg(args, "--size").unwrap_or_else(|| "336x480".into());
    let (w, h) = size.split_once('x').map(|(a, b)| (a.parse::<u32>().unwrap(), b.parse::<u32>().unwrap())).unwrap();
    let mut opts = bal2d_compiler::Options { width: w, height: h, ..Default::default() };
    if let Some(z) = arg(args, "--zoom") {
        opts.zoom = z.parse().unwrap();
    }
    if let Some(b) = arg(args, "--budget") {
        opts.budget = b.parse().unwrap();
    }
    if let Some(q) = arg(args, "--quality") {
        opts.quality = q.parse().unwrap();
    }
    if let Some(n) = arg(args, "--name") {
        opts.name = n;
    }
    opts.bake = !args.iter().any(|a| a == "--no-bake");
    if let Some(t) = arg(args, "--simplify") {
        opts.simplify = t.parse().unwrap();
    }
    opts.idle = !args.iter().any(|a| a == "--no-idle");
    if let Some(t) = arg(args, "--idle-tolerance") {
        opts.idle_tolerance = t.parse().unwrap();
    }
    opts.format = match arg(args, "--format").as_deref() {
        Some("argb4444") => bal2d_pack::PageFormat::Argb4444,
        Some("index8") => bal2d_pack::PageFormat::Index8,
        Some("argb8565") => bal2d_pack::PageFormat::Argb8565,
        _ => bal2d_pack::PageFormat::Bgra8888,
    };
    // `{"talks": [{"talk": "Talk_01", "lines": [{"text": "…", "ms": 5300, "thought": false}]}]}`
    if let Some(path) = arg(args, "--dialogue") {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let talks = v["talks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| bal2d_pack::TalkLines {
                talk: t["talk"].as_str().unwrap().to_string(),
                lines: t["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| bal2d_pack::Line { text: l["text"].as_str().unwrap().to_string(), ms: l["ms"].as_u64().unwrap() as u32, thought: l["thought"].as_bool().unwrap_or(false) })
                    .collect(),
            })
            .collect();
        opts.dialogue = Some(bal2d_pack::Dialogue { talks });
    }
    let inputs: Vec<bal2d_compiler::LayerInput> = skels
        .iter()
        .map(|p| {
            let atlas = std::fs::read_to_string(p.with_extension("atlas")).unwrap();
            let parsed = Atlas::parse(&atlas);
            let dir = p.parent().unwrap();
            bal2d_compiler::LayerInput {
                name: p.file_stem().unwrap().to_string_lossy().into_owned(),
                skel: std::fs::read(p).unwrap(),
                pages: parsed.pages.iter().map(|pg| (pg.name.clone(), std::fs::read(dir.join(&pg.name)).unwrap().into())).collect(),
                atlas,
            }
        })
        .collect();
    let original: usize = skels
        .iter()
        .map(|p| {
            let dir = p.parent().unwrap();
            let atlas = Atlas::parse(&std::fs::read_to_string(p.with_extension("atlas")).unwrap());
            std::fs::metadata(p).unwrap().len() as usize
                + atlas.pages.iter().map(|pg| std::fs::metadata(dir.join(&pg.name)).unwrap().len() as usize).sum::<usize>()
        })
        .sum();
    let t0 = std::time::Instant::now();
    let (bytes, report) = bal2d_compiler::compile(&inputs, &opts, None).unwrap_or_else(|e| {
        eprintln!("compile failed: {e}");
        std::process::exit(1)
    });
    let out = arg(args, "--out").expect("--out");
    std::fs::write(&out, &bytes).unwrap();
    println!("compiled in {:.1}s: {} -> {} bytes ({:.1}%), budget scale {:.3}, camera {:?}", t0.elapsed().as_secs_f32(), original, bytes.len(), bytes.len() as f32 * 100.0 / original as f32, report.budget_scale, report.camera.unwrap());
    if opts.dialogue.is_some() {
        println!("dialogue: {} talks with lines", report.dialogue_talks);
    }
    if !report.hidden_layers.is_empty() {
        println!("  left out {} (never shows: always behind another layer)", report.hidden_layers.join(", "));
    }
    if let Some((slots, tris, r, after_intro)) = report.baked {
        println!(
            "  baked {slots} static slots ({tris} triangles) into {}x{} at ({}, {}){}",
            r[2],
            r[3],
            r[0],
            r[1],
            if after_intro { ", used after the intro" } else { "" }
        );
    }
    if report.idle.layers.is_empty() {
        println!("  idle bake: none ({})", report.idle.skipped);
    }
    for (li, (frames, calls, shapes, bytes)) in report.idle.layers.iter().enumerate() {
        println!("  idle bake layer {li}: {frames} frames (up to {} fps), {calls:.0} calls/frame, {shapes} shapes, {} KB raw", report.idle.rate, bytes / 1024);
    }
    if report.idle.steady > 0 {
        println!("  talks leave {} slots on the bake", report.idle.steady);
    }
    let c = &report.idle.cull;
    if c.fill_before > 0.0 {
        println!("  idle fill: {:.2} screens/frame -> {:.2} without empty texels -> {:.2} clipped ({:.2} with every layer baked), {} attachments split", c.fill_before, c.fill_after_empty, c.fill_after, c.fill_after_above, c.split_attachments);
    }
    for l in &report.layers {
        println!(
            "  {}: attachments {} -> {}, regions {} -> {}, texels {:.2} -> {:.2} Mpx, pages {:?}, skel {} B; dropped {} animations, {} timelines; trimmed {} triangles, {} vertices; simplified away {} triangles, {} vertices",
            l.name,
            l.attachments,
            l.kept,
            l.regions_before,
            l.regions_after,
            l.texels_before as f64 / 1e6,
            l.texels_after as f64 / 1e6,
            l.pages,
            l.skel_bytes,
            l.animations_dropped,
            l.timelines_dropped,
            l.triangles_trimmed,
            l.vertices_trimmed,
            l.triangles_simplified,
            l.vertices_simplified
        );
    }
}

/// Side-by-side image of two frame directories plus PSNR of the second against the first.
fn compare(args: &[String]) {
    let a = PathBuf::from(&args[0]);
    let b = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);
    let read = |p: &Path| -> (usize, usize, Vec<u8>) {
        let dec = png::Decoder::new(std::fs::File::open(p).unwrap());
        let mut r = dec.read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size()];
        let info = r.next_frame(&mut buf).unwrap();
        (info.width as usize, info.height as usize, buf)
    };
    let mut names: Vec<String> = std::fs::read_dir(&a).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("frame_")).collect();
    names.sort();
    let (mut se, mut n) = (0f64, 0usize);
    for name in &names {
        let (w, h, x) = read(&a.join(name));
        let (_, _, y) = read(&b.join(name));
        for i in 0..w * h {
            for c in 0..3 {
                let d = x[i * 4 + c] as f64 - y[i * 4 + c] as f64;
                se += d * d;
                n += 1;
            }
        }
    }
    let mse = se / n.max(1) as f64;
    println!("PSNR over {} frames: {:.2} dB", names.len(), 10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10());
    let pick: Vec<&String> = (0..4).map(|i| &names[i * names.len() / 4 + names.len() / 8]).collect();
    let (w, h, _) = read(&a.join(pick[0]));
    let mut sheet = vec![0u8; w * 2 * h * pick.len() * 4];
    for (k, name) in pick.iter().enumerate() {
        for (side, dir) in [&a, &b].iter().enumerate() {
            let (_, _, px) = read(&dir.join(name));
            for y in 0..h {
                let s = y * w * 4;
                let d = ((k * h + y) * w * 2 + side * w) * 4;
                sheet[d..d + w * 4].copy_from_slice(&px[s..s + w * 4]);
            }
        }
    }
    write_png(&out, w * 2, h * pick.len(), &sheet);
}

/// Plays a pack through `bal2d-core::Lobby` with a scripted touch sequence and records it:
///   play --pack P --touches t.json --seconds S --out DIR [--fps 30] [--video]
/// touches: [{"t": 9.0, "type": "down"|"move"|"up", "x": 168, "y": 120}, ...] (screen pixels)
fn play(args: &[String]) {
    let pack = std::fs::read(arg(args, "--pack").expect("--pack")).unwrap();
    let (mut lobby, pages) = bal2d_core::Lobby::load(&pack).expect("load pack");
    let textures: Vec<(u16, Texture)> = pages
        .iter()
        .map(|p| {
            let mut section = vec![0u8; p.section_len()];
            bal2d_core::read_page(&mut &pack[..], p, &mut section).expect("page");
            let img = bal2d_compiler::image::decode_page(p.header.width as u32, p.header.height as u32, p.header.format, &section[8..]);
            (p.layer, Texture::from_premultiplied_rgba8(img.width as usize, img.height as usize, &img.data))
        })
        .collect();
    let mut per_layer: Vec<Vec<Texture>> = (0..lobby.layers.len()).map(|_| Vec::new()).collect();
    for (l, t) in textures {
        per_layer[l as usize].push(t);
    }
    let touches: Vec<Value> = arg(args, "--touches")
        .map(|p| serde_json::from_str::<Value>(&std::fs::read_to_string(p).unwrap()).unwrap().as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    let fps: f32 = arg(args, "--fps").and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let seconds: f32 = arg(args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let out = PathBuf::from(arg(args, "--out").expect("--out"));
    std::fs::create_dir_all(&out).unwrap();
    for e in std::fs::read_dir(&out).unwrap().flatten() {
        if e.file_name().to_string_lossy().starts_with("frame_") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let (w, h) = (lobby.width as usize, lobby.height as usize);
    let cam = Camera { cx: lobby.camera.cx, cy: lobby.camera.cy, scale: lobby.camera.scale };
    let bake = lobby.bake.as_ref().map(|b| bal2d_preview::decode_bake(&pack, b).expect("bake"));
    let dt = 1.0 / fps;
    let frames = (seconds * fps) as usize;
    let mut list = DrawList::default();
    let mut ti = 0;
    let mut finger: Option<(f32, f32)> = None;
    let (mut t_update, mut t_build) = (0f64, 0f64);
    let mut saved = Vec::new();
    for f in 0..frames {
        let now = f as f32 * dt;
        while ti < touches.len() && touches[ti]["t"].as_f64().unwrap() as f32 <= now {
            let e = &touches[ti];
            let (x, y) = (e["x"].as_f64().unwrap() as f32, e["y"].as_f64().unwrap() as f32);
            let kind = match e["type"].as_str().unwrap() {
                "down" => bal2d_core::Touch::Down,
                "move" => bal2d_core::Touch::Move,
                _ => bal2d_core::Touch::Up,
            };
            finger = if kind == bal2d_core::Touch::Up { None } else { Some((x, y)) };
            lobby.touch(kind, x, y);
            ti += 1;
        }
        let t0 = std::time::Instant::now();
        lobby.update(dt);
        t_update += t0.elapsed().as_secs_f64();
        for n in lobby.notices.drain(..) {
            println!("{:6.2}s {:?}", now, n);
        }
        let mut fb = Framebuffer::new(w, h, [0.0, 0.0, 0.0, 1.0]);
        if let Some(b) = bake.as_ref().filter(|_| lobby.bake_active()) {
            bal2d_preview::draw_bake(&mut fb, b);
        }
        for li in 0..lobby.layers.len() {
            let t1 = std::time::Instant::now();
            lobby.draw_list(li, &mut list);
            t_build += t1.elapsed().as_secs_f64();
            raster::draw_as_gpu(&mut fb, &list, &per_layer[li], cam, gpu_tolerance());
        }
        if let Some((x, y)) = finger {
            // Finger marker for the recording.
            for dy in -6i32..=6 {
                for dx in -6i32..=6 {
                    let (px, py) = (x as i32 + dx, y as i32 + dy);
                    if dx * dx + dy * dy <= 36 && dx * dx + dy * dy >= 16 && px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                        fb.pixels[py as usize * w + px as usize] = [1.0, 0.2, 0.4, 1.0];
                    }
                }
            }
        }
        let path = out.join(format!("frame_{f:05}.png"));
        write_png(&path, w, h, &fb.to_rgba8());
        saved.push(path);
    }
    println!(
        "{} frames; host cost per frame: update {:.2} ms, draw list {:.2} ms",
        frames,
        t_update * 1000.0 / frames as f64,
        t_build * 1000.0 / frames as f64
    );
    contact_sheet(&saved, &out.join("sheet.png"), w, h);
    if args.iter().any(|a| a == "--video") {
        let status = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-framerate", &fps.to_string(), "-pattern_type", "glob", "-i"])
            .arg(out.join("frame_*.png"))
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20"])
            .arg(out.join("preview.mp4"))
            .status();
        println!("video: {:?}", status.map(|s| s.success()));
    }
}

fn read_face(path: &str) -> (Vec<u8>, bool) {
    let bytes = std::fs::read(path).expect("read watchface");
    if bytes.starts_with(b"PK") {
        let entries = bal2d_face::zip::read(&bytes).expect("mwz");
        let bin = entries.into_iter().find(|e| e.0 == "resource.bin").expect("resource.bin").1;
        (bin, true)
    } else {
        (bytes, false)
    }
}

fn face_info(args: &[String]) {
    let (bin, mwz) = read_face(&args[0]);
    let face = bal2d_face::Face::parse(&bin).unwrap_or_else(|e| panic!("{e}"));
    println!("{} ({} bytes{}): protocol 0x{:x}, name {:?}, {} themes, has BAL2D layer: {}", args[0], bin.len(), if mwz { ", from MWZ" } else { "" }, face.protocol, face.name, face.themes.len(), face.has_marker(&bin));
    for (i, t) in face.themes.iter().enumerate() {
        let counts: Vec<usize> = t.tables.iter().map(|t| t.count).collect();
        println!("  theme {i} {:?} aod={} tables {:?}", t.name, t.aod, counts);
        for (z, l) in face.layouts(&bin, i).iter().enumerate() {
            println!("    z{z:<3} layout 0x{:08x} -> resource 0x{:08x} (type {}) at ({}, {})", l.uid, l.resource_uid, l.resource_uid >> 24, l.x, l.y);
        }
    }
}

fn face_patch(args: &[String]) {
    let input = &args[0];
    let z: usize = arg(args, "--z").map(|v| if v == "top" { usize::MAX } else { v.parse().unwrap() }).unwrap_or(0);
    let rect: Vec<i32> = arg(args, "--rect").unwrap_or_else(|| "0,0,336,480".into()).split(',').map(|v| v.parse().unwrap()).collect();
    let place = bal2d_face::Placement { z, x: rect[0] as i16, y: rect[1] as i16, width: rect[2] as u16, height: rect[3] as u16 };
    let out = arg(args, "--out").expect("--out");
    let bytes = std::fs::read(input).unwrap();
    if bytes.starts_with(b"PK") {
        let (mwz, bin) = bal2d_face::patch_mwz(&bytes, place).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&out, &mwz).unwrap();
        let bin_out = Path::new(&out).with_extension("bin");
        std::fs::write(&bin_out, &bin).unwrap();
        println!("wrote {out} ({} bytes) and {} ({} bytes)", mwz.len(), bin_out.display(), bin.len());
    } else {
        let bin = bal2d_face::patch_bin(&bytes, place).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&out, &bin).unwrap();
        println!("wrote {out} ({} bytes)", bin.len());
    }
}

/// Draw calls per frame after affine clustering: calls --pack P [--seconds S] [--tol T]...
fn calls(args: &[String]) {
    let pack = std::fs::read(arg(args, "--pack").expect("--pack")).unwrap();
    let (mut lobby, pages) = bal2d_core::Lobby::load(&pack).expect("load pack");
    let seconds: f32 = arg(args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let tols: Vec<f32> = args_all(args, "--tol").iter().map(|t| t.parse().unwrap()).collect();
    let tols = if tols.is_empty() { vec![0.5, 0.75, 1.0] } else { tols };
    let page_size: Vec<(u16, (f32, f32))> = pages.iter().map(|p| (p.layer, (p.header.width as f32, p.header.height as f32))).collect();
    let (w2, h2) = (lobby.width * 0.5, lobby.height * 0.5);
    let cam = lobby.camera;
    let dt = 1.0 / 30.0;
    let frames = (seconds / dt) as usize;
    let mut list = DrawList::default();
    let mut tris = Vec::new();
    let (mut order, mut clusters) = (Vec::new(), Vec::new());
    let mut totals = vec![(0usize, 0usize, 0f64); tols.len()];
    let mut raw = 0usize;
    // Path words per frame at the first tolerance: one subpath per triangle vs the outline.
    let (mut words_tri, mut words_outline, mut t_outline) = (0usize, 0usize, 0f64);
    // Sum over calls of the path bounding-box area, and of the triangles' own area.
    let (mut bbox_area, mut tri_area) = (0f64, 0f64);
    let mut scratch = bal2d_render::Outline::default();
    for _ in 0..frames {
        lobby.update(dt);
        for li in 0..lobby.layers.len() {
            lobby.draw_list(li, &mut list);
            let layer_pages: Vec<(f32, f32)> = page_size.iter().filter(|p| p.0 as usize == li).map(|p| p.1).collect();
            for b in &list.batches {
                let (pw, ph) = layer_pages[b.page as usize];
                tris.clear();
                let idx = &list.indices[b.first_index as usize..(b.first_index + b.index_count) as usize];
                for t in idx.chunks_exact(3) {
                    let mut tri = bal2d_render::Tri::default();
                    for k in 0..3 {
                        let vi = b.first_vertex as usize + t[k] as usize;
                        tri.s[k] = [(list.positions[vi * 2] - cam.cx) * cam.scale + w2, h2 - (list.positions[vi * 2 + 1] - cam.cy) * cam.scale];
                        tri.t[k] = [list.uvs[vi * 2] * pw, list.uvs[vi * 2 + 1] * ph];
                        tri.v[k] = vi as u32;
                    }
                    let (minx, maxx) = (tri.s.iter().map(|p| p[0]).fold(f32::MAX, f32::min), tri.s.iter().map(|p| p[0]).fold(f32::MIN, f32::max));
                    let (miny, maxy) = (tri.s.iter().map(|p| p[1]).fold(f32::MAX, f32::min), tri.s.iter().map(|p| p[1]).fold(f32::MIN, f32::max));
                    if maxx < 0.0 || maxy < 0.0 || minx > w2 * 2.0 || miny > h2 * 2.0 {
                        continue;
                    }
                    tris.push(tri);
                }
                raw += tris.len();
                for (k, &tol) in tols.iter().enumerate() {
                    let t0 = std::time::Instant::now();
                    bal2d_render::cluster(&tris, tol, &mut order, &mut clusters);
                    totals[k].2 += t0.elapsed().as_secs_f64();
                    totals[k].0 += clusters.len();
                    totals[k].1 = totals[k].1.max(clusters.iter().map(|c| c.count as usize).max().unwrap_or(0));
                    if k == 0 {
                        let t1 = std::time::Instant::now();
                        for c in &clusters {
                            let members = &order[c.start as usize..(c.start + c.count) as usize];
                            words_tri += members.len() * 10 + 1;
                            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                            for &m in members {
                                let t = &tris[m as usize];
                                for q in &t.s {
                                    x0 = x0.min(q[0].max(0.0));
                                    y0 = y0.min(q[1].max(0.0));
                                    x1 = x1.max(q[0].min(w2 * 2.0));
                                    y1 = y1.max(q[1].min(h2 * 2.0));
                                }
                                tri_area += (((t.s[1][0] - t.s[0][0]) * (t.s[2][1] - t.s[0][1]) - (t.s[2][0] - t.s[0][0]) * (t.s[1][1] - t.s[0][1])) * 0.5).abs() as f64;
                            }
                            bbox_area += ((x1 - x0).max(0.0) * (y1 - y0).max(0.0)) as f64;
                            bal2d_render::outline(&tris, members, &mut scratch, |op, _, _| {
                                words_outline += if op == bal2d_render::PATH_CLOSE { 1 } else { 3 };
                            });
                            words_outline += 1;
                        }
                        t_outline += t1.elapsed().as_secs_f64();
                    }
                }
            }
        }
    }
    println!("{} frames: {:.0} visible triangles per frame", frames, raw as f64 / frames as f64);
    for (k, &tol) in tols.iter().enumerate() {
        println!("  tol {tol:.2} px: {:.0} calls per frame (largest cluster {}), clustering {:.3} ms/frame on host", totals[k].0 as f64 / frames as f64, totals[k].1, totals[k].2 * 1000.0 / frames as f64);
    }
    println!(
        "  path data at tol {:.2}: {:.1} KB per frame as triangles, {:.1} KB as outlines ({:.0}%), outlining {:.3} ms/frame on host",
        tols[0],
        words_tri as f64 * 4.0 / 1024.0 / frames as f64,
        words_outline as f64 * 4.0 / 1024.0 / frames as f64,
        words_outline as f64 * 100.0 / words_tri.max(1) as f64,
        t_outline * 1000.0 / frames as f64
    );
    let screen = (w2 * 2.0 * h2 * 2.0) as f64;
    println!(
        "  per frame: path bounding boxes {:.1} screens, triangle area {:.1} screens",
        bbox_area / frames as f64 / screen,
        tri_area / frames as f64 / screen
    );
}

/// The watch's per-frame CPU work on the host, phase by phase, with the same code:
///   bench --pack P... [--seconds S] [--touch]
/// `--touch` pats the head and taps (talk) during the run, like a user would.
fn bench(args: &[String]) {
    let seconds: f32 = arg(args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let touch = args.iter().any(|a| a == "--touch");
    for path in args_all(args, "--pack") {
        let pack = std::fs::read(&path).unwrap();
        let (mut lobby, pages) = bal2d_core::Lobby::load(&pack).expect("load pack");
        let mut sizes: Vec<Vec<(f32, f32)>> = vec![Vec::new(); lobby.layers.len()];
        for p in &pages {
            sizes[p.layer as usize].push((p.header.width as f32, p.header.height as f32));
        }
        let mut list = DrawList::default();
        let mut paths = bal2d_render::Paths::new();
        if let Some(t) = std::env::var("BAL2D_TOL").ok().and_then(|v| v.parse().ok()) { paths.tolerance = t; }
        let (w, h) = (lobby.width, lobby.height);
        let cam = lobby.camera;
        let dt = 1.0 / 30.0;
        let frames = (seconds / dt) as usize;
        let (mut t_update, mut t_list, mut t_paths) = (0f64, 0f64, 0f64);
        let (mut words, mut tris, mut calls, mut baked_frames) = (0usize, 0usize, 0usize, 0usize);
        let head = lobby.grab_box().map(|(x, y, bw, bh)| (x + bw / 2.0, y + bh / 2.0));
        for f in 0..frames {
            if touch {
                let t = f as f32 * dt;
                // A tap every 9 s, a 2 s pat every 9 s (offset), after the intro.
                if t > 10.0 {
                    let phase = (t - 10.0) % 9.0;
                    if (phase - 0.0).abs() < dt * 0.5 {
                        lobby.touch(bal2d_core::Touch::Down, w * 0.5, h * 0.75);
                    } else if (phase - 0.1).abs() < dt * 0.5 {
                        lobby.touch(bal2d_core::Touch::Up, w * 0.5, h * 0.75);
                    }
                    if let Some((hx, hy)) = head {
                        if (phase - 6.0).abs() < dt * 0.5 {
                            lobby.touch(bal2d_core::Touch::Down, hx, hy);
                        } else if phase > 6.0 && phase < 8.0 {
                            let k = (phase - 6.0) * 3.0;
                            lobby.touch(bal2d_core::Touch::Move, hx + 30.0 * k.sin(), hy);
                        } else if (phase - 8.0).abs() < dt * 0.5 {
                            lobby.touch(bal2d_core::Touch::Up, hx, hy);
                        }
                    }
                }
            }
            let t0 = std::time::Instant::now();
            lobby.update(dt);
            lobby.notices.clear();
            t_update += t0.elapsed().as_secs_f64();
            baked_frames += lobby.bake_active() as usize;
            for li in 0..lobby.layers.len() {
                let t1 = std::time::Instant::now();
                lobby.draw_list(li, &mut list);
                t_list += t1.elapsed().as_secs_f64();
                let t2 = std::time::Instant::now();
                let ps = &sizes[li];
                paths.draw(li, &list, (cam.cx, cam.cy, cam.scale), (w, h), |p| ps.get(p as usize).copied(), |c| {
                    words += c.path.len();
                    calls += 1;
                });
                t_paths += t2.elapsed().as_secs_f64();
            }
            tris += paths.triangles as usize;
            paths.reset_stats();
        }
        let n = frames as f64;
        println!(
            "{path}: per frame on the host: update {:.3} ms, draw list {:.3} ms, paths {:.3} ms; {:.0} triangles, {:.0} calls, {:.1} KB path data; bake active {:.0}% of frames",
            t_update * 1000.0 / n,
            t_list * 1000.0 / n,
            t_paths * 1000.0 / n,
            tris as f64 / n,
            calls as f64 / n,
            words as f64 * 2.0 / 1024.0 / n,
            baked_frames as f64 * 100.0 / n
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("render") => render(&args[1..]),
        Some("compile") => compile(&args[1..]),
        Some("compare") => compare(&args[1..]),
        Some("play") => play(&args[1..]),
        Some("calls") => calls(&args[1..]),
        Some("bench") => bench(&args[1..]),
        Some("face-info") => face_info(&args[1..]),
        Some("face-patch") => face_patch(&args[1..]),
        _ => {
            eprintln!("usage: bal2d-tool render --skel A.skel [--skel B.skel] --scenario s.json --size WxH --out DIR [--every N] [--video] [--camera auto|cx,cy,scale] [--zoom Z]");
            std::process::exit(2);
        }
    }
}

/// `BAL2D_GPU_TOL=0.75`: render the way the watch's GPU draws (one affine per triangle group, with
/// this clustering tolerance) instead of exactly.
fn gpu_tolerance() -> Option<f32> {
    std::env::var("BAL2D_GPU_TOL").ok().and_then(|v| v.parse().ok())
}

//! Layers that never show.
//!
//! A lobby made of several skeletons can have one that ends up entirely behind another: CH0184's
//! `CH0184_00` lies under the opaque scenery of `CH0184_home` the whole time and would still cost
//! the watch half its triangles every frame. The compiled pack is played the way the watch plays
//! it (intro, idle, taps on the body, pats on the head) and drawn at half resolution with and
//! without each layer; a layer that never changes more than a few stray pixels is left out of
//! the pack.

use bal2d_core::{Lobby, Touch};
use bal2d_render::DrawList;

use crate::raster::{self, Camera, Framebuffer, Texture};

/// Pixels (at half resolution) a frame may differ by without the layer counting as visible:
/// an edge pixel that rounds differently once in a while (CH0184's hidden layer differs by one
/// pixel in one frame of 375).
const MAX_STRAY_PIXELS: usize = 8;
/// Seconds played (intro included) and frames sampled per second.
const SECONDS: f32 = 75.0;
const SAMPLES_PER_SECOND: usize = 3;

/// The pack's layers, other than the interactive one, that change no pixel in any sampled
/// frame. The pack should be compiled without a baked background (it would hide what layers
/// contribute to it).
pub fn hidden_layers(pack: &[u8]) -> Result<Vec<usize>, String> {
    let (mut lobby, pages) = Lobby::load(pack).map_err(|e| format!("{e:?}"))?;
    let n = lobby.layers.len();
    if n < 2 {
        return Ok(Vec::new());
    }
    let mut textures: Vec<Vec<Texture>> = (0..n).map(|_| Vec::new()).collect();
    for p in &pages {
        let mut section = vec![0u8; p.section_len()];
        bal2d_core::read_page(&mut &pack[..], p, &mut section).map_err(|e| format!("{e:?}"))?;
        let img = crate::image::decode_page(p.header.width as u32, p.header.height as u32, p.header.format, &section[8..]);
        textures[p.layer as usize].push(Texture::from_premultiplied_rgba8(img.width as usize, img.height as usize, &img.data));
    }
    lobby.start();
    let main = lobby.interactions.layer as usize;
    let (w, h) = (lobby.width, lobby.height);
    let c = lobby.camera;
    // Half resolution: the camera scales about the centre of the (halved) screen.
    let cam = Camera { cx: c.cx, cy: c.cy, scale: c.scale * 0.5 };
    let (fw, fh) = ((w as usize / 2).max(1), (h as usize / 2).max(1));
    let head = lobby.grab_box().map(|(x, y, bw, bh)| (x + bw / 2.0, y + bh / 2.0));
    let mut shows = vec![false; n];
    shows[main] = true;
    let mut list = DrawList::default();
    let dt = 1.0 / 30.0;
    let every = (30 / SAMPLES_PER_SECOND).max(1);
    let render = |lobby: &Lobby, list: &mut DrawList, skip: Option<usize>| {
        let mut fb = Framebuffer::new(fw, fh, [0.0, 0.0, 0.0, 1.0]);
        for li in 0..n {
            if Some(li) != skip {
                lobby.draw_list(li, list);
                raster::draw(&mut fb, list, &textures[li], cam);
            }
        }
        fb.to_rgba8()
    };
    for f in 0..(SECONDS / dt) as usize {
        let t = f as f32 * dt;
        // After the intro: a tap on the body every 9 s, a 2 s pat on the head 6 s later.
        if t > 10.0 {
            let phase = (t - 10.0) % 9.0;
            if phase < dt {
                lobby.touch(Touch::Down, w * 0.5, h * 0.75);
            } else if (phase - 0.1).abs() < dt * 0.5 {
                lobby.touch(Touch::Up, w * 0.5, h * 0.75);
            }
            if let Some((hx, hy)) = head {
                if (phase - 6.0).abs() < dt * 0.5 {
                    lobby.touch(Touch::Down, hx, hy);
                } else if phase > 6.0 && phase < 8.0 {
                    lobby.touch(Touch::Move, hx + 30.0 * ((phase - 6.0) * 3.0).sin(), hy);
                } else if (phase - 8.0).abs() < dt * 0.5 {
                    lobby.touch(Touch::Up, hx, hy);
                }
            }
        }
        lobby.update(dt);
        lobby.notices.clear();
        if f % every != 0 {
            continue;
        }
        let all = render(&lobby, &mut list, None);
        for li in 0..n {
            if shows[li] {
                continue;
            }
            let without = render(&lobby, &mut list, Some(li));
            // Any visible difference (beyond rounding) counts.
            let diff = all.chunks_exact(4).zip(without.chunks_exact(4)).filter(|(a, b)| a.iter().zip(*b).any(|(x, y)| x.abs_diff(*y) > 2)).count();
            // A few pixels where edges round differently are not the layer showing.
            shows[li] = diff > MAX_STRAY_PIXELS;
            if shows[li] && std::env::var("BAL2D_DEBUG_HIDDEN").is_ok() {
                eprintln!("layer {li} shows at {t:.2}s ({diff} px at half resolution)");
            }
        }
        if shows.iter().all(|s| *s) {
            break;
        }
    }
    Ok((0..n).filter(|&li| !shows[li]).collect())
}

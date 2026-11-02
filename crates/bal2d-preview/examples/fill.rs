//! Pixels the GPU fills per frame in the baked idle, per layer and the biggest slots:
//! `fill PACK`.
use std::collections::HashMap;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).unwrap();
    let mut pv = bal2d_preview::PackPreview::load(&bytes).unwrap();
    let mut t = 0.0;
    while !pv.lobby.idle_ready() && t < 120.0 {
        pv.advance(0.5);
        t += 0.5;
    }
    let (w, h) = pv.size();
    let mut by_slot: HashMap<(usize, String), usize> = HashMap::new();
    let mut total = 0usize;
    for li in 0..pv.lobby.layers.len() {
        let mut fb = bal2d_preview::Framebuffer::new(w, h, [0.0; 4]);
        let mut slots = Vec::new();
        let mut blends = Vec::new();
        pv.idle_calls(li, |call, tex| {
            slots.push(bal2d_preview::raster::draw_call(&mut fb, call, tex));
            blends.push((call.blend, call.color[3]));
        });
        // Per slot, through the bake's shapes in call order.
        let cur: Vec<u16> = {
            let l = pv.lobby.idle[li].as_ref().unwrap();
            l.current_shapes()
        };
        for (i, sh) in cur.iter().enumerate() {
            if i < slots.len() {
                let l = pv.lobby.idle[li].as_ref().unwrap();
                let slot = l.bake.shapes[*sh as usize].slot as usize;
                *by_slot.entry((li, format!("{} {:?}", pv.lobby.layers[li].data.slots[slot].name, blends[i].0))).or_default() += slots[i];
            }
        }
        let sum: usize = slots.iter().sum();
        total += sum;
        println!("layer {li}: {} calls, {} pixels ({:.1} screens)", slots.len(), sum, sum as f32 / (w * h) as f32);

    }
    let mut v: Vec<_> = by_slot.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    for ((li, name), px) in v.iter().take(15) {
        println!("  layer {li} {name}: {:.2} screens", *px as f32 / (w * h) as f32);
    }
    println!("total {:.1} screens of fill; at 7 B/px and 400 MB/s: {:.1} ms", total as f32 / (w * h) as f32, total as f32 * 7.0 / 400e6 * 1000.0);
}

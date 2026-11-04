//! Development: taps on a pack's lobby as the watch feeds them (15 fps, baked idles, resume on
//! touch; each press lasts two frames), printing the lobby's state at each press and the talks
//! that start.
//!
//!     cargo run --release -p bal2d-core --example tapsim PACK [X Y]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("PACK");
    let at = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(200.0);
    let (x, y) = (at(2), at(3));
    let bytes = std::fs::read(path).unwrap();
    let (mut lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
    for d in &lobby.interactions.drags {
        println!("drag {} box {}x{} delay {} ing {} / {}", d.bone, d.box_w, d.box_h, d.delay, d.ing_m, d.ing_a);
    }
    println!("talks {:?}", lobby.interactions.talks);
    let dt = 1.0 / 15.0;
    let taps = [8.3f32, 18.0, 20.0, 30.0, 32.0];
    let mut t = 0.0f32;
    let mut down: Option<f32> = None;
    let mut ti = 0;
    while t < 45.0 {
        if ti < taps.len() && t >= taps[ti] && down.is_none() {
            lobby.resume();
            println!("{t:5.2} down: idle {} free {} grabs {} box {:?}", lobby.is_idle(), lobby.is_free(), lobby.grabs(x, y), lobby.grab_box());
            lobby.touch(bal2d_core::Touch::Down, x, y);
            down = Some(t);
        } else if down.is_some_and(|d| t - d >= 0.08) {
            lobby.resume();
            lobby.touch(bal2d_core::Touch::Up, x, y);
            down = None;
            ti += 1;
        }
        lobby.update_with(dt, true);
        for n in lobby.notices.drain(..) {
            if let bal2d_core::Notice::TalkStarted(i) = n {
                println!("{t:5.2} talk {i}");
            }
        }
        t += dt;
    }
}

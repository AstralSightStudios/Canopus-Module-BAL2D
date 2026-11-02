//! Smallest thread stack that survives Lobby::load (host approximation of the watch's UI task).
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let bytes = std::sync::Arc::new(std::fs::read(path).unwrap());
    for kb in [2usize, 4, 6, 8, 12, 16, 24, 32, 48, 64] {
        let b = bytes.clone();
        let ok = std::thread::Builder::new()
            .stack_size(kb * 1024)
            .spawn(move || {
                let (mut lobby, _) = bal2d_core::Lobby::load(&b).unwrap();
                for _ in 0..10 {
                    lobby.update(0.033);
                }
            })
            .unwrap()
            .join()
            .is_ok();
        println!("{kb:3} KB stack: {}", if ok { "ok" } else { "overflow/panic" });
        if ok {
            break;
        }
    }
}

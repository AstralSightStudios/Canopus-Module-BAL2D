//! Intro and idle lengths: `intros PACK...` prints each layer's Start_Idle_01 / Idle_01 durations.
fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (lobby, _) = bal2d_core::Lobby::load(&bytes).unwrap();
        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
        let d: Vec<String> = lobby
            .layers
            .iter()
            .map(|l| {
                let dur = |n: &str| l.data.animations.iter().find(|a| a.name == n).map_or(-1.0, |a| a.duration);
                format!("intro {:.1}s idle {:.1}s", dur("Start_Idle_01"), dur("Idle_01"))
            })
            .collect();
        println!("{name:14} {}", d.join(" | "));
    }
}

//! How much row prediction would shrink a pack's pages: `deltasize PACK...`.
fn main() {
    for path in std::env::args().skip(1) {
        let pack = std::fs::read(&path).unwrap();
        let (_, pages) = bal2d_core::Lobby::load(&pack).unwrap();
        let (mut raw_z, mut sub_z, mut up_z, mut avg_z, mut total) = (0usize, 0usize, 0usize, 0usize, 0usize);
        for p in &pages {
            let mut section = vec![0u8; p.section_len()];
            bal2d_core::read_page(&mut &pack[..], p, &mut section).unwrap();
            let px = &section[p.prefix()..];
            let bpp = p.header.format.bytes_per_pixel();
            let stride = p.header.width as usize * bpp;
            total += px.len();
            let z = |d: &[u8]| miniz_oxide::deflate::compress_to_vec(d, 9).len();
            raw_z += z(px);
            let mut sub = px.to_vec();
            for i in (bpp..px.len()).rev() {
                if i % stride >= bpp {
                    sub[i] = px[i].wrapping_sub(px[i - bpp]);
                }
            }
            sub_z += z(&sub);
            let mut up = px.to_vec();
            for i in (stride..px.len()).rev() {
                up[i] = px[i].wrapping_sub(px[i - stride]);
            }
            up_z += z(&up);
            let mut avg = px.to_vec();
            for i in 0..px.len() {
                let a = if i % stride >= bpp { px[i - bpp] as u16 } else { 0 };
                let b = if i >= stride { px[i - stride] as u16 } else { 0 };
                avg[i] = px[i].wrapping_sub(((a + b) / 2) as u8);
            }
            avg_z += z(&avg);
        }
        println!(
            "{path}: pages {:.0} KB raw; deflate {:.0} KB, sub {:.0} KB, up {:.0} KB, avg {:.0} KB; pack {:.0} KB",
            total as f64 / 1024.0,
            raw_z as f64 / 1024.0,
            sub_z as f64 / 1024.0,
            up_z as f64 / 1024.0,
            avg_z as f64 / 1024.0,
            pack.len() as f64 / 1024.0
        );
    }
}

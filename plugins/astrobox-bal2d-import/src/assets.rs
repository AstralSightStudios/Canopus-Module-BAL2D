//! Picked files -> compiler input: recognises each file by its content (a zip is opened and its
//! entries recognised the same way), pairs skeletons with atlases and atlases with their PNG pages,
//! then compiles the pack and renders a preview frame.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use bal2d_compiler::{LayerInput, Options};
use bal2d_pack::PageFormat;
use bal2d_spine::{Atlas, bind_atlas, read_skeleton};

use crate::state::{Asset, Built, FileKind};

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Recognises one file; a zip yields every recognised entry. Unknown content is skipped.
pub fn recognise(name: &str, data: Vec<u8>, out: &mut Vec<Asset>) -> usize {
    if data.starts_with(b"PK\x03\x04") {
        let Ok(entries) = bal2d_face::zip::read(&data) else { return 0 };
        let mut n = 0;
        for (entry, bytes) in entries {
            if entry.ends_with('/') || entry.starts_with("__MACOSX") {
                continue;
            }
            let short = entry.rsplit('/').next().unwrap_or(&entry).to_string();
            n += recognise(&short, bytes, out);
        }
        return n;
    }
    let kind = if data.starts_with(PNG_MAGIC) {
        Some(FileKind::Png)
    } else if looks_like_atlas(&data) {
        Some(FileKind::Atlas)
    } else if data.first().is_some_and(|b| *b == b'{' || *b == b'[') || data.starts_with(b"\xef\xbb\xbf{") {
        Some(FileKind::Interactions)
    } else if read_skeleton(&data, 1.0).is_ok() {
        Some(FileKind::Skeleton)
    } else {
        None
    };
    match kind {
        Some(kind) => {
            out.retain(|a| a.name != name || a.kind != kind);
            out.push(Asset { name: name.to_string(), kind, data });
            1
        }
        None => 0,
    }
}

fn looks_like_atlas(data: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(&data[..data.len().min(4096)]) else { return false };
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    first.to_ascii_lowercase().ends_with(".png") && text.contains("size:")
}

fn stem(name: &str) -> &str {
    let base = name.rsplit('/').next().unwrap_or(name);
    base.split_once('.').map(|(s, _)| s).unwrap_or(base)
}

/// PNG width and height from the IHDR chunk.
fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 24 || !data.starts_with(PNG_MAGIC) {
        return None;
    }
    let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    Some((w, h))
}

/// How many of the page's regions have an opaque centre texel in `png`.
fn region_hits(png: &Asset, atlas: &Atlas, page: u32) -> usize {
    let Ok(img) = bal2d_compiler::image::decode_png(&png.data) else { return 0 };
    atlas
        .regions
        .iter()
        .filter(|r| r.page == page)
        .filter(|r| {
            // Packed size: rotated regions swap width and height.
            let (w, h) = if r.degrees == 90 || r.degrees == 270 { (r.height, r.width) } else { (r.width, r.height) };
            let (x, y) = ((r.x + w / 2) as u32, (r.y + h / 2) as u32);
            x < img.width && y < img.height && img.data[((y * img.width + x) * 4 + 3) as usize] > 0
        })
        .count()
}

/// The skeletons with their atlas and pages, background layers first and the lobby with the
/// touch bones last (the compiler takes the layer with `Touch_Point` as the main one).
pub fn layers(assets: &[Asset]) -> Result<Vec<LayerInput>, String> {
    let skels: Vec<&Asset> = assets.iter().filter(|a| a.kind == FileKind::Skeleton).collect();
    if skels.is_empty() {
        return Err("还没有骨骼文件（.skel）。".into());
    }
    let atlases: Vec<(&Asset, String, Atlas)> = assets
        .iter()
        .filter(|a| a.kind == FileKind::Atlas)
        .map(|a| {
            let text = String::from_utf8_lossy(&a.data).into_owned();
            let parsed = Atlas::parse(&text);
            (a, text, parsed)
        })
        .collect();
    let pngs: Vec<&Asset> = assets.iter().filter(|a| a.kind == FileKind::Png).collect();
    let mut out: Vec<(bool, LayerInput)> = Vec::new();
    for skel in skels {
        let data = read_skeleton(&skel.data, 1.0).map_err(|e| format!("{}：无法读取骨骼（{e:?}）", skel.name))?;
        // The atlas with the same name, else the one that leaves the fewest regions unresolved.
        let pick = atlases.iter().position(|(a, _, _)| stem(&a.name) == stem(&skel.name)).or_else(|| {
            atlases
                .iter()
                .enumerate()
                .map(|(i, (_, _, atlas))| {
                    let mut d = data.clone();
                    (i, bind_atlas(&mut d, atlas).len())
                })
                .min_by_key(|x| x.1)
                .map(|x| x.0)
        });
        let Some(ai) = pick else {
            return Err(format!("{}：没有找到配套的图集（.atlas）。", skel.name));
        };
        let (_, text, atlas) = &atlases[ai];
        let mut d = data.clone();
        let missing = bind_atlas(&mut d, atlas).len();
        if missing * 2 > d.attachments.len().max(1) {
            return Err(format!("{}：图集与骨骼不匹配（{missing} 个区域缺失）。", skel.name));
        }
        let mut pages = Vec::new();
        for (pi, page) in atlas.pages.iter().enumerate() {
            let png = pngs
                .iter()
                .find(|p| p.name == page.name)
                .or_else(|| pngs.iter().find(|p| stem(&p.name) == stem(&page.name)))
                .copied()
                .or_else(|| {
                    // Content-URI names: among the PNGs of the page's size, the one whose pixels are
                    // opaque where the atlas puts its regions.
                    let same: Vec<&Asset> = pngs.iter().copied().filter(|p| png_size(&p.data) == Some((page.width, page.height))).collect();
                    match same.len() {
                        0 => None,
                        1 => Some(same[0]),
                        _ => same.into_iter().max_by_key(|p| region_hits(p, atlas, pi as u32)),
                    }
                })
                .ok_or_else(|| format!("缺少贴图 {}（{}×{}）。", page.name, page.width, page.height))?;
            pages.push((page.name.clone(), png.data.clone()));
        }
        let main = data.find_bone("Touch_Point").is_some();
        out.push((main, LayerInput { name: stem(&skel.name).to_string(), skel: skel.data.clone(), atlas: text.clone(), pages }));
    }
    out.sort_by_key(|(main, l)| (*main, l.name.clone()));
    Ok(out.into_iter().map(|x| x.1).collect())
}

/// The suggested display name: the main skeleton's, without `_home`.
pub fn suggested_name(assets: &[Asset]) -> String {
    layers(assets)
        .ok()
        .and_then(|l| l.last().map(|l| l.name.trim_end_matches("_home").to_string()))
        .unwrap_or_default()
}

/// The prefab entry for the main skeleton in a community interaction dump (`interactions.json`:
/// keys like `SpineLobbies/<lobby>/<skeleton>`), if one was added.
fn interactions(assets: &[Asset], layers: &[LayerInput]) -> Option<bal2d_pack::Interactions> {
    let main = layers.last()?;
    let file = assets.iter().find(|a| a.kind == FileKind::Interactions)?;
    let text = std::str::from_utf8(&file.data).ok()?;
    let v = bal2d_compiler::interactions::json::parse(text.trim_start_matches('\u{feff}'))?;
    let entry = v
        .obj()
        .iter()
        .find(|(k, _)| k.rsplit('/').next() == Some(main.name.as_str()))
        .map(|(_, e)| e)
        .or_else(|| v.get("drags").map(|_| &v))?;
    let data = read_skeleton(&main.skel, 1.0).ok()?;
    Some(bal2d_compiler::interactions::from_prefab(&data, (layers.len() - 1) as u8, entry))
}

/// Texel budget. Pages are ARGB8565 (3 bytes, 565 colour like the 16-bit screen, 8-bit alpha:
/// no dithering grain); packing adds about a fifth. The Band 10 Pro has about 10 MB of heap
/// free and a big lobby's skeletons take up to 3 MB of it, so standard keeps pages near 2.9 MB
/// and high near 4.3 MB.
fn budget(high: bool) -> u32 {
    if high { 1_200_000 } else { 800_000 }
}

pub struct BuildRequest {
    pub assets: Vec<Asset>,
    pub name: String,
    pub screen: (u32, u32),
    pub zoom: u32,
    pub high_quality: bool,
}

/// Compiles the pack and renders a frame after the intro.
pub fn build(req: &BuildRequest) -> Result<Built, String> {
    let layers = layers(&req.assets)?;
    let format = PageFormat::Argb8565;
    let name = if req.name.trim().is_empty() { layers.last().map(|l| l.name.clone()).unwrap_or_default() } else { req.name.trim().to_string() };
    let opts = Options {
        width: req.screen.0,
        height: req.screen.1,
        zoom: req.zoom as f32 / 100.0,
        format,
        budget: budget(req.high_quality),
        name: name.clone(),
        ..Default::default()
    };
    let inter = interactions(&req.assets, &layers);
    let original: usize = layers.iter().map(|l| l.skel.len() + l.pages.iter().map(|p| p.1.len()).sum::<usize>()).sum();
    let (bytes, report) = bal2d_compiler::compile(&layers, &opts, inter).map_err(|e| format!("编译失败：{e}"))?;
    let mut preview = bal2d_preview::PackPreview::load(&bytes).map_err(|e| format!("预览失败：{e:?}"))?;
    preview.advance(7.0);
    let rgba = preview.render();
    let (w, h) = preview.size();
    let png = bal2d_compiler::image::encode_png(&bal2d_compiler::image::Rgba { width: w as u32, height: h as u32, data: rgba });
    let pages: usize = report.layers.iter().map(|l| l.pages.len()).sum();
    let summary = format!(
        "{} 层 · {} 张贴图 · {:.0} KB（原始素材 {:.1} MB）{}",
        report.layers.len(),
        pages,
        bytes.len() as f64 / 1024.0,
        original as f64 / 1048576.0,
        if report.budget_scale < 0.999 { format!(" · 为适配手表内存缩小到 {:.0}%", report.budget_scale * 100.0) } else { String::new() }
    );
    // The same lobby always lands on the same id, so sending it again replaces it on the watch.
    let id = bal2d_proto::crc32(layers.last().map(|l| l.name.as_bytes()).unwrap_or(b"")) | 1;
    Ok(Built {
        bytes,
        name,
        id,
        screen: req.screen,
        preview: format!("data:image/png;base64,{}", BASE64.encode(png)),
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "/Volumes/EXT0/tmp/bal2d-ext/assets/Hoshino_home";

    /// The Hoshino lobby (two skeletons) as picked files, or `None` when the local copy is absent.
    fn files() -> Option<Vec<(String, Vec<u8>)>> {
        let mut out = Vec::new();
        for sub in ["Hoshino_home", "Hoshino_home_background"] {
            for e in std::fs::read_dir(format!("{DIR}/{sub}")).ok()?.flatten() {
                out.push((e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).ok()?));
            }
        }
        Some(out)
    }

    #[test]
    fn files_are_recognised_and_paired_even_without_names() {
        let Some(files) = files() else { return };
        for anonymous in [false, true] {
            let mut assets = Vec::new();
            for (i, (name, data)) in files.iter().enumerate() {
                // Android hands out content-URI names without extensions.
                let name = if anonymous { format!("document%3A{}", 1000 + i) } else { name.clone() };
                assert_eq!(recognise(&name, data.clone(), &mut assets), 1, "{name}");
            }
            let layers = layers(&assets).unwrap();
            assert_eq!(layers.len(), 2);
            assert_eq!(layers[1].pages.len(), 1, "main layer pages");
            assert_eq!(layers[0].pages.len(), 2, "background pages");
            if !anonymous {
                assert_eq!(layers[1].name, "Hoshino_home");
                assert_eq!(suggested_name(&assets), "Hoshino");
            }
        }
    }

    #[test]
    fn a_zip_is_opened() {
        let Some(files) = files() else { return };
        let zip = bal2d_face::zip::write(&files);
        let mut assets = Vec::new();
        assert_eq!(recognise("hoshino.zip", zip, &mut assets), files.len());
        assert_eq!(layers(&assets).unwrap().len(), 2);
    }

    #[test]
    fn a_pack_is_built_with_a_preview() {
        let Some(files) = files() else { return };
        let mut assets = Vec::new();
        for (name, data) in files {
            recognise(&name, data, &mut assets);
        }
        let t0 = std::time::Instant::now();
        let built = build(&BuildRequest { assets, name: String::new(), screen: (336, 480), zoom: 100, high_quality: false }).unwrap();
        eprintln!("built in {:?}: {}", t0.elapsed(), built.summary);
        assert!(built.bytes.starts_with(b"BL2D"));
        assert!(built.preview.starts_with("data:image/png;base64,"));
        assert_eq!(built.screen, (336, 480));
        // The pages stay within about 3 MB.
        let (lobby, pages) = bal2d_core::Lobby::load(&built.bytes).unwrap();
        let texels: u64 = pages.iter().map(|p| p.header.width as u64 * p.header.height as u64).sum();
        assert!(texels * 3 <= 3_300_000, "{texels} texels");
        assert_eq!(lobby.width as u32, 336);
    }
}

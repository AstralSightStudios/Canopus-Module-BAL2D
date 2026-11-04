//! The online library's downloads: where each file lives (GitHub repositories, reached through
//! jsDelivr, GitHub directly or a user-given proxy prefix, falling back in order and remembering
//! what worked), the catalog, a lobby's files, and the dialogue tables (fetched once per session;
//! only the extracted talks are kept).

use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::library::{self, DialogMap, Lang, LibEntry};
use crate::net;

/// A way to reach a file in a GitHub repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    JsdCdn,
    JsdFastly,
    GitHub,
    Custom,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::JsdCdn => "jsDelivr",
            Source::JsdFastly => "jsDelivr（Fastly）",
            Source::GitHub => "GitHub",
            Source::Custom => "自定义线路",
        }
    }
}

/// The download route chosen in the UI: `auto` (or empty), `jsdelivr`, `github` or `custom`
/// (`prefix` + the GitHub raw URL).
#[derive(Clone, Debug, Default)]
pub struct Route {
    pub kind: String,
    pub prefix: String,
}

impl Route {
    fn prefix(&self) -> Option<String> {
        let p = self.prefix.trim();
        if p.is_empty() || net::split_url(p).is_none() {
            return None;
        }
        Some(if p.ends_with('/') { p.to_string() } else { format!("{p}/") })
    }
}

/// A file in a GitHub repository.
#[derive(Clone, Debug)]
pub struct Gh {
    pub owner: &'static str,
    pub repo: &'static str,
    pub branch: &'static str,
    pub path: String,
}

impl Gh {
    /// A file of the lobby asset repository (`memorial/assets/<file>`).
    pub fn asset(file: &str) -> Gh {
        Gh { owner: "asdfdsa12", repo: "BA-Spine-Viewer-Asset", branch: "main", path: format!("memorial/assets/{file}") }
    }

    fn catalog() -> Gh {
        Gh { owner: "asdfdsa12", repo: "BA-Spine-Viewer", branch: "main", path: "memorial/Characters.json".into() }
    }

    /// A table of the game's data (`global` or `jp` branch).
    fn table(branch: &'static str, name: &str) -> Gh {
        Gh { owner: "electricgoat", repo: "ba-data", branch, path: format!("DB/{name}.json") }
    }

    /// SchaleDB's repository copy (older than the site; used when the site is unreachable).
    fn students() -> Gh {
        Gh { owner: "SchaleDB", repo: "SchaleDB", branch: "main", path: "data/cn/students.min.json".into() }
    }

    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn url(&self, src: Source, route: &Route) -> Option<String> {
        let (o, r, b, p) = (self.owner, self.repo, self.branch, encode_path(&self.path));
        let raw = format!("https://raw.githubusercontent.com/{o}/{r}/{b}/{p}");
        Some(match src {
            Source::JsdCdn => format!("https://cdn.jsdelivr.net/gh/{o}/{r}@{b}/{p}"),
            Source::JsdFastly => format!("https://fastly.jsdelivr.net/gh/{o}/{r}@{b}/{p}"),
            Source::GitHub => raw,
            Source::Custom => format!("{}{raw}", route.prefix()?),
        })
    }
}

/// SchaleDB's own copy, the most up to date.
const STUDENTS_URL: &str = "https://schaledb.com/data/cn/students.min.json";

fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/@!$&'()*+,;=:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The source that last worked this session (tried first).
static WORKED: Mutex<Option<Source>> = Mutex::new(None);

/// Forgets the source that worked (the route was changed in the UI).
pub fn forget_source() {
    *WORKED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The sources to try, in order.
pub fn order(route: &Route) -> Vec<Source> {
    use Source::*;
    let mut list = match route.kind.as_str() {
        "jsdelivr" => vec![JsdCdn, JsdFastly],
        "github" => vec![GitHub],
        "custom" if route.prefix().is_some() => vec![Custom, JsdCdn, JsdFastly, GitHub],
        _ => vec![JsdCdn, JsdFastly, GitHub],
    };
    if let Some(w) = *WORKED.lock().unwrap_or_else(|e| e.into_inner()) {
        if let Some(i) = list.iter().position(|s| *s == w) {
            let s = list.remove(i);
            list.insert(0, s);
        }
    }
    list
}

/// What a downloaded file must look like (a proxy or a captive portal may answer 200 with a page).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Json,
    Png,
    Text,
    Binary,
}

/// The body without its gzip layer (if it has one).
pub fn decoded(bytes: &[u8]) -> Box<dyn Read + '_> {
    if bytes.starts_with(&[0x1f, 0x8b]) { Box::new(flate2::read::MultiGzDecoder::new(bytes)) } else { Box::new(bytes) }
}

fn decoded_vec(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(bytes);
    }
    let mut out = Vec::with_capacity(bytes.len() * 4);
    decoded(&bytes).read_to_end(&mut out).map_err(|e| format!("解压失败（{e}）"))?;
    Ok(out)
}

pub fn plausible(expect: Expect, bytes: &[u8]) -> bool {
    let mut head = [0u8; 256];
    let n = decoded(bytes).read(&mut head).unwrap_or(0);
    let head = &head[..n];
    let text = String::from_utf8_lossy(head);
    let lower = text.trim_start().to_ascii_lowercase();
    let html = lower.starts_with("<!doctype html") || lower.starts_with("<html");
    match expect {
        Expect::Json => matches!(text.trim_start_matches('\u{feff}').trim_start().chars().next(), Some('{' | '[')),
        Expect::Png => head.starts_with(b"\x89PNG\r\n\x1a\n"),
        Expect::Text => !head.is_empty() && !html,
        Expect::Binary => !head.is_empty() && !html,
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1048576.0)
}

/// "1.2/3.9 MB", or "1.2 MB" when the length is unknown.
pub fn amount(got: u64, total: Option<u64>) -> String {
    match total {
        Some(t) if t > 0 => format!("{}/{} MB", mb(got), mb(t)),
        _ => format!("{} MB", mb(got)),
    }
}

/// Downloads a file through the route's sources in order. `progress` gets bytes and length.
pub fn fetch(file: &Gh, route: &Route, gzip: bool, expect: Expect, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Vec<u8>, String> {
    let mut errors: Vec<String> = Vec::new();
    for src in order(route) {
        let Some(url) = file.url(src, route) else { continue };
        tracing::info!("GET {url}");
        match net::get(&url, gzip, progress) {
            Ok(body) if plausible(expect, &body) => {
                *WORKED.lock().unwrap_or_else(|e| e.into_inner()) = Some(src);
                CACHE.lock().unwrap_or_else(|e| e.into_inner()).failed = None;
                return Ok(body);
            }
            Ok(_) => errors.push(format!("{}：返回的不是预期的文件", src.label())),
            Err(e) => {
                tracing::warn!("{url}: {e}");
                errors.push(format!("{}：{e}", src.label()));
            }
        }
    }
    Err(format!("下载 {} 失败（{}）", file.name(), errors.join("；")))
}

// -- session cache ------------------------------------------------------------------------------

struct Cache {
    /// Lowercased prefab name -> CharacterId, from both servers' MemoryLobby tables.
    prefabs: Option<HashMap<String, u32>>,
    global: Option<DialogMap>,
    jp: Option<DialogMap>,
    /// When a table last failed to download: builds do not wait for the network again soon
    /// after (cleared by any successful download).
    failed: Option<Instant>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache { prefabs: None, global: None, jp: None, failed: None });

const RETRY_AFTER: Duration = Duration::from_secs(180);

fn cache<T>(f: impl FnOnce(&mut Cache) -> T) -> T {
    f(&mut CACHE.lock().unwrap_or_else(|e| e.into_inner()))
}

fn note_failure<T>(r: Result<T, String>) -> Result<T, String> {
    if r.is_err() {
        cache(|c| c.failed = Some(Instant::now()));
    }
    r
}

fn recently_failed() -> bool {
    cache(|c| c.failed.is_some_and(|t| t.elapsed() < RETRY_AFTER))
}

/// The lobby -> student map (both servers' MemoryLobby tables), downloaded once.
fn prefabs(route: &Route, progress: &mut dyn FnMut(&str, f32), span: (f32, f32)) -> Result<HashMap<String, u32>, String> {
    if let Some(p) = cache(|c| c.prefabs.clone()) {
        return Ok(p);
    }
    let mut map = HashMap::new();
    let mut errors = Vec::new();
    for (i, (branch, label)) in [("global", "国际服"), ("jp", "日服")].into_iter().enumerate() {
        let at = span.0 + (span.1 - span.0) * i as f32 / 2.0;
        let body = fetch(&Gh::table(branch, "MemoryLobbyExcelTable"), route, true, Expect::Json, &mut |got, total| {
            progress(&format!("下载大厅对照表（{label}）{}", amount(got, total)), at)
        });
        if let Err(e) = body.and_then(|b| library::parse_prefabs(decoded(&b), &mut map)) {
            errors.push(e);
            if map.is_empty() {
                // Nothing came through: the network is down, do not wait for the second table.
                break;
            }
        }
    }
    if map.is_empty() {
        return note_failure(Err(errors.join("；")));
    }
    if errors.is_empty() {
        cache(|c| c.prefabs = Some(map.clone()));
    }
    Ok(map)
}

/// SchaleDB's Chinese names: the site, else its repository copy.
fn students(route: &Route, progress: &mut dyn FnMut(&str, f32), at: f32) -> Result<HashMap<u32, String>, String> {
    let mut show = |got, total| progress(&format!("下载中文名 {}", amount(got, total)), at);
    let body = match net::get(STUDENTS_URL, true, &mut show) {
        Ok(b) if plausible(Expect::Json, &b) => b,
        other => {
            if let Err(e) = other {
                tracing::warn!("{STUDENTS_URL}: {e}");
            }
            fetch(&Gh::students(), route, true, Expect::Json, &mut show)?
        }
    };
    library::parse_students(&decoded_vec(body)?)
}

/// Downloads the lobby list and joins it with the student names. The second value is a
/// warning when the names could not be had (the Korean names are shown instead).
pub fn load_catalog(route: &Route, progress: &mut dyn FnMut(&str, f32)) -> Result<(Vec<LibEntry>, Option<String>), String> {
    progress("下载角色列表", 0.0);
    let body = fetch(&Gh::catalog(), route, true, Expect::Json, &mut |got, total| progress(&format!("下载角色列表 {}", amount(got, total)), 0.05))?;
    let items = library::parse_characters(decoded(&body))?;
    let mut warning = None;
    let prefabs = prefabs(route, progress, (0.15, 0.5)).unwrap_or_else(|e| {
        tracing::warn!("prefabs: {e}");
        warning = Some("大厅对照表下载失败，暂时显示韩文名。".to_string());
        HashMap::new()
    });
    let names = if prefabs.is_empty() {
        HashMap::new()
    } else {
        students(route, progress, 0.6).unwrap_or_else(|e| {
            tracing::warn!("students: {e}");
            warning = Some("中文名下载失败，暂时显示韩文名。".to_string());
            HashMap::new()
        })
    };
    progress("整理角色列表", 0.95);
    Ok((library::join(&items, &prefabs, &names), warning))
}

/// Downloads a lobby: its atlases first (they name the texture pages), then the skeletons and
/// pages, background layers included. Returns (file name, bytes) in download order.
pub fn download_lobby(entry: &LibEntry, route: &Route, progress: &mut dyn FnMut(&str, f32)) -> Result<Vec<(String, Vec<u8>)>, String> {
    let skeletons: Vec<&str> = entry.extras.iter().map(String::as_str).chain([entry.id.as_str()]).collect();
    let mut files = Vec::new();
    let mut pages: Vec<String> = Vec::new();
    for (i, s) in skeletons.iter().enumerate() {
        let name = format!("{s}.atlas");
        let label = format!("下载图集 {}/{}：{name}", i + 1, skeletons.len());
        progress(&label, 0.02 * i as f32);
        let body = fetch(&Gh::asset(&name), route, true, Expect::Text, &mut |got, total| progress(&format!("{label} {}", amount(got, total)), 0.02 * i as f32))?;
        let body = decoded_vec(body)?;
        let atlas = bal2d_spine::Atlas::parse(&String::from_utf8_lossy(&body));
        if atlas.pages.is_empty() {
            return Err(format!("{name} 不是有效的图集。"));
        }
        for p in &atlas.pages {
            if !pages.contains(&p.name) {
                pages.push(p.name.clone());
            }
        }
        files.push((name, body));
    }
    let rest: Vec<(String, Expect)> =
        skeletons.iter().map(|s| (format!("{s}.skel"), Expect::Binary)).chain(pages.iter().map(|p| (p.clone(), Expect::Png))).collect();
    let n = rest.len();
    for (i, (name, expect)) in rest.into_iter().enumerate() {
        let base = 0.05 + 0.95 * i as f32 / n as f32;
        let label = format!("下载 {}/{n}：{name}", i + 1);
        progress(&label, base);
        let body = fetch(&Gh::asset(&name), route, false, expect, &mut |got, total| {
            let within = total.filter(|t| *t > 0).map(|t| got as f32 / t as f32).unwrap_or(0.0).min(1.0);
            progress(&format!("{label} {}", amount(got, total)), base + 0.95 * within / n as f32)
        })?;
        // Asked for without gzip: taken as it is (a skeleton may start with the gzip magic).
        files.push((name, body));
    }
    progress("下载完成", 1.0);
    Ok(files)
}

/// The dialogue table of one server, downloaded and extracted once per session.
fn talks_of(jp: bool, character: u32, route: &Route, progress: &mut dyn FnMut(&str, f32)) -> Result<Option<Vec<library::Talk>>, String> {
    if let Some(found) = cache(|c| if jp { c.jp.as_ref() } else { c.global.as_ref() }.map(|m| m.get(&character).cloned())) {
        return Ok(found);
    }
    let (branch, label) = if jp { ("jp", "日服") } else { ("global", "国际服") };
    let body = note_failure(fetch(&Gh::table(branch, "CharacterDialogExcelTable"), route, true, Expect::Json, &mut |got, total| {
        let f = total.filter(|t| *t > 0).map(|t| got as f32 / t as f32).unwrap_or(0.0);
        progress(&format!("下载对话台词（{label}）{}", amount(got, total)), f.min(1.0) * 0.9)
    }))?;
    progress(&format!("读取对话台词（{label}）"), 0.95);
    let map = library::extract_dialogue(decoded(&body))?;
    drop(body);
    let found = map.get(&character).cloned();
    cache(|c| if jp { c.jp = Some(map) } else { c.global = Some(map) });
    Ok(found)
}

/// The talks' lines for the lobby: its student is `character` (online lobbies) or found from the
/// file `names` (picked by hand). `Ok(None)` when the tables have nothing for it.
pub fn dialogue(character: Option<u32>, names: &[String], lang: Lang, route: &Route, progress: &mut dyn FnMut(&str, f32)) -> Result<Option<bal2d_pack::Dialogue>, String> {
    let cached = cache(|c| c.global.is_some() && (character.is_some() || c.prefabs.is_some()));
    if !cached && recently_failed() {
        return Err("网络不可用".into());
    }
    let character = match character {
        Some(c) => c,
        None => match library::character_for(names, &prefabs(route, progress, (0.0, 0.1))?) {
            Some(c) => c,
            None => return Ok(None),
        },
    };
    let talks = match talks_of(false, character, route, progress)? {
        Some(t) => t,
        // Students newer than the global server's tables.
        None => match talks_of(true, character, route, progress)? {
            Some(t) => t,
            None => return Ok(None),
        },
    };
    let d = library::dialogue(&talks, lang);
    Ok((!d.talks.is_empty()).then_some(d))
}

/// The lobby's thumbnail, through the source most likely to work (the webview loads it).
pub fn icon_url(lobby_id: &str, route: &Route) -> String {
    let base = lobby_id.strip_suffix("_home").unwrap_or(lobby_id);
    let file = Gh::asset(&format!("icons/Lobbyillust_Icon_{base}_01.png"));
    order(route).into_iter().find_map(|s| file.url(s, route)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_give_sources_in_order() {
        let auto = Route::default();
        let custom = Route { kind: "custom".into(), prefix: " https://ghfast.top ".into() };
        forget_source();
        assert_eq!(order(&auto), [Source::JsdCdn, Source::JsdFastly, Source::GitHub]);
        assert_eq!(order(&Route { kind: "github".into(), ..Default::default() }), [Source::GitHub]);
        assert_eq!(order(&custom)[0], Source::Custom);
        // A custom route without a usable prefix is the automatic one.
        assert_eq!(order(&Route { kind: "custom".into(), prefix: "ghfast.top".into() }), order(&auto));
        let f = Gh::asset("Hoshino_home.png");
        assert_eq!(f.url(Source::JsdCdn, &auto).unwrap(), "https://cdn.jsdelivr.net/gh/asdfdsa12/BA-Spine-Viewer-Asset@main/memorial/assets/Hoshino_home.png");
        assert_eq!(f.url(Source::JsdFastly, &auto).unwrap(), "https://fastly.jsdelivr.net/gh/asdfdsa12/BA-Spine-Viewer-Asset@main/memorial/assets/Hoshino_home.png");
        assert_eq!(f.url(Source::GitHub, &auto).unwrap(), "https://raw.githubusercontent.com/asdfdsa12/BA-Spine-Viewer-Asset/main/memorial/assets/Hoshino_home.png");
        assert_eq!(
            f.url(Source::Custom, &custom).unwrap(),
            "https://ghfast.top/https://raw.githubusercontent.com/asdfdsa12/BA-Spine-Viewer-Asset/main/memorial/assets/Hoshino_home.png"
        );
        assert!(f.url(Source::Custom, &auto).is_none());
        assert_eq!(Gh::asset("a b+.png").path, "memorial/assets/a b+.png");
        assert!(Gh::asset("a b.png").url(Source::GitHub, &auto).unwrap().ends_with("/a%20b.png"));
        assert_eq!(Gh::table("jp", "MemoryLobbyExcelTable").url(Source::JsdCdn, &auto).unwrap(), "https://cdn.jsdelivr.net/gh/electricgoat/ba-data@jp/DB/MemoryLobbyExcelTable.json");
        assert!(icon_url("Hoshino_home", &auto).ends_with("/memorial/assets/icons/Lobbyillust_Icon_Hoshino_01.png"));
        // What worked is tried first, within the route's sources.
        *WORKED.lock().unwrap() = Some(Source::GitHub);
        assert_eq!(order(&auto), [Source::GitHub, Source::JsdCdn, Source::JsdFastly]);
        assert_eq!(order(&Route { kind: "jsdelivr".into(), ..Default::default() }), [Source::JsdCdn, Source::JsdFastly]);
        forget_source();
    }

    #[test]
    fn gzip_bodies_are_unpacked_and_checked() {
        use std::io::Write;
        let json = br#"{"characters":[{"id":"Hoshino_home","name":"x","new":true}]}"#;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(json).unwrap();
        let gz = gz.finish().unwrap();
        assert!(plausible(Expect::Json, &gz) && plausible(Expect::Json, json));
        assert!(!plausible(Expect::Json, b"<!DOCTYPE html><html>blocked</html>"));
        assert!(!plausible(Expect::Text, b"<html><body>portal</body></html>"));
        assert!(plausible(Expect::Text, b"\nHoshino_home.png\nsize: 4096,4096\n"));
        assert!(!plausible(Expect::Png, b"GIF89a"));
        assert_eq!(library::parse_characters(decoded(&gz)).unwrap(), [("Hoshino_home".to_string(), "x".to_string(), true)]);
        assert_eq!(decoded_vec(gz).unwrap(), json);
        assert_eq!(amount(1258291, Some(4089446)), "1.2/3.9 MB");
        assert_eq!(amount(1258291, None), "1.2 MB");
    }
}

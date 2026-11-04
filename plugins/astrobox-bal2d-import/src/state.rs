//! The plugin's UI state: one value behind a mutex, read whole for rendering.

use std::sync::{Mutex, OnceLock};

use bal2d_proto::{Hello, PackInfo, Stats};

#[derive(Clone, Debug, Default)]
pub struct DeviceInfo {
    pub addr: String,
    pub name: String,
}

/// What a picked file is, judged by its content (Android gives content-URI names without
/// extensions).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Skeleton,
    Atlas,
    Png,
    /// A prefab interaction table (JSON).
    Interactions,
}

impl FileKind {
    pub fn label(self) -> &'static str {
        match self {
            FileKind::Skeleton => "骨骼 .skel",
            FileKind::Atlas => "图集 .atlas",
            FileKind::Png => "贴图 .png",
            FileKind::Interactions => "互动参数 .json",
        }
    }
}

/// One asset file, kept in memory (a lobby is a few MB). Shared, not copied, by the build: the
/// plugin has 128 MiB in all.
#[derive(Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub kind: FileKind,
    pub data: std::sync::Arc<[u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Import,
    Watch,
    Face,
}

/// A compiled pack waiting to be installed.
#[derive(Clone, Debug)]
pub struct Built {
    pub bytes: Vec<u8>,
    pub name: String,
    pub id: u32,
    pub screen: (u32, u32),
    /// PNG of a frame after the intro, as a data URL.
    pub preview: String,
    pub summary: String,
}

#[derive(Clone, Debug, Default)]
pub struct Transfer {
    pub sent: u64,
    pub total: u64,
    pub speed: u64,
}

#[derive(Clone, Debug, Default)]
pub struct FaceState {
    pub name: String,
    pub data: Vec<u8>,
    /// One line per layout entry of the first theme, bottom first.
    pub layers: Vec<String>,
    pub z: usize,
    pub has_marker: bool,
    /// The patched BIN, ready to install or save.
    pub patched: Option<(String, Vec<u8>)>,
}

/// The online character library card.
#[derive(Clone, Debug, Default)]
pub struct LibraryState {
    /// The lobbies, once the list has been loaded.
    pub entries: Vec<crate::library::LibEntry>,
    pub query: String,
    /// The list is being downloaded.
    pub loading: bool,
    /// The lobby being downloaded, built and installed.
    pub active: Option<String>,
    /// What the download is doing and the percent done.
    pub progress: Option<(String, u32)>,
    /// Download route: "auto" (or empty), "jsdelivr", "github", "custom".
    pub route: String,
    /// The custom route's prefix (put before the GitHub raw URL).
    pub custom_prefix: String,
    /// Install the pack on the watch once built (on by default).
    pub auto_install: bool,
}

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub root: Option<String>,
    pub tab: Tab,
    pub devices: Vec<DeviceInfo>,
    pub selected_addr: String,
    /// The module's `HELLO` answer for the selected watch.
    pub hello: Option<Hello>,
    pub packs: Vec<PackInfo>,
    /// The watch's latest measurements (refreshed while the watch tab is open).
    pub stats: Option<Stats>,
    pub assets: Vec<Asset>,
    pub name: String,
    /// Bumped when the name is set by the plugin, so the name field shows it.
    pub name_rev: u32,
    /// The student of the lobby in `assets`, when it came from the online library.
    pub character: Option<u32>,
    /// Speech bubble language: "zh-hans" (or empty), "zh-hant", "ja", "en", "off".
    pub bubble_lang: String,
    pub lib: LibraryState,
    /// Framing zoom in percent.
    pub zoom: u32,
    pub high_quality: bool,
    /// Baked idle: "off", "fine", "balanced" (default) or "fast" (grouping tolerance).
    pub idle_mode: String,
    pub built: Option<Built>,
    pub busy: bool,
    /// While compiling: what it is doing and the percent done.
    pub progress: Option<(String, u32)>,
    pub transfer: Option<Transfer>,
    /// The framing editor: the scene overview, a camera per phase, the phase shown, and a drag
    /// on the box (resizing, last pointer position in CSS pixels).
    pub overview: Option<crate::framing::Overview>,
    pub framing: Option<crate::framing::Framing>,
    pub frame_phase: u8,
    pub frame_drag: Option<(bool, f32, f32)>,
    pub status: String,
    pub error: bool,
    pub face: FaceState,
}

static STATE: OnceLock<Mutex<UiState>> = OnceLock::new();

fn state() -> &'static Mutex<UiState> {
    STATE.get_or_init(|| Mutex::new(UiState { zoom: 100, lib: LibraryState { auto_install: true, ..Default::default() }, ..Default::default() }))
}

pub fn with_state<T>(f: impl FnOnce(&mut UiState) -> T) -> T {
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

pub fn snapshot() -> UiState {
    with_state(|s| s.clone())
}

pub fn set_status(text: impl Into<String>, error: bool) {
    let text = text.into();
    with_state(|s| {
        s.status = text;
        s.error = error;
    });
}

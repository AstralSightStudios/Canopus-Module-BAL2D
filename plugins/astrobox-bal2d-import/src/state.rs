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
    /// Framing zoom in percent.
    pub zoom: u32,
    pub high_quality: bool,
    pub built: Option<Built>,
    pub busy: bool,
    /// While compiling: what it is doing and the percent done.
    pub progress: Option<(String, u32)>,
    pub transfer: Option<Transfer>,
    pub status: String,
    pub error: bool,
    pub face: FaceState,
}

static STATE: OnceLock<Mutex<UiState>> = OnceLock::new();

fn state() -> &'static Mutex<UiState> {
    STATE.get_or_init(|| Mutex::new(UiState { zoom: 100, ..Default::default() }))
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

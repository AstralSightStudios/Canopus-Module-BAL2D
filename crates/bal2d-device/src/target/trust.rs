//! Which GPU features this watch draws without the driver timing out, found on the watch.
//!
//! The real watch's GPU hung (the driver's 100 ms timeout, a GPU reset, parts of frames lost,
//! the system frozen after a few) once features beyond the oldest drawing were on, while every
//! self-test probe passed and Mirage showed nothing. So frames start with the plainest drawing
//! (32-bit pages, transparent pattern edges, a driver call per draw, the background drawn as its
//! slots) and the features are tried one at a time once frames have run clean for a while: a
//! timeout during a trial marks that feature bad and turns it off at once, a clean trial marks it
//! good. The verdicts are kept in a file, and the feature under trial is written there before it
//! is turned on, so a watch that freezes and restarts during a trial counts it as bad.

use super::host::Fs;

/// Native page formats (ARGB8565, ARGB4444, INDEX8 drawn as they are, not widened to BGRA8888).
pub const NATIVE: u8 = 1;
/// Batched draws (`gpu::Stream`).
pub const STREAM: u8 = 2;
/// The baked background drawn with one blit.
pub const BAKE: u8 = 4;
/// PAD pattern edges.
pub const PAD: u8 = 8;
pub const ALL: u8 = NATIVE | STREAM | BAKE | PAD;
/// The order of the trials: what saves the most first.
const ORDER: [u8; 4] = [NATIVE, STREAM, BAKE, PAD];
const FILE: &str = "gpu-trust.bin";
/// Bumped when the drawing changes enough that old verdicts mean nothing.
const VERSION: u8 = 1;
/// Frames drawn without a timeout before a trial starts, and for one to pass.
pub const CLEAN_FRAMES: u32 = 120;

#[derive(Clone, Copy, Default, Debug)]
pub struct Trust {
    pub good: u8,
    pub bad: u8,
    /// The feature under trial (0: none).
    pub trying: u8,
}

impl Trust {
    /// The saved verdicts; a trial still open in the file ended with the watch restarting.
    pub fn load() -> Trust {
        let mut t = Trust::default();
        if let Some(b) = Fs::read(FILE, 8) {
            if b.len() >= 4 && b[0] == VERSION {
                t = Trust { good: b[1] & ALL, bad: b[2] & ALL, trying: b[3] & ALL };
            }
        }
        if t.trying != 0 {
            super::trace(&alloc::format!("gpu trust: the watch restarted while trying {}: it counts as bad", names(t.trying)));
            t.bad |= t.trying;
            t.good &= !t.trying;
            t.trying = 0;
            t.save();
        }
        t
    }

    pub fn save(&self) {
        Fs::write(FILE, &[VERSION, self.good, self.bad, self.trying]);
    }

    /// The next feature to try (neither good nor bad yet).
    pub fn next(&self) -> Option<u8> {
        ORDER.into_iter().find(|f| (self.good | self.bad) & f == 0)
    }
}

/// Feature names for the log and the status page.
pub fn names(mask: u8) -> alloc::string::String {
    let mut t = alloc::string::String::new();
    for (bit, name) in [(NATIVE, "原格式贴图"), (STREAM, "批量绘制"), (BAKE, "背景烘焙"), (PAD, "边缘延伸")] {
        if mask & bit != 0 {
            if !t.is_empty() {
                t += "、";
            }
            t += name;
        }
    }
    if t.is_empty() {
        t += "无";
    }
    t
}

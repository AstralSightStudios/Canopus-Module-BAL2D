//! The GPU features the watch draws with.
//!
//! Early builds hung the real watch's GPU (the driver's 100 ms timeout, a GPU reset, parts of
//! frames lost) and the module tried each feature on the watch, one at a time, keeping the
//! verdicts in `gpu-trust.bin`. The hangs came from long triangle-soup paths of live layers
//! (drawn as outlines since), and every Band 10 Pro has the same GPU and firmware, so a release
//! turns every drawing feature on from the start (as far as the start-up self-test's read-backs
//! allow): no trial period of 32-bit pages and slower drawing, no per-watch state. Batched draws
//! alone are not verified on the watch in their current form (the first form hung it): they go
//! off for the rest of a boot after [`STREAM_RESETS`] timeouts.

/// Native page formats (ARGB8565, ARGB4444, INDEX8 drawn as they are, not widened to BGRA8888).
pub const NATIVE: u8 = 1;
/// Batched draws (`gpu::Stream`).
pub const STREAM: u8 = 2;
/// The baked background drawn with one blit.
pub const BAKE: u8 = 4;
/// PAD pattern edges.
pub const PAD: u8 = 8;
/// GPU addresses through PSRAM's non-cached alias (`gpu::UNCACHED`): off (the watch's
/// measurement has it at half the speed for textures).
pub const UNCACHED: u8 = 16;
/// What a release draws with.
pub const ON: u8 = NATIVE | STREAM | BAKE | PAD;
/// The verdicts file of the builds that tried features one by one (removed at start).
pub const OLD_FILE: &str = "gpu-trust.bin";
/// Timeouts with batched draws on, in one boot, that turn them off until the next.
pub const STREAM_RESETS: u32 = 2;

/// Feature names for the log and the status page.
pub fn names(mask: u8) -> alloc::string::String {
    let mut t = alloc::string::String::new();
    for (bit, name) in [(NATIVE, "原格式贴图"), (STREAM, "批量绘制"), (BAKE, "背景烘焙"), (PAD, "边缘延伸"), (UNCACHED, "非缓存地址")] {
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

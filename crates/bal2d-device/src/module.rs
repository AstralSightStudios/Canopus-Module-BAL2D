//! Module descriptor and lifecycle callbacks.

use canopus_abi::*;
use canopus_runtime::{status_put_u32, status_writer_publish};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static LAST_ERROR: AtomicU32 = AtomicU32::new(0);
static CONTEXT: AtomicPtr<CanopusContext> = AtomicPtr::new(core::ptr::null_mut());

pub const MODULE_ID: &[u8] = b"ba_l2d";
pub const MODULE_VERSION: &[u8] = b"0.1.0";
const MAGIC: u32 = 0x4241_4C32; // "BAL2"

pub fn context() -> *const CanopusContext {
    CONTEXT.load(Ordering::Acquire)
}

pub fn set_error(code: i32) {
    LAST_ERROR.store(code as u32, Ordering::Release);
}

const fn pack<const N: usize>(value: &[u8]) -> [u8; N] {
    let mut output = [0; N];
    let mut index = 0;
    while index < value.len() && index < N {
        output[index] = value[index];
        index += 1;
    }
    output
}

#[cfg(feature = "device")]
const TARGET_ID: &[u8] = canopus_target_private::TARGET_ID.as_bytes();
#[cfg(not(feature = "device"))]
const TARGET_ID: &[u8] = b"host-test";

#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_prepare(ctx: *const ContextV1) -> i32 {
    if !ctx.is_null() {
        CONTEXT.store(ctx as *mut CanopusContext, Ordering::Release);
    }
    0
}

/// Runs on the loader's small stack: only records the context. The player starts on the UI
/// owner when the Supervisor publishes the native app (stage 2).
#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_activate(ctx: *const ContextV1) -> i32 {
    if !ctx.is_null() {
        CONTEXT.store(ctx as *mut CanopusContext, Ordering::Release);
    }
    #[cfg(feature = "device")]
    {
        let guard = canopus_target_private::canopus_identity_guard();
        if guard != 0 {
            set_error(guard);
            return guard;
        }
    }
    ACTIVE.store(true, Ordering::Release);
    0
}

/// LVGL objects and GPU buffers live in this image: unloading needs a reboot.
#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_deactivate(_ctx: *const ContextV1) -> i32 {
    if ACTIVE.load(Ordering::Acquire) { RESULT_REBOOT_REQUIRED as i32 } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_stop(ctx: *const ContextV1) -> i32 {
    canopus_mod_deactivate(ctx)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_query(writer: *mut StatusWriterV1) -> i32 {
    if writer.is_null() {
        return -1;
    }
    let writer = unsafe { &mut *writer };
    #[cfg(feature = "device")]
    let extra = crate::target::status();
    #[cfg(not(feature = "device"))]
    let extra = [0u32; 8];
    unsafe {
        let mut ok = status_put_u32(writer, MAGIC)
            && status_put_u32(writer, ACTIVE.load(Ordering::Acquire) as u32)
            && status_put_u32(writer, LAST_ERROR.load(Ordering::Acquire));
        for value in extra {
            ok = ok && status_put_u32(writer, value);
        }
        if !ok {
            return -1;
        }
    }
    status_writer_publish(writer);
    0
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_publish_native_app_stage(ctx: *const ContextV1, stage: u32) -> i32 {
    if !ctx.is_null() {
        CONTEXT.store(ctx as *mut CanopusContext, Ordering::Release);
    }
    #[cfg(feature = "device")]
    let result = unsafe { crate::target::app::publish_stage(ctx, stage) }.map(|()| 0).unwrap_or_else(|e| e);
    #[cfg(not(feature = "device"))]
    let result = if matches!(stage, 1 | 2) { 0 } else { -22 };
    if result != 0 {
        set_error(result);
    }
    result
}

#[unsafe(no_mangle)]
pub static canopus_module_descriptor: ModuleDescriptorV1 = ModuleDescriptorV1 {
    struct_size: core::mem::size_of::<ModuleDescriptorV1>() as u32,
    abi_major: ABI_MAJOR,
    abi_minor: ABI_MINOR,
    flags: FLAG_HAS_NATIVE_APP
        | FLAG_NATIVE_APP_STANDALONE
        | FLAG_REGISTERS_LAUNCHER_ENTRY
        | FLAG_REQUIRES_UI_DISPATCHER
        | FLAG_APP_UNREGISTER_REBOOT_REQUIRED,
    module_id: pack(MODULE_ID),
    module_version: pack(MODULE_VERSION),
    build_id: pack(b"ba-l2d-0.1.0"),
    target_id: pack(TARGET_ID),
    prepare: Some(canopus_mod_prepare),
    activate: Some(canopus_mod_activate),
    deactivate: Some(canopus_mod_deactivate),
    stop: Some(canopus_mod_stop),
    query: Some(canopus_mod_query),
    publish_native_app: None,
    publish_native_app_stage: Some(canopus_mod_publish_native_app_stage),
};

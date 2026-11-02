//! BAL2D on the watch. Everything runs on the LVGL/UI owner task: the frame timer, LVGL event
//! callbacks and the native page callbacks.
#![cfg_attr(feature = "device", no_std)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(feature = "device")]
extern crate alloc;

#[cfg(feature = "device")]
mod allocator {
    use core::alloc::{GlobalAlloc, Layout};

    /// The firmware heap (umm, cached PSRAM), with the original pointer stored in front of each
    /// aligned block. GPU buffers ask for 64-byte alignment.
    pub struct FirmwareAllocator;

    /// Requests from this size up are checked against the heap first.
    const LARGE: usize = 32 * 1024;

    unsafe impl GlobalAlloc for FirmwareAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let header = core::mem::size_of::<usize>();
            let total = match layout.size().checked_add(layout.align()).and_then(|s| s.checked_add(header)) {
                Some(total) if total <= u32::MAX as usize => total,
                _ => return core::ptr::null_mut(),
            };
            // The firmware's malloc asserts (resets the watch) instead of returning NULL: refuse
            // large requests the heap cannot serve while leaving the system its working room.
            if total >= LARGE && !crate::target::heap_allows(total) {
                return core::ptr::null_mut();
            }
            let raw = unsafe { canopus_target_private::bt_alloc(total as u32) }.cast::<u8>();
            if raw.is_null() {
                return raw;
            }
            let start = unsafe { raw.add(header) } as usize;
            let aligned = (start + layout.align() - 1) & !(layout.align() - 1);
            let output = aligned as *mut u8;
            unsafe { output.sub(header).cast::<usize>().write(raw as usize) };
            output
        }

        unsafe fn dealloc(&self, pointer: *mut u8, _layout: Layout) {
            if pointer.is_null() {
                return;
            }
            let header = core::mem::size_of::<usize>();
            let raw = unsafe { pointer.sub(header).cast::<usize>().read() } as *mut core::ffi::c_void;
            unsafe { canopus_target_private::bt_free(raw) };
        }
    }
}

#[cfg(feature = "device")]
#[global_allocator]
static ALLOCATOR: allocator::FirmwareAllocator = allocator::FirmwareAllocator;

mod module;

#[cfg(feature = "device")]
mod target;

pub use module::*;

/// Records the panic in the module's log before parking the task (the watchdog then resets
/// the device and the Supervisor marks the module unstable).
#[cfg(all(feature = "device", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::sync::atomic::{AtomicBool, Ordering};
    static ONCE: AtomicBool = AtomicBool::new(false);
    if !ONCE.swap(true, Ordering::AcqRel) {
        let mut line = alloc::string::String::new();
        let _ = core::fmt::write(&mut line, format_args!("PANIC {info}"));
        target::trace(&line);
    }
    loop {}
}

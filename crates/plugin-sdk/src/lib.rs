//! Authoring SDK for AgenticArt filter plugins.
//!
//! A plugin is a `cdylib` crate compiled to `wasm32-unknown-unknown` that
//! links this crate and calls [`export_filter!`] once with a function of
//! signature `fn(&mut [u8], u32, u32)` (RGBA8 pixel bytes, width, height,
//! mutated in place). The host (`agenticart_core::plugin`) loads the
//! resulting `.wasm` module in a sandbox with no imports beyond linear
//! memory - a plugin cannot touch the filesystem, network, or anything
//! outside the pixel buffer it's handed.
//!
//! This crate supplies the `alloc`/`dealloc`/`filter` exports the host's
//! ABI expects, so plugin authors only write the pixel-processing function
//! itself.

use std::alloc::{alloc, dealloc, Layout};

/// Allocates `len` bytes in the plugin's linear memory and returns the
/// pointer, for the host to write the input pixel buffer into.
#[no_mangle]
pub extern "C" fn alloc_buffer(len: u32) -> u32 {
    let layout = Layout::array::<u8>(len as usize).expect("plugin buffer layout");
    unsafe { alloc(layout) as u32 }
}

/// Frees a buffer previously returned by `alloc_buffer` (or by a filter
/// function, since filters are expected to mutate and return the same
/// pointer they were given).
#[no_mangle]
pub extern "C" fn dealloc_buffer(ptr: u32, len: u32) {
    let layout = Layout::array::<u8>(len as usize).expect("plugin buffer layout");
    unsafe { dealloc(ptr as *mut u8, layout) }
}

/// Defines the plugin's `filter` export from a plain Rust function.
///
/// ```ignore
/// fn invert(pixels: &mut [u8], _width: u32, _height: u32) {
///     for p in pixels.chunks_exact_mut(4) {
///         p[0] = 255 - p[0];
///         p[1] = 255 - p[1];
///         p[2] = 255 - p[2];
///     }
/// }
/// agenticart_plugin_sdk::export_filter!(invert);
/// ```
#[macro_export]
macro_rules! export_filter {
    ($f:path) => {
        #[no_mangle]
        pub extern "C" fn filter(ptr: u32, len: u32, width: u32, height: u32) -> u32 {
            let slice = unsafe { std::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) };
            $f(slice, width, height);
            ptr
        }
    };
}

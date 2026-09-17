//! Sandboxed WASM plugin host for filter plugins built against
//! `agenticart-plugin-sdk` (see that crate's docs for the plugin-side ABI).
//!
//! Plugins run with **no host imports at all** - not even WASI - so a
//! loaded module can only read/write its own linear memory. It cannot open
//! files, make network calls, or call back into the host in any way. A
//! fuel budget additionally bounds how much work a single `run_filter`
//! call may do, so a plugin bug (or a malicious plugin) can't hang the
//! editor with an infinite loop.

use anyhow::Context;
use anyhow::Result;

/// Converts a `wasmtime::Error` result into an `anyhow::Result`, since
/// `wasmtime::Error` no longer implements `std::error::Error` and so can't
/// use `anyhow::Context` directly.
fn wctx<T>(r: std::result::Result<T, wasmtime::Error>, msg: &str) -> Result<T> {
    r.map_err(|e| anyhow::anyhow!("{msg}: {e}"))
}
use image::RgbaImage;
use wasmtime::{Config, Engine, Instance, Memory, Module, Store, TypedFunc};

const FUEL_BUDGET: u64 = 10_000_000_000;

pub struct PluginHost {
    engine: Engine,
}

impl PluginHost {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = wctx(Engine::new(&config), "failed to initialize wasm engine")?;
        Ok(Self { engine })
    }

    /// Compiles and instantiates a plugin from raw `.wasm` bytes. Fails if
    /// the module imports anything (it isn't allowed to - the sandbox
    /// provides no host functions) or doesn't export the ABI functions
    /// `agenticart_plugin_sdk::export_filter!` generates.
    pub fn load(&self, wasm_bytes: &[u8]) -> Result<LoadedPlugin> {
        let module = wctx(Module::new(&self.engine, wasm_bytes), "invalid wasm plugin module")?;
        let mut store = Store::new(&self.engine, ());
        wctx(store.set_fuel(FUEL_BUDGET), "setting fuel budget")?;
        let instance = wctx(
            Instance::new(&mut store, &module, &[]),
            "plugin failed to instantiate (it must import nothing but its own memory)",
        )?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| anyhow::anyhow!("plugin does not export linear memory"))?;
        let alloc: TypedFunc<u32, u32> = wctx(
            instance.get_typed_func(&mut store, "alloc_buffer"),
            "plugin does not export alloc_buffer(len) -> ptr",
        )?;
        let dealloc: TypedFunc<(u32, u32), ()> = wctx(
            instance.get_typed_func(&mut store, "dealloc_buffer"),
            "plugin does not export dealloc_buffer(ptr, len)",
        )?;
        let filter: TypedFunc<(u32, u32, u32, u32), u32> = wctx(
            instance.get_typed_func(&mut store, "filter"),
            "plugin does not export filter(ptr, len, width, height) -> ptr",
        )?;

        Ok(LoadedPlugin { store, memory, alloc, dealloc, filter })
    }
}

pub struct LoadedPlugin {
    store: Store<()>,
    memory: Memory,
    alloc: TypedFunc<u32, u32>,
    dealloc: TypedFunc<(u32, u32), ()>,
    filter: TypedFunc<(u32, u32, u32, u32), u32>,
}

impl LoadedPlugin {
    /// Runs the plugin's filter over `image` in place. Refuels the sandbox
    /// to the standard budget before each call, so one runaway invocation
    /// can't exhaust the budget for the next.
    pub fn run_filter(&mut self, image: &mut RgbaImage) -> Result<()> {
        wctx(self.store.set_fuel(FUEL_BUDGET), "setting fuel budget")?;

        let (width, height) = image.dimensions();
        let bytes = image.as_raw();
        let len = bytes.len() as u32;

        let in_ptr = wctx(self.alloc.call(&mut self.store, len), "plugin alloc_buffer failed")?;
        self.memory
            .write(&mut self.store, in_ptr as usize, bytes)
            .context("writing pixels into plugin memory")?;

        let out_ptr = wctx(
            self.filter.call(&mut self.store, (in_ptr, len, width, height)),
            "plugin filter() call failed (ran out of fuel, or trapped)",
        )?;

        let mut out = vec![0u8; len as usize];
        self.memory
            .read(&self.store, out_ptr as usize, &mut out)
            .context("reading pixels back from plugin memory")?;

        wctx(self.dealloc.call(&mut self.store, (out_ptr, len)), "plugin dealloc_buffer failed")?;

        *image = RgbaImage::from_raw(width, height, out)
            .ok_or_else(|| anyhow::anyhow!("plugin returned a buffer of the wrong size"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_example_plugin_wasm() -> Vec<u8> {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .expect("core crate is two levels under the workspace root");

        let status = std::process::Command::new(env!("CARGO"))
            .args([
                "build",
                "--target",
                "wasm32-unknown-unknown",
                "--release",
                "-p",
                "agenticart-example-plugin-invert",
            ])
            .current_dir(workspace_root)
            .status()
            .expect("failed to invoke cargo to build the example plugin");
        assert!(status.success(), "example plugin failed to build");

        let wasm_path = workspace_root
            .join("target/wasm32-unknown-unknown/release/agenticart_example_plugin_invert.wasm");
        std::fs::read(&wasm_path).unwrap_or_else(|e| panic!("reading {wasm_path:?}: {e}"))
    }

    #[test]
    fn sandboxed_plugin_inverts_pixels_in_place() {
        let wasm = build_example_plugin_wasm();
        let host = PluginHost::new().unwrap();
        let mut plugin = host.load(&wasm).unwrap();

        let mut image = RgbaImage::from_raw(2, 1, vec![10, 20, 30, 255, 0, 0, 0, 128]).unwrap();
        plugin.run_filter(&mut image).unwrap();

        assert_eq!(image.get_pixel(0, 0).0, [245, 235, 225, 255], "RGB inverted, alpha untouched");
        assert_eq!(image.get_pixel(1, 0).0, [255, 255, 255, 128], "RGB inverted, alpha untouched");
    }

    #[test]
    fn plugin_missing_required_exports_is_rejected_not_panicked() {
        // A minimal, deliberately non-conforming module: exports memory but
        // none of the ABI functions the host requires.
        let wat = r#"(module (memory (export "memory") 1))"#;
        let wasm = wat::parse_str(wat).unwrap();

        let host = PluginHost::new().unwrap();
        let result = host.load(&wasm);
        assert!(result.is_err(), "module without the plugin ABI must be rejected");
        let err = result.err().unwrap();
        assert!(err.to_string().contains("alloc_buffer"), "error should name the missing export: {err}");
    }
}

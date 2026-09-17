use agenticart_core::{Document, DocumentStore};
use serde_json::json;

fn build_example_plugin_wasm() -> std::path::PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = std::path::Path::new(manifest_dir).parent().and_then(|p| p.parent()).expect("mcp-server crate is two levels under the workspace root");

    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "--target", "wasm32-unknown-unknown", "--release", "-p", "agenticart-example-plugin-invert"])
        .current_dir(workspace_root)
        .status()
        .expect("failed to invoke cargo to build the example plugin");
    assert!(status.success(), "example plugin failed to build");

    workspace_root.join("target/wasm32-unknown-unknown/release/agenticart_example_plugin_invert.wasm")
}

#[test]
fn plugin_run_filter_tool_inverts_a_layers_pixels_through_the_mcp_dispatcher() {
    let wasm_path = build_example_plugin_wasm();

    let store = DocumentStore::new();
    let doc = Document::new("Plugin Test", 2, 1);
    let doc_id = doc.id;
    let layer_id = doc.layers[0].id;
    store.insert(doc);

    store
        .mutate(doc_id, |doc| {
            let layer = doc.find_layer_mut(layer_id).unwrap();
            layer.pixels = image::RgbaImage::from_raw(2, 1, vec![10, 20, 30, 255, 0, 0, 0, 128]).unwrap();
        })
        .unwrap();

    let args = json!({
        "documentId": doc_id.to_string(),
        "layerId": layer_id.to_string(),
        "pluginPath": wasm_path.to_str().unwrap(),
    });
    let result = agenticart_mcp_server::tools::call(&store, "plugin.runFilter", &args).expect("plugin.runFilter should succeed");
    assert_eq!(result, json!({"ok": true}));

    let doc = store.get_clone(doc_id).unwrap();
    let layer = doc.layers.iter().find(|l| l.id == layer_id).unwrap();
    assert_eq!(layer.pixels.get_pixel(0, 0).0, [245, 235, 225, 255], "RGB inverted, alpha untouched, applied through the MCP tool exactly as a human-driven filter would be");
    assert_eq!(layer.pixels.get_pixel(1, 0).0, [255, 255, 255, 128]);
}

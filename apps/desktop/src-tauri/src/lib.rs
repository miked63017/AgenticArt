mod commands;

use agenticart_core::DocumentStore;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Shared state for the embedded MCP server: the live token (mutable, so a
/// "Regenerate" action in Settings can rotate it without restarting the
/// server - see `http::AppState::write_token`'s doc comment) plus enough
/// info for `commands::get_mcp_info`/`regenerate_mcp_token` to describe the
/// current connection to the UI.
pub struct McpState {
    pub port: u16,
    pub token: Arc<RwLock<String>>,
    pub config_path: PathBuf,
}

/// Where the persisted token lives: the OS's per-user config directory
/// (`%APPDATA%\AgenticArt` on Windows), not next to the executable - the
/// executable gets overwritten by every rebuild, but this needs to survive
/// that so the token stays stable across app updates, not just relaunches.
fn config_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(std::env::temp_dir);
    base.join("AgenticArt").join("mcp-config.json")
}

/// Loads the persisted token, generating and persisting a fresh one on
/// first run. A stable, disk-backed token (vs. a fresh UUID every launch)
/// means an agent's MCP client config doesn't need re-editing every time
/// the app restarts.
fn load_or_create_token(path: &PathBuf) -> String {
    if let Ok(bytes) = std::fs::read(path) {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if let Some(token) = value.get("token").and_then(|t| t.as_str()) {
                return token.to_string();
            }
        }
    }
    let token = uuid::Uuid::new_v4().to_string();
    persist_token(path, &token);
    token
}

pub fn persist_token(path: &PathBuf, token: &str) {
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("agenticart desktop: failed to create config dir {}: {e}", dir.display());
            return;
        }
    }
    let contents = serde_json::json!({ "token": token }).to_string();
    if let Err(e) = std::fs::write(path, contents) {
        eprintln!("agenticart desktop: failed to persist mcp token to {}: {e}", path.display());
    }
}

/// Starts the same HTTP MCP transport `agenticart-mcp-server` exposes as a
/// standalone binary, but sharing *this* process's `DocumentStore` (cloned,
/// not a fresh one) - so an agent connecting to it sees and edits the exact
/// same live document(s) the desktop UI has open - one live document
/// engine shared by every client, agent or human. Running the standalone
/// server binary alongside the app instead would give the agent its own
/// empty, isolated store.
fn spawn_embedded_mcp_server(store: DocumentStore) -> McpState {
    const PORT: u16 = 9223;
    let path = config_path();
    let token = Arc::new(RwLock::new(load_or_create_token(&path)));

    // Also drop a session snapshot next to the executable - a convenience
    // for a human/agent poking around the install dir who doesn't know
    // about the per-user config location.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let info = serde_json::json!({
                "port": PORT,
                "token": *token.read().unwrap(),
                "url": format!("http://127.0.0.1:{PORT}/mcp"),
            });
            let _ = std::fs::write(dir.join("mcp-session.json"), info.to_string());
        }
    }
    eprintln!("agenticart desktop: embedded MCP server on http://127.0.0.1:{PORT}/mcp (see Settings for the current token)");

    let server_token = token.clone();
    std::thread::spawn(move || {
        if let Err(e) = agenticart_mcp_server::http::serve(store, PORT, server_token, None, vec![]) {
            eprintln!("agenticart desktop: embedded MCP server failed: {e}");
        }
    });

    McpState { port: PORT, token, config_path: path }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = DocumentStore::new();
    let mcp_state = spawn_embedded_mcp_server(store.clone());
    let store_for_events = store.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // Pushes a "document-changed" event to every window the instant
            // *any* mutation lands - whether it came from an MCP tool call
            // or this window's own UI - so the canvas (and the last-used-
            // color hint) can update live instead of the frontend having to
            // poll and hope it lands within some interval.
            use tauri::Emitter;
            let handle = app.handle().clone();
            store_for_events.on_change(move |id| {
                let _ = handle.emit("document-changed", id.to_string());
            });
            Ok(())
        })
        .manage(store)
        .manage(mcp_state)
        .invoke_handler(tauri::generate_handler![
            commands::get_mcp_info,
            commands::regenerate_mcp_token,
            commands::create_document,
            commands::list_documents,
            commands::get_document,
            commands::close_document,
            commands::take_focus_request,
            commands::get_last_color,
            commands::resize_document,
            commands::resize_canvas,
            commands::create_layer,
            commands::create_group,
            commands::move_into_group,
            commands::ungroup_layer,
            commands::duplicate_layer,
            commands::delete_layer,
            commands::reorder_layer,
            commands::paint_stroke,
            commands::erase_stroke,
            commands::set_layer_properties,
            commands::fill_rect,
            commands::fill_ellipse,
            commands::fill_path,
            commands::stroke_path_shape,
            commands::convert_color_profile,
            commands::cmyk_soft_proof,
            commands::export_high_bit_depth,
            commands::cutout_subject,
            commands::select_subject,
            commands::set_drop_shadow,
            commands::set_stroke,
            commands::set_selection_rect,
            commands::clear_selection,
            commands::apply_brightness_contrast,
            commands::apply_hue_saturation,
            commands::apply_gaussian_blur,
            commands::apply_sharpen,
            commands::draw_text,
            commands::save_project,
            commands::open_project,
            commands::open_psd,
            commands::export_psd,
            commands::open_document_from_file,
            commands::place_image,
            commands::render_canvas,
            commands::export_document,
            commands::undo,
            commands::redo,
            commands::transform_layer,
            commands::merge_down,
            commands::flatten_image,
            commands::set_outer_glow,
            commands::fill_gradient,
            commands::paint_bucket,
            commands::clone_stamp,
            commands::dodge_burn,
            commands::create_layer_comp,
            commands::apply_layer_comp,
            commands::delete_layer_comp,
            commands::list_layer_comps,
            commands::add_smart_filter,
            commands::remove_smart_filter,
            commands::mask_from_selection,
            commands::clear_mask,
            commands::create_text_layer,
            commands::set_text_content,
            commands::content_aware_fill,
            commands::ml_inpaint,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

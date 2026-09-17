use crate::McpState;
use agenticart_core::document::{BlendMode, SelectionRect};
use agenticart_core::{Brush, BrushPoint, Document, DocumentStore};
use base64::Engine;
use image::Rgba;
use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|_| "invalid id".to_string())
}

#[derive(Serialize)]
pub struct McpInfo {
    pub port: u16,
    pub token: String,
    pub url: String,
}

fn describe_mcp(state: &McpState) -> McpInfo {
    McpInfo { port: state.port, token: state.token.read().unwrap().clone(), url: format!("http://127.0.0.1:{}/mcp", state.port) }
}

#[tauri::command]
pub fn get_mcp_info(state: State<McpState>) -> McpInfo {
    describe_mcp(&state)
}

/// Rotates the live MCP bearer token: generates a new one, persists it (so
/// it survives the next app restart), and swaps it into the running HTTP
/// server via its shared `Arc<RwLock<String>>` - no server restart, and
/// any client still using the old token is rejected on its very next call.
#[tauri::command]
pub fn regenerate_mcp_token(state: State<McpState>) -> McpInfo {
    let new_token = Uuid::new_v4().to_string();
    crate::persist_token(&state.config_path, &new_token);
    *state.token.write().unwrap() = new_token;
    describe_mcp(&state)
}

#[derive(Serialize)]
pub struct LayerInfo {
    pub id: String,
    pub name: String,
    pub opacity: f32,
    pub visible: bool,
    pub blend_mode: String,
    pub clip_to_below: bool,
    pub has_drop_shadow: bool,
    pub has_stroke: bool,
    pub has_outer_glow: bool,
    pub smart_filter_count: usize,
    pub has_mask: bool,
    pub has_text: bool,
    pub is_group: bool,
    pub parent_group: Option<String>,
}

#[derive(Serialize)]
pub struct DocumentInfo {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub layers: Vec<LayerInfo>,
}

fn describe(doc: &Document) -> DocumentInfo {
    DocumentInfo {
        id: doc.id.to_string(),
        name: doc.name.clone(),
        width: doc.width,
        height: doc.height,
        layers: doc
            .layers
            .iter()
            .map(|l| LayerInfo {
                id: l.id.to_string(),
                name: l.name.clone(),
                opacity: l.opacity,
                visible: l.visible,
                blend_mode: l.blend_mode.as_str().to_string(),
                clip_to_below: l.clip_to_below,
                has_drop_shadow: l.style.drop_shadow.is_some(),
                has_stroke: l.style.stroke.is_some(),
                has_outer_glow: l.style.outer_glow.is_some(),
                smart_filter_count: l.smart_filters.len(),
                has_mask: l.mask.is_some(),
                has_text: l.text.is_some(),
                is_group: l.is_group,
                parent_group: l.parent_group.map(|g| g.to_string()),
            })
            .collect(),
    }
}

#[tauri::command]
pub fn create_document(store: State<DocumentStore>, name: String, width: u32, height: u32) -> Result<DocumentInfo, String> {
    let doc = Document::new(name, width, height);
    let info = describe(&doc);
    store.insert(doc);
    Ok(info)
}

#[derive(Serialize)]
pub struct DocumentSummary {
    pub id: String,
    pub name: String,
}

/// Every document currently open in this process's `DocumentStore` - not
/// just the one this window happens to be displaying. Lets the UI show a
/// tab for a document an MCP agent created directly against the same
/// embedded server, without the UI needing any push/event channel from
/// the agent side.
#[tauri::command]
pub fn list_documents(store: State<DocumentStore>) -> Vec<DocumentSummary> {
    store.list().into_iter().map(|(id, name)| DocumentSummary { id: id.to_string(), name }).collect()
}

#[tauri::command]
pub fn get_document(store: State<DocumentStore>, document_id: String) -> Result<DocumentInfo, String> {
    let id = parse_uuid(&document_id)?;
    store.get_clone(id).map(|d| describe(&d)).ok_or_else(|| "document not found".into())
}

#[tauri::command]
pub fn close_document(store: State<DocumentStore>, document_id: String) -> Result<bool, String> {
    let id = parse_uuid(&document_id)?;
    Ok(store.close(id))
}

/// Polled by the UI's tab bar alongside `list_documents` - drains a
/// pending `document.focus` MCP call (see `DocumentStore::take_focus_request`)
/// so an agent can bring a specific tab to the front without the store
/// knowing anything about tabs or windows itself.
#[tauri::command]
pub fn take_focus_request(store: State<DocumentStore>) -> Option<String> {
    store.take_focus_request().map(|id| id.to_string())
}

/// The color most recently used by a paint/fill MCP call against this
/// document (see `DocumentStore::last_color`'s doc comment) - read by the
/// UI's "document-changed" event handler so the color swatch can mirror
/// what an agent just painted with.
#[tauri::command]
pub fn get_last_color(store: State<DocumentStore>, document_id: String) -> Result<Option<[u8; 4]>, String> {
    let id = parse_uuid(&document_id)?;
    Ok(store.get_last_color(id))
}

#[tauri::command]
pub fn resize_document(store: State<DocumentStore>, document_id: String, width: u32, height: u32) -> Result<DocumentInfo, String> {
    let doc_id = parse_uuid(&document_id)?;
    store
        .mutate(doc_id, move |doc| {
            agenticart_core::resize::resize_document(doc, width, height);
            describe(doc)
        })
        .ok_or("document not found".into())
}

#[tauri::command]
pub fn resize_canvas(
    store: State<DocumentStore>,
    document_id: String,
    width: u32,
    height: u32,
    offset_x: i64,
    offset_y: i64,
) -> Result<DocumentInfo, String> {
    let doc_id = parse_uuid(&document_id)?;
    store
        .mutate(doc_id, move |doc| {
            agenticart_core::resize::resize_canvas(doc, width, height, offset_x, offset_y);
            describe(doc)
        })
        .ok_or("document not found".into())
}

#[tauri::command]
pub fn create_group(store: State<DocumentStore>, document_id: String, name: String) -> Result<String, String> {
    let doc_id = parse_uuid(&document_id)?;
    store.mutate(doc_id, move |doc| doc.create_group(name).to_string()).ok_or("document not found".into())
}

#[tauri::command]
pub fn move_into_group(store: State<DocumentStore>, document_id: String, layer_id: String, group_id: String) -> Result<bool, String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let group_id = parse_uuid(&group_id)?;
    store.mutate(doc_id, move |doc| doc.move_layer_into_group(layer_id, group_id)).ok_or("document not found".into())
}

#[tauri::command]
pub fn ungroup_layer(store: State<DocumentStore>, document_id: String, layer_id: String) -> Result<bool, String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store.mutate(doc_id, move |doc| doc.ungroup_layer(layer_id)).ok_or("document not found".into())
}

#[tauri::command]
pub fn create_layer(store: State<DocumentStore>, document_id: String, name: String) -> Result<String, String> {
    let id = parse_uuid(&document_id)?;
    store
        .mutate(id, |doc| {
            let layer = agenticart_core::Layer::new_transparent(name, doc.width, doc.height);
            let layer_id = layer.id;
            doc.layers.push(layer);
            doc.active_layer = doc.layers.len() - 1;
            layer_id
        })
        .map(|id| id.to_string())
        .ok_or_else(|| "document not found".into())
}

#[tauri::command]
pub fn duplicate_layer(store: State<DocumentStore>, document_id: String, layer_id: String) -> Result<String, String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| doc.duplicate_layer(layer_id))
        .ok_or("document not found")?
        .map(|id| id.to_string())
        .ok_or_else(|| "layer not found".into())
}

#[tauri::command]
pub fn delete_layer(store: State<DocumentStore>, document_id: String, layer_id: String) -> Result<bool, String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store.mutate(doc_id, move |doc| doc.delete_layer(layer_id)).ok_or("document not found".into())
}

#[tauri::command]
pub fn reorder_layer(store: State<DocumentStore>, document_id: String, layer_id: String, new_index: usize) -> Result<bool, String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store.mutate(doc_id, move |doc| doc.reorder_layer(layer_id, new_index)).ok_or("document not found".into())
}

#[derive(Deserialize)]
pub struct StrokePointInput {
    pub x: f32,
    pub y: f32,
    pub pressure: Option<f32>,
}

#[derive(Deserialize)]
pub struct BrushInput {
    pub size: f32,
    pub color: [u8; 4],
    pub hardness: f32,
}

/// Runs `f` through `mutate` (a fresh undo snapshot) or `mutate_continue`
/// (extends the current gesture without a new snapshot) depending on
/// `continue_stroke` - see `DocumentStore::mutate_continue`'s doc comment.
/// The UI passes `continue_stroke: true` for every pointer-move segment
/// after a stroke's first (pointer-down) segment, so a whole freehand
/// drag undoes as one step instead of one step per segment.
fn mutate_gesture<F, R>(store: &DocumentStore, id: Uuid, continue_stroke: Option<bool>, f: F) -> Option<R>
where
    F: FnOnce(&mut Document) -> R,
{
    if continue_stroke.unwrap_or(false) {
        store.mutate_continue(id, f)
    } else {
        store.mutate(id, f)
    }
}

#[tauri::command]
pub fn paint_stroke(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    points: Vec<StrokePointInput>,
    brush: BrushInput,
    continue_stroke: Option<bool>,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let brush_points: Vec<BrushPoint> = points
        .into_iter()
        .map(|p| BrushPoint {
            x: p.x,
            y: p.y,
            pressure: p.pressure.unwrap_or(1.0),
        })
        .collect();
    let brush = Brush {
        size: brush.size,
        color: Rgba(brush.color),
        hardness: brush.hardness,
    };
    mutate_gesture(&store, doc_id, continue_stroke, move |doc| -> Result<(), String> {
        let clip = doc.selection;
        let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
        agenticart_core::paint::stroke_path(layer, &brush_points, &brush, clip);
        Ok(())
    })
    .ok_or("document not found")?
}

#[tauri::command]
pub fn erase_stroke(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    points: Vec<StrokePointInput>,
    brush: BrushInput,
    continue_stroke: Option<bool>,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let brush_points: Vec<BrushPoint> = points
        .into_iter()
        .map(|p| BrushPoint {
            x: p.x,
            y: p.y,
            pressure: p.pressure.unwrap_or(1.0),
        })
        .collect();
    let brush = Brush {
        size: brush.size,
        color: Rgba(brush.color),
        hardness: brush.hardness,
    };
    mutate_gesture(&store, doc_id, continue_stroke, move |doc| -> Result<(), String> {
        let clip = doc.selection;
        let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
        agenticart_core::paint::erase_path(layer, &brush_points, &brush, clip);
        Ok(())
    })
    .ok_or("document not found")?
}

#[tauri::command]
pub fn set_layer_properties(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    opacity: Option<f32>,
    visible: Option<bool>,
    blend_mode: Option<String>,
    clip_to_below: Option<bool>,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let blend = match blend_mode.as_deref() {
        Some(s) => Some(BlendMode::parse(s).ok_or_else(|| format!("unknown blend mode '{s}'"))?),
        None => None,
    };
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            if let Some(o) = opacity {
                layer.opacity = o;
            }
            if let Some(v) = visible {
                layer.visible = v;
            }
            if let Some(bm) = blend {
                layer.blend_mode = bm;
            }
            if let Some(c) = clip_to_below {
                layer.clip_to_below = c;
            }
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn set_selection_rect(store: State<DocumentStore>, document_id: String, x: i64, y: i64, width: u32, height: u32) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    store.mutate(doc_id, move |doc| doc.selection = Some(SelectionRect { x, y, width, height })).ok_or("document not found".into())
}

#[tauri::command]
pub fn clear_selection(store: State<DocumentStore>, document_id: String) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    store.mutate(doc_id, |doc| doc.selection = None).ok_or("document not found".into())
}

#[tauri::command]
pub fn apply_brightness_contrast(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    brightness: f32,
    contrast: f32,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let clip = doc.selection;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::adjustments::brightness_contrast(layer, brightness, contrast, clip);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn apply_hue_saturation(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    hue: f32,
    saturation: f32,
    lightness: f32,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let clip = doc.selection;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::adjustments::hue_saturation(layer, hue, saturation, lightness, clip);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn render_canvas(store: State<DocumentStore>, document_id: String) -> Result<String, String> {
    let id = parse_uuid(&document_id)?;
    let doc = store.get_clone(id).ok_or("document not found")?;
    let bytes = agenticart_core::io::render_to_bytes(&doc, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

#[tauri::command]
pub fn export_document(store: State<DocumentStore>, document_id: String, path: String) -> Result<(), String> {
    let id = parse_uuid(&document_id)?;
    let doc = store.get_clone(id).ok_or("document not found")?;
    agenticart_core::io::export_to_file(&doc, std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn draw_text(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    text: String,
    x: i32,
    y: i32,
    size: f32,
    color: [u8; 4],
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::text::draw_text(layer, &text, x, y, size, Rgba(color), None).map_err(|e| e.to_string())
        })
        .ok_or("document not found")?
}

#[derive(Deserialize)]
pub struct PathNodeInput {
    pub x: f32,
    pub y: f32,
}

fn build_path(nodes: Vec<PathNodeInput>, closed: bool) -> agenticart_core::path::Path {
    agenticart_core::path::Path {
        nodes: nodes
            .into_iter()
            .map(|n| agenticart_core::path::PathNode { anchor: agenticart_core::path::Point { x: n.x, y: n.y }, control_in: None, control_out: None })
            .collect(),
        closed,
    }
}

#[tauri::command]
pub fn fill_path(store: State<DocumentStore>, document_id: String, layer_id: String, nodes: Vec<PathNodeInput>, color: [u8; 4]) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let path = build_path(nodes, true);
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let clip = doc.selection;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::path::fill_path(layer, &path, Rgba(color), clip);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn stroke_path_shape(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    nodes: Vec<PathNodeInput>,
    brush_size: f32,
    color: [u8; 4],
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let path = build_path(nodes, false);
    let brush = Brush { size: brush_size, color: Rgba(color), hardness: 1.0 };
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let clip = doc.selection;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::path::stroke_path_shape(layer, &path, &brush, clip);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn fill_rect(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    x: i64,
    y: i64,
    width: u32,
    height: u32,
    color: [u8; 4],
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let clip = doc.selection;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::shapes::fill_rect(layer, x, y, width, height, Rgba(color), clip);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn fill_ellipse(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    color: [u8; 4],
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let clip = doc.selection;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::shapes::fill_ellipse(layer, cx, cy, rx, ry, Rgba(color), clip);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn convert_color_profile(store: State<DocumentStore>, document_id: String, from: String, to: String) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let from = agenticart_core::color::NamedProfile::parse(&from).ok_or_else(|| format!("unknown profile '{from}'"))?;
    let to = agenticart_core::color::NamedProfile::parse(&to).ok_or_else(|| format!("unknown profile '{to}'"))?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            for layer in &mut doc.layers {
                layer.pixels = agenticart_core::color::convert_profile(&layer.pixels, from, to).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn cmyk_soft_proof(store: State<DocumentStore>, document_id: String, layer_id: String) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            layer.pixels = agenticart_core::color::cmyk_soft_proof(&layer.pixels);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn export_high_bit_depth(store: State<DocumentStore>, document_id: String, path: String) -> Result<(), String> {
    let id = parse_uuid(&document_id)?;
    let doc = store.get_clone(id).ok_or("document not found")?;
    let composite = agenticart_core::compositor::render(&doc);
    agenticart_core::color::export_high_bit_depth_png(&composite, std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cutout_subject(store: State<DocumentStore>, document_id: String, layer_id: String) -> Result<String, String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<Uuid, String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            let cutout = agenticart_core::ai::cutout_subject(layer).map_err(|e| e.to_string())?;
            let id = cutout.id;
            doc.layers.push(cutout);
            doc.active_layer = doc.layers.len() - 1;
            Ok(id)
        })
        .ok_or("document not found")?
        .map(|id| id.to_string())
}

#[derive(Serialize)]
pub struct SelectionRectInfo {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

#[tauri::command]
pub fn select_subject(store: State<DocumentStore>, document_id: String, layer_id: String) -> Result<Option<SelectionRectInfo>, String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<Option<SelectionRectInfo>, String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            let rect = agenticart_core::ai::subject_bounding_box(layer, 80).map_err(|e| e.to_string())?;
            doc.selection = rect;
            Ok(rect.map(|r| SelectionRectInfo { x: r.x, y: r.y, width: r.width, height: r.height }))
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn set_drop_shadow(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    enabled: bool,
    color: [u8; 4],
    offset_x: i32,
    offset_y: i32,
    blur_radius: f32,
    opacity: f32,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let shadow = enabled.then_some(agenticart_core::document::DropShadowStyle { color, offset_x, offset_y, blur_radius, opacity });
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            layer.style.drop_shadow = shadow;
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn set_stroke(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    enabled: bool,
    color: [u8; 4],
    width: u32,
    opacity: f32,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let stroke = enabled.then_some(agenticart_core::document::StrokeStyle { color, width, opacity });
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            layer.style.stroke = stroke;
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn save_project(store: State<DocumentStore>, document_id: String, path: String) -> Result<(), String> {
    let id = parse_uuid(&document_id)?;
    let doc = store.get_clone(id).ok_or("document not found")?;
    agenticart_core::project::save_project(&doc, std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_project(store: State<DocumentStore>, path: String) -> Result<DocumentInfo, String> {
    let doc = agenticart_core::project::load_project(std::path::Path::new(&path)).map_err(|e| e.to_string())?;
    let info = describe(&doc);
    store.insert(doc);
    Ok(info)
}

#[tauri::command]
pub fn export_psd(store: State<DocumentStore>, document_id: String, path: String) -> Result<(), String> {
    let id = parse_uuid(&document_id)?;
    let doc = store.get_clone(id).ok_or("document not found")?;
    agenticart_core::psd_export::document_to_psd(&doc, std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_psd(store: State<DocumentStore>, path: String) -> Result<DocumentInfo, String> {
    let doc = agenticart_core::psd_import::document_from_psd(std::path::Path::new(&path)).map_err(|e| e.to_string())?;
    let info = describe(&doc);
    store.insert(doc);
    Ok(info)
}

#[tauri::command]
pub fn open_document_from_file(store: State<DocumentStore>, path: String) -> Result<DocumentInfo, String> {
    let doc = agenticart_core::io::document_from_file(std::path::Path::new(&path)).map_err(|e| e.to_string())?;
    let info = describe(&doc);
    store.insert(doc);
    Ok(info)
}

#[tauri::command]
pub fn place_image(store: State<DocumentStore>, document_id: String, layer_id: String, path: String, x: i64, y: i64) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::io::place_image_on_layer(layer, std::path::Path::new(&path), x, y).map_err(|e| e.to_string())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn apply_gaussian_blur(store: State<DocumentStore>, document_id: String, layer_id: String, radius: f32) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let clip = doc.selection;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::filters::gaussian_blur(layer, radius, clip);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn apply_sharpen(store: State<DocumentStore>, document_id: String, layer_id: String, radius: f32, threshold: i32) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let clip = doc.selection;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::filters::sharpen(layer, radius, threshold, clip);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn undo(store: State<DocumentStore>, document_id: String) -> Result<bool, String> {
    let id = parse_uuid(&document_id)?;
    Ok(store.undo(id))
}

#[tauri::command]
pub fn redo(store: State<DocumentStore>, document_id: String) -> Result<bool, String> {
    let id = parse_uuid(&document_id)?;
    Ok(store.redo(id))
}

#[tauri::command]
pub fn transform_layer(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    translate_x: f32,
    translate_y: f32,
    rotate_degrees: f32,
    scale_x: f32,
    scale_y: f32,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            if translate_x != 0.0 || translate_y != 0.0 {
                agenticart_core::transform::translate(layer, translate_x, translate_y);
            }
            if rotate_degrees != 0.0 {
                agenticart_core::transform::rotate(layer, rotate_degrees);
            }
            if scale_x != 1.0 || scale_y != 1.0 {
                agenticart_core::transform::scale(layer, scale_x, scale_y);
            }
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn merge_down(store: State<DocumentStore>, document_id: String, layer_id: String) -> Result<Option<String>, String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let survivor = store.mutate(doc_id, move |doc| doc.merge_down(layer_id)).ok_or("document not found")?;
    Ok(survivor.map(|id| id.to_string()))
}

#[tauri::command]
pub fn flatten_image(store: State<DocumentStore>, document_id: String) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    store.mutate(doc_id, agenticart_core::compositor::flatten).ok_or("document not found".to_string())
}

#[tauri::command]
pub fn set_outer_glow(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    enabled: bool,
    color: [u8; 4],
    radius: f32,
    opacity: f32,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let glow = enabled.then_some(agenticart_core::document::OuterGlowStyle { color, radius, opacity });
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            layer.style.outer_glow = glow;
            Ok(())
        })
        .ok_or("document not found")?
}

#[derive(Deserialize)]
pub struct GradientStopInput {
    pub position: f32,
    pub color: [u8; 4],
}

#[tauri::command]
pub fn fill_gradient(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    kind: String,
    start_x: f32,
    start_y: f32,
    end_x: f32,
    end_y: f32,
    stops: Vec<GradientStopInput>,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let kind = if kind == "radial" { agenticart_core::gradient::GradientKind::Radial } else { agenticart_core::gradient::GradientKind::Linear };
    let stops: Vec<agenticart_core::gradient::GradientStop> = stops.into_iter().map(|s| agenticart_core::gradient::GradientStop { position: s.position, color: s.color }).collect();
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let clip = doc.selection;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::gradient::fill_gradient(layer, kind, (start_x, start_y), (end_x, end_y), &stops, clip);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn paint_bucket(store: State<DocumentStore>, document_id: String, layer_id: String, x: u32, y: u32, color: [u8; 4], tolerance: u8) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let clip = doc.selection;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::shapes::paint_bucket(layer, x, y, Rgba(color), tolerance, clip);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn clone_stamp(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    source_x: f32,
    source_y: f32,
    dest_points: Vec<StrokePointInput>,
    brush: BrushInput,
    continue_stroke: Option<bool>,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let dest_points: Vec<BrushPoint> = dest_points.into_iter().map(|p| BrushPoint { x: p.x, y: p.y, pressure: p.pressure.unwrap_or(1.0) }).collect();
    let brush = Brush { size: brush.size, color: Rgba(brush.color), hardness: brush.hardness };
    mutate_gesture(&store, doc_id, continue_stroke, move |doc| -> Result<(), String> {
        let clip = doc.selection;
        let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
        agenticart_core::paint::clone_stamp(layer, (source_x, source_y), &dest_points, &brush, clip);
        Ok(())
    })
    .ok_or("document not found")?
}

#[tauri::command]
pub fn dodge_burn(
    store: State<DocumentStore>,
    document_id: String,
    layer_id: String,
    mode: String,
    strength: f32,
    points: Vec<StrokePointInput>,
    brush: BrushInput,
    continue_stroke: Option<bool>,
) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let mode = if mode == "burn" { agenticart_core::paint::ToneMode::Burn } else { agenticart_core::paint::ToneMode::Dodge };
    let points: Vec<BrushPoint> = points.into_iter().map(|p| BrushPoint { x: p.x, y: p.y, pressure: p.pressure.unwrap_or(1.0) }).collect();
    let brush = Brush { size: brush.size, color: Rgba(brush.color), hardness: brush.hardness };
    mutate_gesture(&store, doc_id, continue_stroke, move |doc| -> Result<(), String> {
        let clip = doc.selection;
        let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
        agenticart_core::paint::dodge_burn(layer, &points, &brush, mode, strength, clip);
        Ok(())
    })
    .ok_or("document not found")?
}

#[tauri::command]
pub fn create_layer_comp(store: State<DocumentStore>, document_id: String, name: String) -> Result<String, String> {
    let doc_id = parse_uuid(&document_id)?;
    store.mutate(doc_id, move |doc| doc.create_layer_comp(name)).map(|id| id.to_string()).ok_or("document not found".to_string())
}

#[tauri::command]
pub fn apply_layer_comp(store: State<DocumentStore>, document_id: String, comp_id: String) -> Result<bool, String> {
    let doc_id = parse_uuid(&document_id)?;
    let comp_id = parse_uuid(&comp_id)?;
    store.mutate(doc_id, move |doc| doc.apply_layer_comp(comp_id)).ok_or("document not found".to_string())
}

#[tauri::command]
pub fn delete_layer_comp(store: State<DocumentStore>, document_id: String, comp_id: String) -> Result<bool, String> {
    let doc_id = parse_uuid(&document_id)?;
    let comp_id = parse_uuid(&comp_id)?;
    store.mutate(doc_id, move |doc| doc.delete_layer_comp(comp_id)).ok_or("document not found".to_string())
}

#[derive(Serialize)]
pub struct LayerCompInfo {
    pub id: String,
    pub name: String,
}

#[tauri::command]
pub fn list_layer_comps(store: State<DocumentStore>, document_id: String) -> Result<Vec<LayerCompInfo>, String> {
    let doc_id = parse_uuid(&document_id)?;
    let doc = store.get_clone(doc_id).ok_or("document not found")?;
    Ok(doc.layer_comps.iter().map(|c| LayerCompInfo { id: c.id.to_string(), name: c.name.clone() }).collect())
}

#[tauri::command]
pub fn add_smart_filter(store: State<DocumentStore>, document_id: String, layer_id: String, filter_type: String, radius: f32, threshold: i32, brightness: f32, contrast: f32, hue: f32, saturation: f32, lightness: f32) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    let filter = match filter_type.as_str() {
        "gaussianBlur" => agenticart_core::document::SmartFilter::GaussianBlur { radius },
        "sharpen" => agenticart_core::document::SmartFilter::Sharpen { radius, threshold },
        "brightnessContrast" => agenticart_core::document::SmartFilter::BrightnessContrast { brightness, contrast },
        "hueSaturation" => agenticart_core::document::SmartFilter::HueSaturation { hue, saturation, lightness },
        "invert" => agenticart_core::document::SmartFilter::Invert,
        other => return Err(format!("unknown smart filter type '{other}'")),
    };
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            layer.smart_filters.push(filter);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn remove_smart_filter(store: State<DocumentStore>, document_id: String, layer_id: String, index: usize) -> Result<bool, String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<bool, String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            if index >= layer.smart_filters.len() {
                return Ok(false);
            }
            layer.smart_filters.remove(index);
            Ok(true)
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn mask_from_selection(store: State<DocumentStore>, document_id: String, layer_id: String) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let selection = doc.selection.ok_or("no active selection")?;
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            let (w, h) = layer.pixels.dimensions();
            let mut mask = image::GrayImage::from_pixel(w, h, image::Luma([0]));
            for y in 0..h {
                for x in 0..w {
                    if selection.contains(x as i64, y as i64) {
                        mask.put_pixel(x, y, image::Luma([255]));
                    }
                }
            }
            layer.mask = Some(mask);
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn clear_mask(store: State<DocumentStore>, document_id: String, layer_id: String) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            layer.mask = None;
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn create_text_layer(store: State<DocumentStore>, document_id: String, text: String, x: i32, y: i32, size: f32, color: [u8; 4]) -> Result<String, String> {
    let doc_id = parse_uuid(&document_id)?;
    store
        .mutate(doc_id, move |doc| {
            let mut layer = agenticart_core::Layer::new_transparent("Text", doc.width, doc.height);
            layer.text = Some(agenticart_core::document::TextLayerData { text, x, y, size, color });
            let id = layer.id;
            doc.layers.push(layer);
            doc.active_layer = doc.layers.len() - 1;
            id
        })
        .map(|id| id.to_string())
        .ok_or("document not found".to_string())
}

#[tauri::command]
pub fn set_text_content(store: State<DocumentStore>, document_id: String, layer_id: String, text: String, x: i32, y: i32, size: f32, color: [u8; 4]) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            let data = layer.text.as_mut().ok_or("layer is not a text layer")?;
            data.text = text;
            data.x = x;
            data.y = y;
            data.size = size;
            data.color = color;
            Ok(())
        })
        .ok_or("document not found")?
}

#[tauri::command]
pub fn content_aware_fill(store: State<DocumentStore>, document_id: String, layer_id: String, x: i64, y: i64, width: u32, height: u32, iterations: usize) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::generative::content_aware_fill(layer, SelectionRect { x, y, width, height }, iterations);
            Ok(())
        })
        .ok_or("document not found")?
}

/// The MI-GAN-backed real ML inpainting from `generative_ml::ml_inpaint`
/// (see that module's doc comment for the model/licensing/quality-tradeoff
/// details), exposed to the UI alongside the classical PDE-based
/// `content_aware_fill` above so a human can compare the two the same way
/// an MCP agent already could via the `generative.mlInpaint` tool.
#[tauri::command]
pub fn ml_inpaint(store: State<DocumentStore>, document_id: String, layer_id: String, x: i64, y: i64, width: u32, height: u32) -> Result<(), String> {
    let doc_id = parse_uuid(&document_id)?;
    let layer_id = parse_uuid(&layer_id)?;
    store
        .mutate(doc_id, move |doc| -> Result<(), String> {
            let layer = doc.find_layer_mut(layer_id).ok_or("layer not found")?;
            agenticart_core::generative_ml::ml_inpaint(layer, SelectionRect { x, y, width, height }).map_err(|e| e.to_string())
        })
        .ok_or("document not found")?
}

use agenticart_core::document::BlendMode;
use agenticart_core::{Brush, BrushPoint, Document, DocumentStore};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use image::Rgba;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use uuid::Uuid;

/// In-memory named-macro store for `automation.*`: a literal replayable
/// sequence of the same MCP tool calls an agent (or the UI) would make one
/// at a time - one automation format, two front-ends. Not persisted
/// across server restarts; that's a natural follow-up (save alongside
/// the document store) rather than something this first version needs to
/// get right.
fn automation_store() -> &'static Mutex<HashMap<String, Vec<(String, Value)>>> {
    static STORE: OnceLock<Mutex<HashMap<String, Vec<(String, Value)>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// In-memory job-handle store for `job.*`: runs any tool call on a
/// background thread and returns a job id immediately, so a slow
/// operation (a large content-aware fill, a big upscale, a long
/// automation) never blocks the calling transport's connection - a
/// long-running op returns a job handle with progress/status rather than
/// blocking the connection. Deliberately generic - works for any tool,
/// not just a hardcoded list of "slow" ones, since agents can't always
/// predict which call on which canvas size will actually be slow.
enum JobStatus {
    Running,
    Done(Value),
    Error(String),
}

fn job_store() -> &'static Mutex<HashMap<String, JobStatus>> {
    static STORE: OnceLock<Mutex<HashMap<String, JobStatus>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn job_status_json(id: &str, status: &JobStatus) -> Value {
    match status {
        JobStatus::Running => json!({"jobId": id, "status": "running"}),
        JobStatus::Done(v) => json!({"jobId": id, "status": "done", "result": v}),
        JobStatus::Error(e) => json!({"jobId": id, "status": "error", "error": e}),
    }
}

fn parse_action_list(v: &Value) -> Result<Vec<(String, Value)>> {
    let arr = v.as_array().context("'actions' must be an array")?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let tool = entry.get("tool").and_then(Value::as_str).context("each action needs a 'tool' name")?.to_string();
        let args = entry.get("args").cloned().unwrap_or_else(|| json!({}));
        out.push((tool, args));
    }
    Ok(out)
}

/// Runs `actions` in order against each of `targets`, substituting each
/// action's `documentId` with the current target - so one recorded
/// sequence is portable across any document, not tied to the one it was
/// authored against. Best-effort per target: an action that fails aborts
/// only *that* target's remaining actions (recorded as its error) and
/// moves on to the next target, so one bad document in a batch doesn't
/// silently hide results for the rest.
fn run_actions_on_targets(store: &DocumentStore, actions: &[(String, Value)], targets: &[String]) -> Value {
    let mut results = Vec::with_capacity(targets.len());
    for target in targets {
        let mut ran = 0usize;
        let mut error: Option<String> = None;
        for (tool, args) in actions {
            let mut args = args.clone();
            if let Value::Object(map) = &mut args {
                map.insert("documentId".to_string(), json!(target));
                // A hardcoded layerId from wherever the action was
                // authored can never be meaningful against a *different*
                // target document (layer ids are per-document random
                // UUIDs), so every action's layerId (present or not) is
                // always replaced with the target's current active layer
                // - the only substitution that could ever be correct
                // across targets. Harmless for a tool that doesn't take a
                // layerId at all, since dispatch only reads the keys it
                // recognizes.
                if let Ok(target_id) = target.parse::<Uuid>() {
                    if let Some(doc) = store.get_clone(target_id) {
                        if let Some(active) = doc.layers.get(doc.active_layer) {
                            map.insert("layerId".to_string(), json!(active.id.to_string()));
                        }
                    }
                }
            }
            match call(store, tool, &args) {
                Ok(_) => ran += 1,
                Err(e) => {
                    error = Some(format!("action {ran} ('{tool}') failed: {e:#}"));
                    break;
                }
            }
        }
        results.push(json!({"documentId": target, "actionsRun": ran, "error": error}));
    }
    json!(results)
}

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// The tool catalog exposed over MCP. This list is what `tools/list`
/// returns — it's also the checklist for Phase 0 parity between the UI and
/// the agent API (see crates/core for the shared engine both drive).
pub fn catalog() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "document.create",
            description: "Create a new document (canvas) with a transparent background layer.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "width": {"type": "integer", "minimum": 1, "maximum": 16384},
                    "height": {"type": "integer", "minimum": 1, "maximum": 16384}
                },
                "required": ["width", "height"]
            }),
        },
        ToolDef {
            name: "document.list",
            description: "List currently open documents.",
            input_schema: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "document.duplicate",
            description: "Photoshop's 'Image > Duplicate': create an independent copy of a document (all layers, masks, smart filters, styles, layer comps) under a new documentId - edits to the copy never affect the original.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "name": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "document.close",
            description: "Close a document, discarding it from memory.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "document.focus",
            description: "Bring a document's tab/window to the front in whatever UI has this same document open - lets an agent make sure the human is looking at the right canvas. A no-op with no visible UI (e.g. the standalone stdio/HTTP server with no desktop app attached), and a UI must be polling for this to take effect (this app's desktop UI polls automatically).",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "document.resize",
            description: "Photoshop's 'Image Size': scale the canvas and every layer's pixels to new dimensions (high-quality resampling). Clears the active selection (its coordinates no longer apply).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "width": {"type": "integer", "minimum": 1, "maximum": 16384},
                    "height": {"type": "integer", "minimum": 1, "maximum": 16384}
                },
                "required": ["documentId", "width", "height"]
            }),
        },
        ToolDef {
            name: "document.resizeCanvas",
            description: "Photoshop's 'Canvas Size': change canvas dimensions WITHOUT scaling content - existing pixels are placed at (offsetX, offsetY) in the new canvas, cropped or padded with transparency as needed. Clears the active selection.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "width": {"type": "integer", "minimum": 1, "maximum": 16384},
                    "height": {"type": "integer", "minimum": 1, "maximum": 16384},
                    "offsetX": {"type": "integer", "default": 0},
                    "offsetY": {"type": "integer", "default": 0}
                },
                "required": ["documentId", "width", "height"]
            }),
        },
        ToolDef {
            name: "layer.createGroup",
            description: "Create a new empty layer group ('folder') at the top of the stack. Move existing layers into it with layer.moveIntoGroup.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "name": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "layer.moveIntoGroup",
            description: "Move an existing layer into a group, keeping the group's own opacity/blend mode/style applied to the flattened group content at render time.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}, "groupId": {"type": "string"}},
                "required": ["documentId", "layerId", "groupId"]
            }),
        },
        ToolDef {
            name: "layer.ungroup",
            description: "Remove a layer from its group, leaving it as a top-level layer at its current stack position.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.create",
            description: "Add a new transparent layer to a document, above the current top layer.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "name": {"type": "string"}
                },
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "layer.list",
            description: "List layers in a document, bottom to top.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "layer.setProperties",
            description: "Set a layer's name, opacity, visibility, blend mode, and/or clipping-mask flag.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "name": {"type": "string"},
                    "opacity": {"type": "number", "minimum": 0, "maximum": 1},
                    "visible": {"type": "boolean"},
                    "blendMode": {"type": "string", "enum": [
                        "normal", "multiply", "screen", "overlay", "darken", "lighten", "colorDodge", "colorBurn", "hardLight", "softLight",
                        "difference", "exclusion", "linearBurn", "darkerColor", "linearDodge", "lighterColor", "vividLight", "linearLight",
                        "pinLight", "hardMix", "subtract", "divide", "hue", "saturation", "color", "luminosity"
                    ]},
                    "clipToBelow": {"type": "boolean", "description": "Photoshop-style clipping mask: layer is only visible where the composite below it already has coverage."}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "generative.expand",
            description: "Photoshop's content-aware canvas extension (outpainting, minus a text prompt - classical PDE-based fill like generative.contentAwareFill, not ML/diffusion-based). Grows the canvas by top/bottom/left/right pixels, repositioning every layer's existing content, then fills the newly exposed border on `layerId` so its content appears to extend outward. Other layers get the same transparent padding a plain canvas resize gives them - call generative.contentAwareFill on any of those separately if they need filling too.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "top": {"type": "integer", "minimum": 0, "default": 0},
                    "bottom": {"type": "integer", "minimum": 0, "default": 0},
                    "left": {"type": "integer", "minimum": 0, "default": 0},
                    "right": {"type": "integer", "minimum": 0, "default": 0},
                    "iterations": {"type": "integer", "minimum": 1, "default": 64}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "generative.upscale",
            description: "Upscales the whole document by `factor` using high-quality Lanczos3 resampling (the same resampling document.resize uses) - a classical upscale, not ML/diffusion-based super-resolution. A documented simplification: it produces a smooth, larger image without hallucinating new fine detail the way a trained super-resolution model would.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "factor": {"type": "number", "minimum": 1.01, "maximum": 8, "description": "e.g. 2.0 doubles both dimensions"}
                },
                "required": ["documentId", "factor"]
            }),
        },
        ToolDef {
            name: "generative.contentAwareFill",
            description: "Fills a rectangular region by diffusing color in from its boundary (classical PDE-based inpainting, not ML/GAN-based - see generative.mlInpaint for that). Convincing for flat colors and smooth gradients; painting over a textured/patterned region produces a blur, not synthesized texture - a documented simplification, not a bug. Good for quick object removal on simple backgrounds; region defaults to the active selection if x/y/width/height are omitted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "width": {"type": "integer", "minimum": 1},
                    "height": {"type": "integer", "minimum": 1},
                    "iterations": {"type": "integer", "minimum": 1, "default": 64, "description": "Should be at least as large as the region's larger dimension for the fill to fully converge in the center."}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "generative.mlInpaint",
            description: "Real ML-based generative fill: fills a rectangular region using MI-GAN (a small, MIT-licensed trained GAN, not a diffusion model - one inference pass, no iterative denoising), which can plausibly synthesize texture and pattern continuation rather than just diffusing color like generative.contentAwareFill does. Quality drops off for a region that's large relative to the layer - prefer several smaller calls over one covering a large area (confirmed empirically: the underlying model's own guidance is 'small, incremental' masks, not one big single-shot region). Region defaults to the active selection if x/y/width/height are omitted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "width": {"type": "integer", "minimum": 1},
                    "height": {"type": "integer", "minimum": 1}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "ai.cutoutSubject",
            description: "AI-native: run local subject segmentation (U2Net) on a layer and cut its foreground subject out into a new layer (original RGB, alpha multiplied by the segmentation mask). The source layer is left untouched. This is an accelerant an agent may use, not a required step - manual selection + copy achieves the same result by hand.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "path.fill",
            description: "Fill a vector path (Photoshop pen-tool model: anchor points with optional Bezier control handles) with a solid color, clipped to the active selection if any. Requires a closed path with >=2 nodes.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "nodes": {
                        "type": "array",
                        "minItems": 2,
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "controlIn": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "description": "Bezier handle shaping the curve arriving at this anchor; omit for a straight segment"},
                                "controlOut": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "description": "Bezier handle shaping the curve leaving this anchor; omit for a straight segment"}
                            },
                            "required": ["x", "y"]
                        }
                    },
                    "closed": {"type": "boolean", "default": true},
                    "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "description": "[r, g, b, a]"}
                },
                "required": ["documentId", "layerId", "nodes", "color"]
            }),
        },
        ToolDef {
            name: "path.stroke",
            description: "Stroke a vector path's outline with a brush (same primitive as paint.strokePath - a vector path and a hand-drawn stroke are the same thing once flattened).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "nodes": {
                        "type": "array",
                        "minItems": 2,
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "controlIn": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}},
                                "controlOut": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}}
                            },
                            "required": ["x", "y"]
                        }
                    },
                    "closed": {"type": "boolean", "default": false},
                    "brush": {
                        "type": "object",
                        "properties": {
                            "size": {"type": "number", "minimum": 0.5},
                            "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4},
                            "hardness": {"type": "number", "minimum": 0, "maximum": 1}
                        }
                    }
                },
                "required": ["documentId", "layerId", "nodes"]
            }),
        },
        ToolDef {
            name: "shape.fillRect",
            description: "Fill an axis-aligned rectangle with a solid color on a layer (hard edges, clipped to the active selection if any).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "width": {"type": "integer", "minimum": 1},
                    "height": {"type": "integer", "minimum": 1},
                    "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "description": "[r, g, b, a]"}
                },
                "required": ["documentId", "layerId", "x", "y", "width", "height", "color"]
            }),
        },
        ToolDef {
            name: "shape.fillEllipse",
            description: "Fill an ellipse (cx, cy, rx, ry) with a solid color on a layer, clipped to the active selection if any.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "cx": {"type": "number"},
                    "cy": {"type": "number"},
                    "rx": {"type": "number", "minimum": 0},
                    "ry": {"type": "number", "minimum": 0},
                    "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "description": "[r, g, b, a]"}
                },
                "required": ["documentId", "layerId", "cx", "cy", "rx", "ry", "color"]
            }),
        },
        ToolDef {
            name: "shape.paintBucket",
            description: "Photoshop's Paint Bucket: flood-fills the contiguous region of pixels starting at (x, y) whose color is within tolerance of the seed pixel's own color, clipped to the active selection if any. Contiguous (4-connected) only - stops at any pixel outside tolerance, never jumps across a boundary.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "description": "[r, g, b, a]"},
                    "tolerance": {"type": "integer", "minimum": 0, "maximum": 255, "default": 32}
                },
                "required": ["documentId", "layerId", "x", "y", "color"]
            }),
        },
        ToolDef {
            name: "shape.fillGradient",
            description: "Photoshop's Gradient tool: fill a layer (clipped to the active selection if any) with a linear or radial color ramp between two or more stops. Linear: perpendicular bands from start to end, solid beyond either end. Radial: concentric rings centered at start, with end's distance from start setting the radius.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "kind": {"type": "string", "enum": ["linear", "radial"], "default": "linear"},
                    "startX": {"type": "number"},
                    "startY": {"type": "number"},
                    "endX": {"type": "number"},
                    "endY": {"type": "number"},
                    "stops": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "position": {"type": "number", "minimum": 0, "maximum": 1},
                                "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4}
                            },
                            "required": ["position", "color"]
                        }
                    }
                },
                "required": ["documentId", "layerId", "startX", "startY", "endX", "endY", "stops"]
            }),
        },
        ToolDef {
            name: "selection.setFromSubject",
            description: "AI-native: run local subject segmentation on a layer and set the document's rectangular selection to the bounding box of the detected subject. An approximation of Photoshop's 'Select Subject' - this engine's selection is a single rectangle, not an arbitrary mask, so it's the subject's bounding box rather than its exact outline.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "threshold": {"type": "integer", "minimum": 0, "maximum": 255, "default": 80, "description": "Mask confidence (0-255) above which a pixel counts as part of the subject."}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.setDropShadow",
            description: "Set or clear a layer's drop-shadow style. This is non-destructive - computed live at render time, so it can be changed or removed at any time without affecting the layer's actual pixels. Pass dropShadow: null (or omit it) to remove.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "dropShadow": {
                        "type": ["object", "null"],
                        "properties": {
                            "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "description": "[r, g, b, a]"},
                            "offsetX": {"type": "integer", "default": 6},
                            "offsetY": {"type": "integer", "default": 6},
                            "blurRadius": {"type": "number", "minimum": 0, "default": 6.0},
                            "opacity": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.6}
                        },
                        "required": ["color"]
                    }
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.setOuterGlow",
            description: "Set or clear a layer's outer-glow style: a blurred halo of color radiating from the layer's opaque edges (non-destructive, computed at render time - construction-wise a drop shadow with zero offset). Pass outerGlow: null (or omit it) to remove.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "outerGlow": {
                        "type": ["object", "null"],
                        "properties": {
                            "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "description": "[r, g, b, a]"},
                            "radius": {"type": "number", "minimum": 0, "default": 8.0},
                            "opacity": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.75}
                        },
                        "required": ["color"]
                    }
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.setBevelEmboss",
            description: "Set or clear a layer's Inner Bevel style: a pseudo-3D highlight/shadow along its opaque edges, simulating a raised surface lit from angleDegrees (0 = from the right, increasing counter-clockwise). Non-destructive, computed at render time. Pass bevelEmboss: null (or omit it) to remove.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "bevelEmboss": {
                        "type": ["object", "null"],
                        "properties": {
                            "depth": {"type": "number", "minimum": 0.5, "default": 3.0, "description": "How wide the beveled edge reads"},
                            "angleDegrees": {"type": "number", "default": 135.0},
                            "highlightColor": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "default": [255, 255, 255, 255]},
                            "shadowColor": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "default": [0, 0, 0, 255]}
                        }
                    }
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.setStroke",
            description: "Set or clear a layer's outer-stroke style (non-destructive, computed at render time). Pass stroke: null (or omit it) to remove.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "stroke": {
                        "type": ["object", "null"],
                        "properties": {
                            "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "description": "[r, g, b, a]"},
                            "width": {"type": "integer", "minimum": 0, "default": 3},
                            "opacity": {"type": "number", "minimum": 0, "maximum": 1, "default": 1.0}
                        },
                        "required": ["color"]
                    }
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "paint.strokePath",
            description: "Paint a brush stroke along a path of points onto a layer. This is the same primitive the UI's brush tool uses, so an agent can reproduce a reference image stroke-by-stroke with full manual control (not just call a generative filter).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "points": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "pressure": {"type": "number", "minimum": 0, "maximum": 1}
                            },
                            "required": ["x", "y"]
                        }
                    },
                    "brush": {
                        "type": "object",
                        "properties": {
                            "size": {"type": "number", "minimum": 0.5},
                            "color": {
                                "type": "array",
                                "items": {"type": "integer", "minimum": 0, "maximum": 255},
                                "minItems": 4,
                                "maxItems": 4,
                                "description": "[r, g, b, a]"
                            },
                            "hardness": {"type": "number", "minimum": 0, "maximum": 1}
                        }
                    }
                },
                "required": ["documentId", "layerId", "points"]
            }),
        },
        ToolDef {
            name: "canvas.render",
            description: "Render the flattened document to an image an agent can inspect, closing the generate-then-inspect loop.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "format": {"type": "string", "enum": ["png", "jpeg"], "default": "png"}
                },
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "canvas.renderTile",
            description: "Like canvas.render, but returns only a rectangular region of the composite instead of the whole canvas - useful for a very large document where pulling the entire canvas back as one base64 image would be huge, and an agent only needs to inspect one area. Does not reduce the memory the document occupies while open; only the returned image is cropped.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "width": {"type": "integer", "minimum": 1},
                    "height": {"type": "integer", "minimum": 1},
                    "format": {"type": "string", "enum": ["png", "jpeg"], "default": "png"}
                },
                "required": ["documentId", "x", "y", "width", "height"]
            }),
        },
        ToolDef {
            name: "color.eyedropper",
            description: "Sample the exact color at one pixel - the manual-tool color-pick primitive an agent reproducing a reference image needs (pick a color, then paint.strokePath/shape.fill* with it). Samples the flattened composite (all visible layers) by default, matching Photoshop's default eyedropper behavior; pass layerId to sample one layer's own raw pixels instead.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "x": {"type": "integer", "minimum": 0},
                    "y": {"type": "integer", "minimum": 0},
                    "layerId": {"type": "string", "description": "Sample this layer's own pixels instead of the flattened composite"}
                },
                "required": ["documentId", "x", "y"]
            }),
        },
        ToolDef {
            name: "canvas.getPixels",
            description: "Get the raw RGBA8 pixel buffer of a single layer, base64-encoded, for agents that want to composite pixels themselves.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "canvas.setPixels",
            description: "Replace a layer's raw RGBA8 pixel buffer from base64-encoded bytes; must match the layer's width*height*4 byte length.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "rgba8Base64": {"type": "string"}
                },
                "required": ["documentId", "layerId", "rgba8Base64"]
            }),
        },
        ToolDef {
            name: "export.toFile",
            description: "Render the document and save it to a file on disk. Format is inferred from the file extension.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "path": {"type": "string"}
                },
                "required": ["documentId", "path"]
            }),
        },
        ToolDef {
            name: "history.undo",
            description: "Undo the last mutation on a document (shared history with the UI).",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "history.redo",
            description: "Redo the last undone mutation on a document.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "history.list",
            description: "Report how many undo/redo steps are currently available for a document. History here is snapshot-based (shared with the UI), not a labeled action log, so this reports depth rather than named actions.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "selection.selectAll",
            description: "Set the active selection to the entire canvas.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "paint.erasePath",
            description: "Erase (reduce alpha) along a path of points on a layer, same shape as paint.strokePath.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "points": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "pressure": {"type": "number", "minimum": 0, "maximum": 1}
                            },
                            "required": ["x", "y"]
                        }
                    },
                    "brush": {
                        "type": "object",
                        "properties": {
                            "size": {"type": "number", "minimum": 0.5},
                            "hardness": {"type": "number", "minimum": 0, "maximum": 1}
                        }
                    }
                },
                "required": ["documentId", "layerId", "points"]
            }),
        },
        ToolDef {
            name: "paint.dodgeBurn",
            description: "Photoshop's Dodge/Burn: lightens (dodge) or darkens (burn) pixels along a brush stroke. Alpha is untouched - this changes tone, not opacity. A simplified single midtones exposure range rather than Photoshop's separate shadows/midtones/highlights selector.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "mode": {"type": "string", "enum": ["dodge", "burn"]},
                    "strength": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.15, "description": "How much a full-coverage dab shifts RGB, per pass"},
                    "points": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "pressure": {"type": "number", "minimum": 0, "maximum": 1}
                            },
                            "required": ["x", "y"]
                        }
                    },
                    "brush": {
                        "type": "object",
                        "properties": {
                            "size": {"type": "number", "minimum": 0.5},
                            "hardness": {"type": "number", "minimum": 0, "maximum": 1}
                        }
                    }
                },
                "required": ["documentId", "layerId", "mode", "points"]
            }),
        },
        ToolDef {
            name: "paint.sponge",
            description: "Photoshop's Sponge: saturates or desaturates pixels along a brush stroke, pulling each pixel toward or away from its own luminosity.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "mode": {"type": "string", "enum": ["saturate", "desaturate"]},
                    "strength": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.3},
                    "points": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "pressure": {"type": "number", "minimum": 0, "maximum": 1}
                            },
                            "required": ["x", "y"]
                        }
                    },
                    "brush": {
                        "type": "object",
                        "properties": {
                            "size": {"type": "number", "minimum": 0.5},
                            "hardness": {"type": "number", "minimum": 0, "maximum": 1}
                        }
                    }
                },
                "required": ["documentId", "layerId", "mode", "points"]
            }),
        },
        ToolDef {
            name: "paint.smudge",
            description: "Photoshop's Smudge: drags color along a brush stroke, blending each dab's destination pixels toward the color sampled at the previous dab's center - the classic smear.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "strength": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.5},
                    "points": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "pressure": {"type": "number", "minimum": 0, "maximum": 1}
                            },
                            "required": ["x", "y"]
                        }
                    },
                    "brush": {
                        "type": "object",
                        "properties": {
                            "size": {"type": "number", "minimum": 0.5},
                            "hardness": {"type": "number", "minimum": 0, "maximum": 1}
                        }
                    }
                },
                "required": ["documentId", "layerId", "points"]
            }),
        },
        ToolDef {
            name: "paint.cloneStamp",
            description: "Photoshop's Clone Stamp: paints along destPoints with color sampled from source plus a constant offset (the offset between source and the FIRST dest point is fixed for the whole stroke, matching Photoshop's default clone stamp behavior). Samples from the layer's state before this stroke, not the live in-progress paint, avoiding a self-referential smear when source and destination overlap.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "sourceX": {"type": "number"},
                    "sourceY": {"type": "number"},
                    "destPoints": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "pressure": {"type": "number", "minimum": 0, "maximum": 1}
                            },
                            "required": ["x", "y"]
                        }
                    },
                    "brush": {
                        "type": "object",
                        "properties": {
                            "size": {"type": "number", "minimum": 0.5},
                            "hardness": {"type": "number", "minimum": 0, "maximum": 1}
                        }
                    }
                },
                "required": ["documentId", "layerId", "sourceX", "sourceY", "destPoints"]
            }),
        },
        ToolDef {
            name: "paint.healingBrush",
            description: "Photoshop's Healing Brush: Clone Stamp plus a one-time tone correction (source patch average vs. destination patch average, computed once for the whole stroke) so the source's texture transfers but its brightness/color shifts to blend with the area it's healing into - the defining difference from a plain clone stamp. Simplified from Photoshop's own per-pixel Poisson blending to a single constant per-channel offset.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "sourceX": {"type": "number"},
                    "sourceY": {"type": "number"},
                    "destPoints": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "pressure": {"type": "number", "minimum": 0, "maximum": 1}
                            },
                            "required": ["x", "y"]
                        }
                    },
                    "brush": {
                        "type": "object",
                        "properties": {
                            "size": {"type": "number", "minimum": 0.5},
                            "hardness": {"type": "number", "minimum": 0, "maximum": 1}
                        }
                    }
                },
                "required": ["documentId", "layerId", "sourceX", "sourceY", "destPoints"]
            }),
        },
        ToolDef {
            name: "paint.patchTool",
            description: "Photoshop's Patch Tool: replaces a rectangular 'damaged' region with content copied from a same-sized region elsewhere, tone-corrected (same one-time average-offset idea as paint.healingBrush) so the patch blends into its surroundings instead of pasting in verbatim.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "destX": {"type": "integer"},
                    "destY": {"type": "integer"},
                    "width": {"type": "integer", "minimum": 1},
                    "height": {"type": "integer", "minimum": 1},
                    "sourceX": {"type": "integer"},
                    "sourceY": {"type": "integer"}
                },
                "required": ["documentId", "layerId", "destX", "destY", "width", "height", "sourceX", "sourceY"]
            }),
        },
        ToolDef {
            name: "layer.duplicate",
            description: "Duplicate a layer, inserting the copy directly above the original.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.delete",
            description: "Delete a layer. A document must always keep at least one layer.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.reorder",
            description: "Move a layer to a new stack position (0 = bottom).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "newIndex": {"type": "integer", "minimum": 0}
                },
                "required": ["documentId", "layerId", "newIndex"]
            }),
        },
        ToolDef {
            name: "selection.setRect",
            description: "Set a rectangular selection on the document; subsequent paint/erase/adjustment ops are clipped to it until cleared.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "width": {"type": "integer", "minimum": 1},
                    "height": {"type": "integer", "minimum": 1}
                },
                "required": ["documentId", "x", "y", "width", "height"]
            }),
        },
        ToolDef {
            name: "selection.clear",
            description: "Clear the document's active selection.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "adjustment.brightnessContrast",
            description: "Apply a brightness/contrast adjustment directly to a layer's pixels (clipped to the active selection if any).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "brightness": {"type": "number", "minimum": -1, "maximum": 1},
                    "contrast": {"type": "number", "minimum": -1, "maximum": 1}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "adjustment.hueSaturation",
            description: "Apply a hue/saturation/lightness adjustment directly to a layer's pixels (clipped to the active selection if any).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "hue": {"type": "number", "minimum": -180, "maximum": 180},
                    "saturation": {"type": "number", "minimum": -1, "maximum": 1},
                    "lightness": {"type": "number", "minimum": -1, "maximum": 1}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "adjustment.invert",
            description: "Invert a layer's RGB channels (clipped to the active selection if any).",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "adjustment.levels",
            description: "Photoshop's Levels: remaps [inBlack, inWhite] to [0,1] (clamped), applies gamma (1.0 = no change), then remaps [0,1] to [outBlack, outWhite].",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"}, "layerId": {"type": "string"},
                    "inBlack": {"type": "number", "minimum": 0, "maximum": 1, "default": 0},
                    "inWhite": {"type": "number", "minimum": 0, "maximum": 1, "default": 1},
                    "gamma": {"type": "number", "minimum": 0.01, "default": 1.0},
                    "outBlack": {"type": "number", "minimum": 0, "maximum": 1, "default": 0},
                    "outWhite": {"type": "number", "minimum": 0, "maximum": 1, "default": 1}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "adjustment.curves",
            description: "Photoshop's Curves, simplified to one master RGB curve rather than separate per-channel curves: points are (input, output) pairs in [0,1], piecewise-linearly interpolated. Requires at least 2 points.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"}, "layerId": {"type": "string"},
                    "points": {"type": "array", "minItems": 2, "items": {"type": "array", "items": {"type": "number", "minimum": 0, "maximum": 1}, "minItems": 2, "maxItems": 2}}
                },
                "required": ["documentId", "layerId", "points"]
            }),
        },
        ToolDef {
            name: "adjustment.exposure",
            description: "Photoshop's Exposure: exposure in stops (each +1.0 doubles linear brightness), offset is an additive shift applied before the exposure scale, gamma is applied last (1.0 = no change).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"}, "layerId": {"type": "string"},
                    "exposure": {"type": "number", "default": 0},
                    "offset": {"type": "number", "default": 0},
                    "gamma": {"type": "number", "minimum": 0.01, "default": 1.0}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "adjustment.colorBalance",
            description: "Photoshop's Color Balance, simplified to midtones only: cyanRed/magentaGreen/yellowBlue are additive shifts in [-1,1] to red/green/blue, weighted down near black/white so shadows and highlights shift less than midtones.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"}, "layerId": {"type": "string"},
                    "cyanRed": {"type": "number", "minimum": -1, "maximum": 1, "default": 0},
                    "magentaGreen": {"type": "number", "minimum": -1, "maximum": 1, "default": 0},
                    "yellowBlue": {"type": "number", "minimum": -1, "maximum": 1, "default": 0}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "adjustment.blackAndWhite",
            description: "Photoshop's Black & White: converts to grayscale using per-channel red/green/blue weights (Photoshop's default preset is close to 0.4/0.4/0.2; weights need not sum to 1.0, though they usually should to avoid clipping).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"}, "layerId": {"type": "string"},
                    "redWeight": {"type": "number", "default": 0.4},
                    "greenWeight": {"type": "number", "default": 0.4},
                    "blueWeight": {"type": "number", "default": 0.2}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "adjustment.vibrance",
            description: "Photoshop's Vibrance: boosts saturation more for already-low-saturation pixels and less for already-saturated ones, unlike a flat Hue/Saturation boost - resists blowing out already-vivid colors. amount in [-1,1].",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}, "amount": {"type": "number", "minimum": -1, "maximum": 1}},
                "required": ["documentId", "layerId", "amount"]
            }),
        },
        ToolDef {
            name: "adjustment.photoFilter",
            description: "Photoshop's Photo Filter: tints the image toward `color` by `density` ([0,1]) - a straight lerp toward the filter color, like a colored-glass camera filter.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"}, "layerId": {"type": "string"},
                    "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 3, "maxItems": 3, "description": "[r, g, b]"},
                    "density": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.25}
                },
                "required": ["documentId", "layerId", "color"]
            }),
        },
        ToolDef {
            name: "adjustment.channelMixer",
            description: "Photoshop's Channel Mixer: each output channel is a linear combination of the source R/G/B channels plus a constant, via a 3x3 matrix (rows = output R/G/B, columns = source R/G/B weights) and a per-channel constant in [-1,1]. E.g. matrix [[1,0,0],[0,1,0],[0,0,1]] with constants [0,0,0] is identity.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"}, "layerId": {"type": "string"},
                    "matrix": {"type": "array", "minItems": 3, "maxItems": 3, "items": {"type": "array", "minItems": 3, "maxItems": 3, "items": {"type": "number"}}},
                    "constants": {"type": "array", "minItems": 3, "maxItems": 3, "items": {"type": "number", "minimum": -1, "maximum": 1}, "default": [0, 0, 0]}
                },
                "required": ["documentId", "layerId", "matrix"]
            }),
        },
        ToolDef {
            name: "adjustment.gradientMap",
            description: "Photoshop's Gradient Map: maps each pixel's luminosity to a color from a gradient ramp (shadows to one end, highlights to the other) - same stop format as shape.fillGradient.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"}, "layerId": {"type": "string"},
                    "stops": {
                        "type": "array", "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "position": {"type": "number", "minimum": 0, "maximum": 1},
                                "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4}
                            },
                            "required": ["position", "color"]
                        }
                    }
                },
                "required": ["documentId", "layerId", "stops"]
            }),
        },
        ToolDef {
            name: "adjustment.posterize",
            description: "Photoshop's Posterize: reduces each channel to `levels` discrete steps (2-255).",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}, "levels": {"type": "integer", "minimum": 2, "maximum": 255, "default": 4}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "adjustment.threshold",
            description: "Photoshop's Threshold: every pixel becomes pure black or pure white based on whether its luminosity is above or below `cutoff` ([0,1]).",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}, "cutoff": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.5}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "adjustment.shadowsHighlights",
            description: "Photoshop's Shadows/Highlights: `shadows` ([0,1]) lifts dark tones, `highlights` ([0,1]) pulls down bright ones, each weighted by how far a pixel's luminosity actually is into that tonal range - midtones are left close to untouched. Simplified: no separate radius/detail/color-correction controls.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"}, "layerId": {"type": "string"},
                    "shadows": {"type": "number", "minimum": 0, "maximum": 1, "default": 0},
                    "highlights": {"type": "number", "minimum": 0, "maximum": 1, "default": 0}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "job.run",
            description: "Runs any tool call on a background thread and returns immediately with a jobId, instead of blocking the connection until it finishes - use for an operation that might be slow (a large content-aware fill, a big upscale, a long automation) on a canvas whose size makes that unpredictable ahead of time. Poll job.status(jobId) for the result.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "tool": {"type": "string", "description": "Any tool name from this catalog"},
                    "args": {"type": "object", "description": "That tool's normal arguments"}
                },
                "required": ["tool"]
            }),
        },
        ToolDef {
            name: "job.status",
            description: "Poll a job started by job.run. status is 'running', 'done' (result holds the wrapped tool's normal return value), or 'error' (error holds the message).",
            input_schema: json!({
                "type": "object",
                "properties": {"jobId": {"type": "string"}},
                "required": ["jobId"]
            }),
        },
        ToolDef {
            name: "job.list",
            description: "List all known jobs and their current status.",
            input_schema: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "automation.run",
            description: "Runs a literal sequence of MCP tool calls against one or more target documents, substituting each action's documentId with the current target - so a batch of edits authored once (or an agent's own recorded plan) can be replayed across many open documents in one call. Best-effort per target: a failing action stops only that target's remaining actions; other targets still run.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "actions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {"tool": {"type": "string"}, "args": {"type": "object"}},
                            "required": ["tool"]
                        }
                    },
                    "targets": {"type": "array", "items": {"type": "string"}, "description": "documentIds to run the action sequence against"}
                },
                "required": ["actions", "targets"]
            }),
        },
        ToolDef {
            name: "automation.save",
            description: "Saves a named, reusable action sequence (a 'macro') for later replay with automation.runNamed - Photoshop's Actions panel, MCP-shaped. In-memory for this server session.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "actions": {"type": "array", "items": {"type": "object", "properties": {"tool": {"type": "string"}, "args": {"type": "object"}}, "required": ["tool"]}}
                },
                "required": ["name", "actions"]
            }),
        },
        ToolDef {
            name: "automation.runNamed",
            description: "Replays a previously-saved automation (see automation.save) against one or more target documents.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "targets": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["name", "targets"]
            }),
        },
        ToolDef {
            name: "automation.list",
            description: "List saved automations and how many actions each has.",
            input_schema: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "automation.delete",
            description: "Delete a saved automation by name.",
            input_schema: json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
        },
        ToolDef {
            name: "automation.runOnFiles",
            description: "Photoshop's 'Batch' command: opens each image file, replays an action sequence against it (documentId substituted per-file, same as automation.run), then exports the result and closes the document. Best-effort per file - a file that fails to open or run stops just that file and is recorded as its error. Output path defaults to overwriting the input; pass outputSuffix to instead write '<name><suffix><ext>' next to each input (e.g. '_edited').",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "actions": {
                        "type": "array",
                        "items": {"type": "object", "properties": {"tool": {"type": "string"}, "args": {"type": "object"}}, "required": ["tool"]}
                    },
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Image file paths to process"},
                    "outputSuffix": {"type": "string", "description": "e.g. '_edited' to write alongside the input instead of overwriting it"}
                },
                "required": ["actions", "paths"]
            }),
        },
        ToolDef {
            name: "text.createLayer",
            description: "Create a live, non-destructive text layer (Photoshop's real text layers, unlike text.draw's one-shot raster stamp): its content stays editable via text.setContent and re-renders live at composite time. Appears above the current top layer.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "name": {"type": "string"},
                    "text": {"type": "string"},
                    "x": {"type": "integer", "default": 0},
                    "y": {"type": "integer", "default": 0},
                    "size": {"type": "number", "minimum": 1, "default": 32},
                    "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "description": "[r, g, b, a]"}
                },
                "required": ["documentId", "text"]
            }),
        },
        ToolDef {
            name: "text.setContent",
            description: "Edit an existing live text layer's string, position, size, and/or color (created by text.createLayer) - unlike text.draw, this never touches baked pixels, so it can be called repeatedly to revise the text.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "text": {"type": "string"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "size": {"type": "number", "minimum": 1},
                    "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "text.getContent",
            description: "Get a live text layer's current content. Returns hasText: false if the layer isn't a text layer.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "text.draw",
            description: "Raster-stamp text onto a layer at (x, y) using a system font. Phase 1 text is a direct pixel stamp (not a live-editable text layer yet).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "text": {"type": "string"},
                    "x": {"type": "integer", "default": 0},
                    "y": {"type": "integer", "default": 0},
                    "size": {"type": "number", "minimum": 1, "default": 32},
                    "color": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 4, "maxItems": 4, "description": "[r, g, b, a]"}
                },
                "required": ["documentId", "layerId", "text"]
            }),
        },
        ToolDef {
            name: "document.convertColorProfile",
            description: "Real ICC-based color management: converts every layer's pixels from one named color profile's gamut to another (e.g. Adobe RGB -> sRGB for web), genuinely remapping color values via the `moxcms` library, not just tagging metadata. Profile names: 'sRGB', 'Adobe RGB', 'Display P3', 'ProPhoto RGB'.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "from": {"type": "string", "enum": ["sRGB", "Adobe RGB", "Display P3", "ProPhoto RGB"]},
                    "to": {"type": "string", "enum": ["sRGB", "Adobe RGB", "Display P3", "ProPhoto RGB"]}
                },
                "required": ["documentId", "from", "to"]
            }),
        },
        ToolDef {
            name: "adjustment.cmykSoftProof",
            description: "Simulates print output by round-tripping a layer's pixels through naive RGB->CMYK->RGB conversion, clamping to CMYK's smaller ink gamut (similar to Photoshop's CMYK preview). Not calibrated to a specific press/ICC profile - a device-independent approximation.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "document.exportHighBitDepth",
            description: "Exports the flattened composite as a 16-bit-per-channel PNG. This losslessly widens the already-8-bit-quantized render, not a recovery of precision lost during 8-bit editing - useful as a 16-bit interchange format for downstream tools.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "path": {"type": "string"}},
                "required": ["documentId", "path"]
            }),
        },
        ToolDef {
            name: "document.exportPrintSeparations",
            description: "Photoshop's Print Color Separations: composites the document, then splits it into four grayscale CMYK plates (Cyan, Magenta, Yellow, Black - white = 0% ink, black = 100% ink for that plate, the standard press-preview convention), writing each as its own PNG next to `path` with a _C/_M/_Y/_K suffix (e.g. 'poster.png' -> poster_C.png, poster_M.png, poster_Y.png, poster_K.png). Registration/crop marks aren't included - this engine has no artboard/bleed concept to place them against.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "path": {"type": "string"}},
                "required": ["documentId", "path"]
            }),
        },
        ToolDef {
            name: "document.saveProject",
            description: "Save the document as an AgenticArt native project file (.agenticart) - a lossless, layered format that round-trips through document.openProject, unlike export.toFile which flattens.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "path": {"type": "string"}},
                "required": ["documentId", "path"]
            }),
        },
        ToolDef {
            name: "document.openProject",
            description: "Open an AgenticArt native project file (.agenticart), restoring all layers, blend modes, opacity, and selection exactly as saved.",
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "document.openPsd",
            description: "Open a Photoshop .psd file as a new multi-layer document (read-only interop - layer names, pixels, opacity, and visibility are preserved; adjustment layers/smart objects/effects are flattened into their layer's pixels). PSD writing is not supported.",
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "document.exportPsd",
            description: "Export the document as a Photoshop .psd file (uncompressed RGB 8-bit). Each top-level layer becomes a PSD layer (name/opacity/blend mode/visibility preserved); a group flattens into one PSD layer; layer styles (drop shadow/stroke) are not baked into the exported pixels. Opens correctly in Photoshop and in this app's own document.openPsd.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "path": {"type": "string"}},
                "required": ["documentId", "path"]
            }),
        },
        ToolDef {
            name: "document.openFile",
            description: "Open an image file (PNG/JPEG/TIFF/WebP/GIF/BMP) as a new document sized to the image, with the image as its single layer.",
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "document.openBayerMosaic",
            description: "RAW image processing: opens a grayscale image file as raw Bayer color-filter-array sensor mosaic data and demosaics it (reconstructs full RGB by averaging each pixel's missing two channels from its same-channel neighbors) into a new document. This is the actual RAW-processing algorithm, not a proprietary-container reader - it expects mosaic data already unwrapped from whatever camera format produced it (e.g. exported as a plain grayscale image), not a .CR2/.NEF/.ARW file directly.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "pattern": {"type": "string", "enum": ["RGGB", "BGGR", "GRBG", "GBRG"], "default": "RGGB", "description": "The camera sensor's color filter array layout"}
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "layer.placeImage",
            description: "Composite an image file onto an existing layer at (x, y) in canvas coordinates, clipped to canvas bounds. Useful for dropping a reference image onto the canvas, e.g. before an agent reproduces it by hand.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "path": {"type": "string"},
                    "x": {"type": "integer", "default": 0},
                    "y": {"type": "integer", "default": 0}
                },
                "required": ["documentId", "layerId", "path"]
            }),
        },
        ToolDef {
            name: "filter.gaussianBlur",
            description: "Apply a Gaussian blur to a layer (clipped to the active selection if any).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "radius": {"type": "number", "minimum": 0, "default": 4.0}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.addSmartFilter",
            description: "Append a non-destructive smart filter to a layer's filter stack. Unlike adjustment.*/filter.* (which bake the change into the layer's pixels immediately), a smart filter is computed live at render time - reorderable and removable at any point via layer.removeSmartFilter, and never touches the layer's actual pixel data.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "filter": {
                        "type": "object",
                        "properties": {
                            "type": {"type": "string", "enum": ["gaussianBlur", "sharpen", "brightnessContrast", "hueSaturation", "invert"]},
                            "radius": {"type": "number", "description": "gaussianBlur/sharpen"},
                            "threshold": {"type": "integer", "description": "sharpen"},
                            "brightness": {"type": "number", "minimum": -1, "maximum": 1, "description": "brightnessContrast"},
                            "contrast": {"type": "number", "minimum": -1, "maximum": 1, "description": "brightnessContrast"},
                            "hue": {"type": "number", "minimum": -180, "maximum": 180, "description": "hueSaturation"},
                            "saturation": {"type": "number", "minimum": -1, "maximum": 1, "description": "hueSaturation"},
                            "lightness": {"type": "number", "minimum": -1, "maximum": 1, "description": "hueSaturation"}
                        },
                        "required": ["type"]
                    }
                },
                "required": ["documentId", "layerId", "filter"]
            }),
        },
        ToolDef {
            name: "layer.listSmartFilters",
            description: "List a layer's non-destructive smart-filter stack, bottom (applied first) to top.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.removeSmartFilter",
            description: "Remove one entry from a layer's smart-filter stack by its index (as returned by layer.listSmartFilters).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "index": {"type": "integer", "minimum": 0}
                },
                "required": ["documentId", "layerId", "index"]
            }),
        },
        ToolDef {
            name: "transform.apply",
            description: "Photoshop's Free Transform, applied directly to a layer's pixels (and its mask, if any): translate, rotate (about the layer's own center, clockwise degrees), scale, and/or skew (about the layer's own center), composed in that order when more than one is given. The canvas itself doesn't change size - content moved/scaled past the edge is clipped, and vacated area becomes transparent. For a true 4-corner perspective/distort warp (not just a shear), use transform.perspective instead.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "translateX": {"type": "number", "default": 0},
                    "translateY": {"type": "number", "default": 0},
                    "rotateDegrees": {"type": "number", "default": 0},
                    "scaleX": {"type": "number", "default": 1, "description": "1.0 = no change"},
                    "scaleY": {"type": "number", "default": 1, "description": "1.0 = no change"},
                    "skewX": {"type": "number", "default": 0, "description": "Shear factor: e.g. 0.5 shifts a pixel 0.5*(y - centerY) horizontally"},
                    "skewY": {"type": "number", "default": 0, "description": "Shear factor: e.g. 0.5 shifts a pixel 0.5*(x - centerX) vertically"}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "transform.perspective",
            description: "Photoshop's Free Transform 'Perspective' (and the general case, 'Distort'): warps a layer via a true 4-point projective homography, not an approximation - the layer's four corners (top-left, top-right, bottom-right, bottom-left, in that order) land exactly on the four destination points given. Use this for real vanishing-point perspective (e.g. making a flat image look like it's on an angled surface); transform.apply's skew only shears, it can't converge parallel lines to a vanishing point. Errors if the four destination points are degenerate (e.g. three or more collinear).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "topLeft": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2},
                    "topRight": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2},
                    "bottomRight": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2},
                    "bottomLeft": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}
                },
                "required": ["documentId", "layerId", "topLeft", "topRight", "bottomRight", "bottomLeft"]
            }),
        },
        ToolDef {
            name: "transform.place3DPlane",
            description: "A minimal, honest '3D layer': treats the layer as a flat rectangular plane in 3D space, rotates it by pitchDegrees (tilt around the horizontal axis) and yawDegrees (turn around the vertical axis), then projects it back to 2D through a simple pinhole camera cameraDistance pixels back, looking at the plane's center - real 3D math, via transform.perspective under the hood. Not a 3D renderer: no mesh geometry beyond one flat plane, no lighting/shading, no z-buffering against other layers - full 3D rendering is a different scale of project. Errors if the rotation brings a corner to or past the camera.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "pitchDegrees": {"type": "number", "default": 0},
                    "yawDegrees": {"type": "number", "default": 0},
                    "cameraDistance": {"type": "number", "minimum": 1, "default": 1000}
                },
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "document.crop",
            description: "Photoshop's Crop tool: keeps only the (x, y, width, height) region of the canvas, discarding everything outside it and repositioning every layer's content so that region becomes the new (0,0)-origin canvas. Thin, explicitly-named wrapper over the same underlying operation document.resizeCanvas uses.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "width": {"type": "integer", "minimum": 1},
                    "height": {"type": "integer", "minimum": 1}
                },
                "required": ["documentId", "x", "y", "width", "height"]
            }),
        },
        ToolDef {
            name: "layer.mergeDown",
            description: "Photoshop's 'Merge Down': bakes a layer onto the one directly beneath it (respecting both layers' opacity, blend mode, smart filters, masks, and styles) into a single layer, removing the merged-away layer. Fails (ok: false) if the layer is at the bottom of its stack, the layer below belongs to a different group, or either layer is a group marker (ungroup first).",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "document.flatten",
            description: "Photoshop's 'Flatten Image': composites every visible layer (respecting groups/masks/smart filters/styles) onto an opaque white background and replaces the entire layer stack with that single 'Background' layer. Irreversible except via history.undo.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "document.createLayerComp",
            description: "Photoshop's 'New Layer Comp': snapshot every layer's current visibility and opacity under a name, so it can be recalled later with document.applyLayerComp - useful for an agent to compare design variants without destructively editing the document each time.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "name": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "document.applyLayerComp",
            description: "Restore every layer's visibility and opacity to what a previously captured layer comp recorded. Layers deleted since the comp was captured are silently skipped.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "compId": {"type": "string"}},
                "required": ["documentId", "compId"]
            }),
        },
        ToolDef {
            name: "document.listLayerComps",
            description: "List a document's saved layer comps.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}},
                "required": ["documentId"]
            }),
        },
        ToolDef {
            name: "document.deleteLayerComp",
            description: "Delete a saved layer comp.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "compId": {"type": "string"}},
                "required": ["documentId", "compId"]
            }),
        },
        ToolDef {
            name: "layer.setVectorMask",
            description: "Set a layer's mask by rasterizing a closed vector path (same pen-tool model as path.fill - anchor points with optional Bezier handles): white (visible) inside the path's even-odd-filled interior, black (hidden) outside. Overwrites any existing mask on the layer; use layer.setMask/layer.getMask for pixel-precise mask edits afterward.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "nodes": {
                        "type": "array",
                        "minItems": 2,
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "controlIn": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}},
                                "controlOut": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}}
                            },
                            "required": ["x", "y"]
                        }
                    }
                },
                "required": ["documentId", "layerId", "nodes"]
            }),
        },
        ToolDef {
            name: "layer.setMask",
            description: "Set a layer's non-destructive raster mask from a raw grayscale8 buffer (white = fully visible, black = fully hidden), base64-encoded; must be width*height bytes. Multiplies the layer's alpha at render time - never touches the layer's actual pixels, and can be cleared with layer.clearMask.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "gray8Base64": {"type": "string"}
                },
                "required": ["documentId", "layerId", "gray8Base64"]
            }),
        },
        ToolDef {
            name: "layer.maskFromSelection",
            description: "Set a layer's mask to white inside the document's active rectangular selection and black outside it - the common 'mask what I've selected' workflow. Requires an active selection (selection.setRect).",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.getMask",
            description: "Get a layer's raster mask as a raw grayscale8 buffer, base64-encoded. Returns hasMask: false if the layer has no mask (fully visible).",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "layer.clearMask",
            description: "Remove a layer's raster mask, restoring full visibility.",
            input_schema: json!({
                "type": "object",
                "properties": {"documentId": {"type": "string"}, "layerId": {"type": "string"}},
                "required": ["documentId", "layerId"]
            }),
        },
        ToolDef {
            name: "plugin.runFilter",
            description: "Run a third-party filter plugin (a .wasm file built with agenticart-plugin-sdk) over a layer's pixels. The plugin executes in a sandbox with no host imports at all - no filesystem, network, or host-call access, and a fuel budget bounding total work - so it can only transform the RGBA8 pixel buffer it's handed.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "pluginPath": {"type": "string", "description": "Path to a compiled .wasm plugin file"}
                },
                "required": ["documentId", "layerId", "pluginPath"]
            }),
        },
        ToolDef {
            name: "filter.sharpen",
            description: "Apply an unsharp-mask sharpen to a layer (clipped to the active selection if any).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string"},
                    "layerId": {"type": "string"},
                    "radius": {"type": "number", "minimum": 0, "default": 1.5},
                    "threshold": {"type": "integer", "minimum": 0, "default": 0}
                },
                "required": ["documentId", "layerId"]
            }),
        },
    ]
}

fn parse_points_and_brush(args: &Value) -> Result<(Vec<BrushPoint>, agenticart_core::Brush)> {
    let points: Vec<BrushPoint> = args
        .get("points")
        .and_then(Value::as_array)
        .context("missing 'points'")?
        .iter()
        .map(|p| BrushPoint {
            x: p.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32,
            y: p.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32,
            pressure: p.get("pressure").and_then(Value::as_f64).unwrap_or(1.0) as f32,
        })
        .collect();
    let mut brush = Brush::default();
    if let Some(b) = args.get("brush") {
        if let Some(size) = b.get("size").and_then(Value::as_f64) {
            brush.size = size as f32;
        }
        if let Some(hardness) = b.get("hardness").and_then(Value::as_f64) {
            brush.hardness = hardness as f32;
        }
        if let Some(color) = b.get("color").and_then(Value::as_array) {
            if color.len() == 4 {
                let c: Vec<u8> = color.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                brush.color = Rgba([c[0], c[1], c[2], c[3]]);
            }
        }
    }
    Ok((points, brush))
}

fn parse_point(v: &Value) -> Option<agenticart_core::path::Point> {
    Some(agenticart_core::path::Point { x: v.get("x")?.as_f64()? as f32, y: v.get("y")?.as_f64()? as f32 })
}

fn parse_path(args: &Value) -> Result<agenticart_core::path::Path> {
    let nodes_val = args.get("nodes").and_then(Value::as_array).context("missing 'nodes'")?;
    let closed = args.get("closed").and_then(Value::as_bool).unwrap_or(false);
    let mut nodes = Vec::with_capacity(nodes_val.len());
    for n in nodes_val {
        let anchor = parse_point(n).context("each node needs numeric 'x'/'y'")?;
        let control_in = n.get("controlIn").and_then(parse_point);
        let control_out = n.get("controlOut").and_then(parse_point);
        nodes.push(agenticart_core::path::PathNode { anchor, control_in, control_out });
    }
    if nodes.len() < 2 {
        bail!("a path needs at least 2 nodes");
    }
    Ok(agenticart_core::path::Path { nodes, closed })
}

fn parse_brush(args: &Value) -> Result<Brush> {
    let mut brush = Brush::default();
    if let Some(b) = args.get("brush") {
        if let Some(size) = b.get("size").and_then(Value::as_f64) {
            brush.size = size as f32;
        }
        if let Some(hardness) = b.get("hardness").and_then(Value::as_f64) {
            brush.hardness = hardness as f32;
        }
        if let Some(color) = b.get("color").and_then(Value::as_array) {
            if color.len() == 4 {
                let c: Vec<u8> = color.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                brush.color = Rgba([c[0], c[1], c[2], c[3]]);
            }
        }
    }
    Ok(brush)
}

fn parse_color(args: &Value, field: &str) -> Result<Rgba<u8>> {
    let arr = args.get(field).and_then(Value::as_array).with_context(|| format!("missing '{field}'"))?;
    if arr.len() != 4 {
        bail!("'{field}' must be [r,g,b,a]");
    }
    let c: Vec<u8> = arr.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
    Ok(Rgba([c[0], c[1], c[2], c[3]]))
}

fn parse_uuid(v: &Value, field: &str) -> Result<Uuid> {
    let s = v.get(field).and_then(Value::as_str).context(format!("missing '{field}'"))?;
    Uuid::parse_str(s).map_err(|_| anyhow!("'{field}' is not a valid id"))
}

fn layer_summary(doc: &Document) -> Value {
    json!(doc
        .layers
        .iter()
        .map(|l| json!({
            "id": l.id.to_string(),
            "name": l.name,
            "opacity": l.opacity,
            "visible": l.visible,
            "blendMode": l.blend_mode.as_str(),
            "clipToBelow": l.clip_to_below,
            "hasDropShadow": l.style.drop_shadow.is_some(),
            "hasStroke": l.style.stroke.is_some(),
            "hasOuterGlow": l.style.outer_glow.is_some(),
            "hasBevelEmboss": l.style.bevel_emboss.is_some(),
            "smartFilterCount": l.smart_filters.len(),
            "hasMask": l.mask.is_some(),
            "hasText": l.text.is_some(),
            "isGroup": l.is_group,
            "parentGroup": l.parent_group.map(|g| g.to_string()),
        }))
        .collect::<Vec<_>>())
}

fn parse_blend_mode(s: &str) -> Result<BlendMode> {
    BlendMode::parse(s).ok_or_else(|| anyhow!("unknown blend mode '{s}'"))
}

fn parse_smart_filter(v: &Value) -> Result<agenticart_core::document::SmartFilter> {
    use agenticart_core::document::SmartFilter;
    let ty = v.get("type").and_then(Value::as_str).context("filter.type missing")?;
    let f = |field: &str| v.get(field).and_then(Value::as_f64);
    Ok(match ty {
        "gaussianBlur" => SmartFilter::GaussianBlur { radius: f("radius").unwrap_or(4.0) as f32 },
        "sharpen" => SmartFilter::Sharpen { radius: f("radius").unwrap_or(1.5) as f32, threshold: f("threshold").unwrap_or(0.0) as i32 },
        "brightnessContrast" => SmartFilter::BrightnessContrast { brightness: f("brightness").unwrap_or(0.0) as f32, contrast: f("contrast").unwrap_or(0.0) as f32 },
        "hueSaturation" => SmartFilter::HueSaturation {
            hue: f("hue").unwrap_or(0.0) as f32,
            saturation: f("saturation").unwrap_or(0.0) as f32,
            lightness: f("lightness").unwrap_or(0.0) as f32,
        },
        "invert" => SmartFilter::Invert,
        other => bail!("unknown smart filter type '{other}'"),
    })
}

fn smart_filter_to_json(f: &agenticart_core::document::SmartFilter) -> Value {
    use agenticart_core::document::SmartFilter;
    match *f {
        SmartFilter::GaussianBlur { radius } => json!({"type": "gaussianBlur", "radius": radius}),
        SmartFilter::Sharpen { radius, threshold } => json!({"type": "sharpen", "radius": radius, "threshold": threshold}),
        SmartFilter::BrightnessContrast { brightness, contrast } => json!({"type": "brightnessContrast", "brightness": brightness, "contrast": contrast}),
        SmartFilter::HueSaturation { hue, saturation, lightness } => json!({"type": "hueSaturation", "hue": hue, "saturation": saturation, "lightness": lightness}),
        SmartFilter::Invert => json!({"type": "invert"}),
    }
}

/// Dispatches a `tools/call` invocation to the shared document engine. This
/// is the only place tool names are matched to core operations, and it is
/// the exact same set of operations the desktop UI's commands call into
/// (via `agenticart-core`), so agent edits and human edits are
/// indistinguishable to the engine.
pub fn call(store: &DocumentStore, name: &str, args: &Value) -> Result<Value> {
    match name {
        "document.create" => {
            let width = args.get("width").and_then(Value::as_u64).context("missing 'width'")? as u32;
            let height = args.get("height").and_then(Value::as_u64).context("missing 'height'")? as u32;
            let doc_name = args.get("name").and_then(Value::as_str).unwrap_or("Untitled").to_string();
            let doc = Document::new(doc_name, width, height);
            let id = store.insert(doc);
            Ok(json!({"documentId": id.to_string()}))
        }
        "document.list" => Ok(json!(store
            .list()
            .into_iter()
            .map(|(id, name)| json!({"id": id.to_string(), "name": name}))
            .collect::<Vec<_>>())),
        "document.duplicate" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let mut doc = store.get_clone(doc_id).context("document not found")?;
            doc.id = Uuid::new_v4();
            if let Some(n) = args.get("name").and_then(Value::as_str) {
                doc.name = n.to_string();
            } else {
                doc.name = format!("{} copy", doc.name);
            }
            let new_id = store.insert(doc);
            Ok(json!({"documentId": new_id.to_string()}))
        }
        "document.close" => {
            let id = parse_uuid(args, "documentId")?;
            Ok(json!({"closed": store.close(id)}))
        }
        "document.focus" => {
            let id = parse_uuid(args, "documentId")?;
            let focused = store.request_focus(id);
            if !focused {
                bail!("document not found");
            }
            Ok(json!({"focused": true}))
        }
        "job.run" => {
            let tool = args.get("tool").and_then(Value::as_str).context("missing 'tool'")?.to_string();
            let job_args = args.get("args").cloned().unwrap_or_else(|| json!({}));
            let job_id = Uuid::new_v4().to_string();
            job_store().lock().unwrap().insert(job_id.clone(), JobStatus::Running);

            let store_clone = store.clone();
            let job_id_for_thread = job_id.clone();
            std::thread::spawn(move || {
                let status = match call(&store_clone, &tool, &job_args) {
                    Ok(v) => JobStatus::Done(v),
                    Err(e) => JobStatus::Error(format!("{e:#}")),
                };
                job_store().lock().unwrap().insert(job_id_for_thread, status);
            });

            Ok(json!({"jobId": job_id}))
        }
        "job.status" => {
            let job_id = args.get("jobId").and_then(Value::as_str).context("missing 'jobId'")?;
            let jobs = job_store().lock().unwrap();
            let status = jobs.get(job_id).with_context(|| format!("no job with id '{job_id}'"))?;
            Ok(job_status_json(job_id, status))
        }
        "job.list" => {
            let jobs = job_store().lock().unwrap();
            Ok(json!(jobs.iter().map(|(id, status)| job_status_json(id, status)).collect::<Vec<_>>()))
        }
        "automation.run" => {
            let actions = parse_action_list(args.get("actions").context("missing 'actions'")?)?;
            let targets: Vec<String> = args
                .get("targets")
                .and_then(Value::as_array)
                .context("missing 'targets'")?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            Ok(run_actions_on_targets(store, &actions, &targets))
        }
        "automation.save" => {
            let name = args.get("name").and_then(Value::as_str).context("missing 'name'")?.to_string();
            let actions = parse_action_list(args.get("actions").context("missing 'actions'")?)?;
            automation_store().lock().unwrap().insert(name, actions);
            Ok(json!({"ok": true}))
        }
        "automation.runNamed" => {
            let name = args.get("name").and_then(Value::as_str).context("missing 'name'")?;
            let actions = automation_store().lock().unwrap().get(name).cloned().with_context(|| format!("no saved automation named '{name}'"))?;
            let targets: Vec<String> = args
                .get("targets")
                .and_then(Value::as_array)
                .context("missing 'targets'")?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            Ok(run_actions_on_targets(store, &actions, &targets))
        }
        "automation.list" => {
            let macros = automation_store().lock().unwrap();
            Ok(json!(macros.iter().map(|(name, actions)| json!({"name": name, "actionCount": actions.len()})).collect::<Vec<_>>()))
        }
        "automation.delete" => {
            let name = args.get("name").and_then(Value::as_str).context("missing 'name'")?;
            let removed = automation_store().lock().unwrap().remove(name).is_some();
            Ok(json!({"deleted": removed}))
        }
        "automation.runOnFiles" => {
            let actions = parse_action_list(args.get("actions").context("missing 'actions'")?)?;
            let paths: Vec<String> = args
                .get("paths")
                .and_then(Value::as_array)
                .context("missing 'paths'")?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            let output_suffix = args.get("outputSuffix").and_then(Value::as_str);

            let mut results = Vec::with_capacity(paths.len());
            for path_str in &paths {
                let path = PathBuf::from(path_str);
                let outcome = (|| -> Result<usize> {
                    let doc = agenticart_core::io::document_from_file(&path)?;
                    let doc_id = store.insert(doc);
                    let run_result = run_actions_on_targets(store, &actions, &[doc_id.to_string()]);
                    let ran = run_result[0]["actionsRun"].as_u64().unwrap_or(0) as usize;
                    let action_error = run_result[0]["error"].as_str().map(str::to_string);

                    let out_path = match output_suffix {
                        Some(suffix) => {
                            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
                            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png");
                            path.with_file_name(format!("{stem}{suffix}.{ext}"))
                        }
                        None => path.clone(),
                    };
                    let doc = store.get_clone(doc_id).context("document vanished mid-batch")?;
                    agenticart_core::io::export_to_file(&doc, &out_path)?;
                    store.close(doc_id);

                    if let Some(e) = action_error {
                        bail!(e);
                    }
                    Ok(ran)
                })();

                match outcome {
                    Ok(ran) => results.push(json!({"path": path_str, "actionsRun": ran, "error": Value::Null})),
                    Err(e) => results.push(json!({"path": path_str, "actionsRun": 0, "error": e.to_string()})),
                }
            }
            Ok(json!(results))
        }
        "text.createLayer" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let text = args.get("text").and_then(Value::as_str).context("missing 'text'")?.to_string();
            let name = args.get("name").and_then(Value::as_str).unwrap_or("Text").to_string();
            let x = args.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
            let y = args.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
            let size = args.get("size").and_then(Value::as_f64).unwrap_or(32.0) as f32;
            let color = match args.get("color").and_then(Value::as_array) {
                Some(c) if c.len() == 4 => {
                    let c: Vec<u8> = c.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                    [c[0], c[1], c[2], c[3]]
                }
                _ => [0, 0, 0, 255],
            };
            let layer_id = store
                .mutate(doc_id, move |doc| {
                    let mut layer = agenticart_core::Layer::new_transparent(name, doc.width, doc.height);
                    layer.text = Some(agenticart_core::document::TextLayerData { text, x, y, size, color });
                    let id = layer.id;
                    doc.layers.push(layer);
                    doc.active_layer = doc.layers.len() - 1;
                    id
                })
                .context("document not found")?;
            Ok(json!({"layerId": layer_id.to_string()}))
        }
        "text.setContent" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let text = args.get("text").and_then(Value::as_str).map(str::to_string);
            let x = args.get("x").and_then(Value::as_i64);
            let y = args.get("y").and_then(Value::as_i64);
            let size = args.get("size").and_then(Value::as_f64);
            let color = match args.get("color").and_then(Value::as_array) {
                Some(c) if c.len() == 4 => Some([
                    c[0].as_u64().unwrap_or(0) as u8,
                    c[1].as_u64().unwrap_or(0) as u8,
                    c[2].as_u64().unwrap_or(0) as u8,
                    c[3].as_u64().unwrap_or(0) as u8,
                ]),
                _ => None,
            };
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    let data = layer.text.as_mut().context("layer is not a text layer - create one with text.createLayer")?;
                    if let Some(t) = text {
                        data.text = t;
                    }
                    if let Some(x) = x {
                        data.x = x as i32;
                    }
                    if let Some(y) = y {
                        data.y = y as i32;
                    }
                    if let Some(s) = size {
                        data.size = s as f32;
                    }
                    if let Some(c) = color {
                        data.color = c;
                    }
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "text.getContent" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            let layer = doc.layers.iter().find(|l| l.id == layer_id).context("layer not found")?;
            match &layer.text {
                Some(t) => Ok(json!({"hasText": true, "text": t.text, "x": t.x, "y": t.y, "size": t.size, "color": t.color})),
                None => Ok(json!({"hasText": false})),
            }
        }
        "text.draw" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let text = args.get("text").and_then(Value::as_str).context("missing 'text'")?.to_string();
            let x = args.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
            let y = args.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
            let size = args.get("size").and_then(Value::as_f64).unwrap_or(32.0) as f32;
            let color = match args.get("color").and_then(Value::as_array) {
                Some(c) if c.len() == 4 => {
                    let c: Vec<u8> = c.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                    Rgba([c[0], c[1], c[2], c[3]])
                }
                _ => Rgba([0, 0, 0, 255]),
            };
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::text::draw_text(layer, &text, x, y, size, color, None)
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "document.convertColorProfile" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let from_str = args.get("from").and_then(Value::as_str).context("missing 'from'")?;
            let to_str = args.get("to").and_then(Value::as_str).context("missing 'to'")?;
            let from = agenticart_core::color::NamedProfile::parse(from_str).with_context(|| format!("unknown profile '{from_str}'"))?;
            let to = agenticart_core::color::NamedProfile::parse(to_str).with_context(|| format!("unknown profile '{to_str}'"))?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    for layer in &mut doc.layers {
                        layer.pixels = agenticart_core::color::convert_profile(&layer.pixels, from, to)?;
                    }
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.cmykSoftProof" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    layer.pixels = agenticart_core::color::cmyk_soft_proof(&layer.pixels);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "document.exportHighBitDepth" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let path = args.get("path").and_then(Value::as_str).context("missing 'path'")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            let composite = agenticart_core::compositor::render(&doc);
            agenticart_core::color::export_high_bit_depth_png(&composite, &PathBuf::from(path))?;
            Ok(json!({"ok": true, "path": path}))
        }
        "document.exportPrintSeparations" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let path_str = args.get("path").and_then(Value::as_str).context("missing 'path'")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            let composite = agenticart_core::compositor::render(&doc);
            let plates = agenticart_core::color::cmyk_separations(&composite);
            let path = PathBuf::from(path_str);
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("separation").to_string();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png").to_string();
            let mut written = Vec::with_capacity(4);
            for (plate, suffix) in plates.iter().zip(["C", "M", "Y", "K"]) {
                let out_path = path.with_file_name(format!("{stem}_{suffix}.{ext}"));
                plate.save(&out_path)?;
                written.push(out_path.to_string_lossy().to_string());
            }
            Ok(json!({"ok": true, "paths": written}))
        }
        "document.saveProject" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let path = args.get("path").and_then(Value::as_str).context("missing 'path'")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            agenticart_core::project::save_project(&doc, &PathBuf::from(path))?;
            Ok(json!({"ok": true, "path": path}))
        }
        "document.openProject" => {
            let path = args.get("path").and_then(Value::as_str).context("missing 'path'")?;
            let doc = agenticart_core::project::load_project(&PathBuf::from(path))?;
            let info = json!({"documentId": doc.id.to_string(), "width": doc.width, "height": doc.height, "name": doc.name});
            store.insert(doc);
            Ok(info)
        }
        "document.openPsd" => {
            let path = args.get("path").and_then(Value::as_str).context("missing 'path'")?;
            let doc = agenticart_core::psd_import::document_from_psd(&PathBuf::from(path))?;
            let info = json!({"documentId": doc.id.to_string(), "width": doc.width, "height": doc.height, "layerCount": doc.layers.len()});
            store.insert(doc);
            Ok(info)
        }
        "document.exportPsd" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let path = args.get("path").and_then(Value::as_str).context("missing 'path'")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            agenticart_core::psd_export::document_to_psd(&doc, &PathBuf::from(path))?;
            Ok(json!({"ok": true, "path": path}))
        }
        "document.openFile" => {
            let path = args.get("path").and_then(Value::as_str).context("missing 'path'")?;
            let doc = agenticart_core::io::document_from_file(&PathBuf::from(path))?;
            let info = json!({"documentId": doc.id.to_string(), "width": doc.width, "height": doc.height});
            store.insert(doc);
            Ok(info)
        }
        "document.openBayerMosaic" => {
            let path = args.get("path").and_then(Value::as_str).context("missing 'path'")?;
            let pattern_str = args.get("pattern").and_then(Value::as_str).unwrap_or("RGGB");
            let pattern = agenticart_core::raw::BayerPattern::parse(pattern_str).with_context(|| format!("unknown Bayer pattern '{pattern_str}'"))?;
            let doc = agenticart_core::raw::document_from_bayer_mosaic(&PathBuf::from(path), pattern)?;
            let info = json!({"documentId": doc.id.to_string(), "width": doc.width, "height": doc.height});
            store.insert(doc);
            Ok(info)
        }
        "layer.placeImage" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let path = args.get("path").and_then(Value::as_str).context("missing 'path'")?.to_string();
            let x = args.get("x").and_then(Value::as_i64).unwrap_or(0);
            let y = args.get("y").and_then(Value::as_i64).unwrap_or(0);
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::io::place_image_on_layer(layer, &PathBuf::from(path), x, y)
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "document.resize" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let width = args.get("width").and_then(Value::as_u64).context("missing 'width'")? as u32;
            let height = args.get("height").and_then(Value::as_u64).context("missing 'height'")? as u32;
            store
                .mutate(doc_id, move |doc| agenticart_core::resize::resize_document(doc, width, height))
                .context("document not found")?;
            Ok(json!({"ok": true}))
        }
        "document.resizeCanvas" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let width = args.get("width").and_then(Value::as_u64).context("missing 'width'")? as u32;
            let height = args.get("height").and_then(Value::as_u64).context("missing 'height'")? as u32;
            let offset_x = args.get("offsetX").and_then(Value::as_i64).unwrap_or(0);
            let offset_y = args.get("offsetY").and_then(Value::as_i64).unwrap_or(0);
            store
                .mutate(doc_id, move |doc| agenticart_core::resize::resize_canvas(doc, width, height, offset_x, offset_y))
                .context("document not found")?;
            Ok(json!({"ok": true}))
        }
        "layer.createGroup" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let name = args.get("name").and_then(Value::as_str).unwrap_or("Group").to_string();
            let group_id = store.mutate(doc_id, move |doc| doc.create_group(name)).context("document not found")?;
            Ok(json!({"layerId": group_id.to_string()}))
        }
        "layer.moveIntoGroup" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let group_id = parse_uuid(args, "groupId")?;
            let ok = store.mutate(doc_id, move |doc| doc.move_layer_into_group(layer_id, group_id)).context("document not found")?;
            Ok(json!({"ok": ok}))
        }
        "layer.ungroup" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let ok = store.mutate(doc_id, move |doc| doc.ungroup_layer(layer_id)).context("document not found")?;
            Ok(json!({"ok": ok}))
        }
        "layer.create" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_name = args.get("name").and_then(Value::as_str).unwrap_or("Layer").to_string();
            let layer_id = store
                .mutate(doc_id, |doc| {
                    let layer = agenticart_core::Layer::new_transparent(layer_name, doc.width, doc.height);
                    let id = layer.id;
                    doc.layers.push(layer);
                    doc.active_layer = doc.layers.len() - 1;
                    id
                })
                .context("document not found")?;
            Ok(json!({"layerId": layer_id.to_string()}))
        }
        "layer.list" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            Ok(layer_summary(&doc))
        }
        "layer.setProperties" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let name = args.get("name").and_then(Value::as_str).map(str::to_string);
            let opacity = args.get("opacity").and_then(Value::as_f64);
            let visible = args.get("visible").and_then(Value::as_bool);
            let blend_mode = match args.get("blendMode").and_then(Value::as_str) {
                Some(s) => Some(parse_blend_mode(s)?),
                None => None,
            };
            let clip_to_below = args.get("clipToBelow").and_then(Value::as_bool);
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    if let Some(n) = name {
                        layer.name = n;
                    }
                    if let Some(o) = opacity {
                        layer.opacity = o as f32;
                    }
                    if let Some(v) = visible {
                        layer.visible = v;
                    }
                    if let Some(bm) = blend_mode {
                        layer.blend_mode = bm;
                    }
                    if let Some(c) = clip_to_below {
                        layer.clip_to_below = c;
                    }
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "generative.expand" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let top = args.get("top").and_then(Value::as_u64).unwrap_or(0) as u32;
            let bottom = args.get("bottom").and_then(Value::as_u64).unwrap_or(0) as u32;
            let left = args.get("left").and_then(Value::as_u64).unwrap_or(0) as u32;
            let right = args.get("right").and_then(Value::as_u64).unwrap_or(0) as u32;
            let iterations = args.get("iterations").and_then(Value::as_u64).unwrap_or(64) as usize;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    agenticart_core::generative::expand_canvas(doc, layer_id, top, bottom, left, right, iterations)
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "generative.upscale" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let factor = args.get("factor").and_then(Value::as_f64).context("missing 'factor'")?;
            if factor <= 1.0 {
                bail!("'factor' must be greater than 1.0 (use document.resize to shrink)");
            }
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let new_width = ((doc.width as f64) * factor).round().clamp(1.0, 16384.0) as u32;
                    let new_height = ((doc.height as f64) * factor).round().clamp(1.0, 16384.0) as u32;
                    agenticart_core::resize::resize_document(doc, new_width, new_height);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "generative.contentAwareFill" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let explicit_rect = match (
                args.get("x").and_then(Value::as_i64),
                args.get("y").and_then(Value::as_i64),
                args.get("width").and_then(Value::as_u64),
                args.get("height").and_then(Value::as_u64),
            ) {
                (Some(x), Some(y), Some(width), Some(height)) => Some(agenticart_core::SelectionRect { x, y, width: width as u32, height: height as u32 }),
                _ => None,
            };
            let iterations = args.get("iterations").and_then(Value::as_u64).unwrap_or(64) as usize;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let region = explicit_rect.or(doc.selection).context("no region given and no active selection - pass x/y/width/height or call selection.setRect first")?;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::generative::content_aware_fill(layer, region, iterations);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "generative.mlInpaint" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let explicit_rect = match (
                args.get("x").and_then(Value::as_i64),
                args.get("y").and_then(Value::as_i64),
                args.get("width").and_then(Value::as_u64),
                args.get("height").and_then(Value::as_u64),
            ) {
                (Some(x), Some(y), Some(width), Some(height)) => Some(agenticart_core::SelectionRect { x, y, width: width as u32, height: height as u32 }),
                _ => None,
            };
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let region = explicit_rect.or(doc.selection).context("no region given and no active selection - pass x/y/width/height or call selection.setRect first")?;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::generative_ml::ml_inpaint(layer, region)
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "ai.cutoutSubject" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let new_layer_id = store
                .mutate(doc_id, move |doc| -> Result<Uuid> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    let cutout = agenticart_core::ai::cutout_subject(layer)?;
                    let id = cutout.id;
                    doc.layers.push(cutout);
                    doc.active_layer = doc.layers.len() - 1;
                    Ok(id)
                })
                .context("document not found")??;
            Ok(json!({"layerId": new_layer_id.to_string()}))
        }
        "path.fill" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let path = parse_path(args)?;
            let color = parse_color(args, "color")?;
            store.set_last_color(doc_id, color.0);
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::path::fill_path(layer, &path, color, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "path.stroke" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let path = parse_path(args)?;
            let brush = parse_brush(args)?;
            store.set_last_color(doc_id, brush.color.0);
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::path::stroke_path_shape(layer, &path, &brush, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "shape.fillRect" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let x = args.get("x").and_then(Value::as_i64).context("missing 'x'")?;
            let y = args.get("y").and_then(Value::as_i64).context("missing 'y'")?;
            let width = args.get("width").and_then(Value::as_u64).context("missing 'width'")? as u32;
            let height = args.get("height").and_then(Value::as_u64).context("missing 'height'")? as u32;
            let color = parse_color(args, "color")?;
            store.set_last_color(doc_id, color.0);
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::shapes::fill_rect(layer, x, y, width, height, color, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "shape.fillEllipse" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let cx = args.get("cx").and_then(Value::as_f64).context("missing 'cx'")? as f32;
            let cy = args.get("cy").and_then(Value::as_f64).context("missing 'cy'")? as f32;
            let rx = args.get("rx").and_then(Value::as_f64).context("missing 'rx'")? as f32;
            let ry = args.get("ry").and_then(Value::as_f64).context("missing 'ry'")? as f32;
            let color = parse_color(args, "color")?;
            store.set_last_color(doc_id, color.0);
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::shapes::fill_ellipse(layer, cx, cy, rx, ry, color, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "shape.paintBucket" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let x = args.get("x").and_then(Value::as_i64).context("missing 'x'")?;
            let y = args.get("y").and_then(Value::as_i64).context("missing 'y'")?;
            let color = parse_color(args, "color")?;
            let tolerance = args.get("tolerance").and_then(Value::as_u64).unwrap_or(32) as u8;
            store.set_last_color(doc_id, color.0);
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    if x < 0 || y < 0 {
                        bail!("'x' and 'y' must be non-negative");
                    }
                    agenticart_core::shapes::paint_bucket(layer, x as u32, y as u32, color, tolerance, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "shape.fillGradient" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let kind = match args.get("kind").and_then(Value::as_str).unwrap_or("linear") {
                "radial" => agenticart_core::gradient::GradientKind::Radial,
                _ => agenticart_core::gradient::GradientKind::Linear,
            };
            let start_x = args.get("startX").and_then(Value::as_f64).context("missing 'startX'")? as f32;
            let start_y = args.get("startY").and_then(Value::as_f64).context("missing 'startY'")? as f32;
            let end_x = args.get("endX").and_then(Value::as_f64).context("missing 'endX'")? as f32;
            let end_y = args.get("endY").and_then(Value::as_f64).context("missing 'endY'")? as f32;
            let stops_val = args.get("stops").and_then(Value::as_array).context("missing 'stops'")?;
            let mut stops = Vec::with_capacity(stops_val.len());
            for s in stops_val {
                let position = s.get("position").and_then(Value::as_f64).context("stop missing 'position'")? as f32;
                let color = s.get("color").and_then(Value::as_array).context("stop missing 'color'")?;
                if color.len() != 4 {
                    bail!("stop.color must be [r,g,b,a]");
                }
                let c: Vec<u8> = color.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                stops.push(agenticart_core::gradient::GradientStop { position, color: [c[0], c[1], c[2], c[3]] });
            }
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::gradient::fill_gradient(layer, kind, (start_x, start_y), (end_x, end_y), &stops, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "selection.setFromSubject" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let threshold = args.get("threshold").and_then(Value::as_u64).unwrap_or(80) as u8;
            let rect = store
                .mutate(doc_id, move |doc| -> Result<Option<agenticart_core::SelectionRect>> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    let rect = agenticart_core::ai::subject_bounding_box(layer, threshold)?;
                    doc.selection = rect;
                    Ok(rect)
                })
                .context("document not found")??;
            match rect {
                Some(r) => Ok(json!({"found": true, "x": r.x, "y": r.y, "width": r.width, "height": r.height})),
                None => Ok(json!({"found": false})),
            }
        }
        "layer.setDropShadow" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let shadow = match args.get("dropShadow") {
                None | Some(Value::Null) => None,
                Some(ds) => {
                    let color = ds.get("color").and_then(Value::as_array).context("dropShadow.color missing")?;
                    if color.len() != 4 {
                        bail!("dropShadow.color must be [r,g,b,a]");
                    }
                    let c: Vec<u8> = color.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                    Some(agenticart_core::document::DropShadowStyle {
                        color: [c[0], c[1], c[2], c[3]],
                        offset_x: ds.get("offsetX").and_then(Value::as_i64).unwrap_or(6) as i32,
                        offset_y: ds.get("offsetY").and_then(Value::as_i64).unwrap_or(6) as i32,
                        blur_radius: ds.get("blurRadius").and_then(Value::as_f64).unwrap_or(6.0) as f32,
                        opacity: ds.get("opacity").and_then(Value::as_f64).unwrap_or(0.6) as f32,
                    })
                }
            };
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    layer.style.drop_shadow = shadow;
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "layer.setOuterGlow" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let glow = match args.get("outerGlow") {
                None | Some(Value::Null) => None,
                Some(og) => {
                    let color = og.get("color").and_then(Value::as_array).context("outerGlow.color missing")?;
                    if color.len() != 4 {
                        bail!("outerGlow.color must be [r,g,b,a]");
                    }
                    let c: Vec<u8> = color.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                    Some(agenticart_core::document::OuterGlowStyle {
                        color: [c[0], c[1], c[2], c[3]],
                        radius: og.get("radius").and_then(Value::as_f64).unwrap_or(8.0) as f32,
                        opacity: og.get("opacity").and_then(Value::as_f64).unwrap_or(0.75) as f32,
                    })
                }
            };
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    layer.style.outer_glow = glow;
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "layer.setBevelEmboss" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let bevel = match args.get("bevelEmboss") {
                None | Some(Value::Null) => None,
                Some(be) => {
                    let parse_color4 = |v: &Value, field: &str, default: [u8; 4]| -> Result<[u8; 4]> {
                        match v.get(field).and_then(Value::as_array) {
                            Some(arr) if arr.len() == 4 => {
                                let c: Vec<u8> = arr.iter().map(|x| x.as_u64().unwrap_or(0) as u8).collect();
                                Ok([c[0], c[1], c[2], c[3]])
                            }
                            Some(_) => bail!("'{field}' must be [r,g,b,a]"),
                            None => Ok(default),
                        }
                    };
                    Some(agenticart_core::document::BevelEmbossStyle {
                        depth: be.get("depth").and_then(Value::as_f64).unwrap_or(3.0) as f32,
                        angle_degrees: be.get("angleDegrees").and_then(Value::as_f64).unwrap_or(135.0) as f32,
                        highlight_color: parse_color4(be, "highlightColor", [255, 255, 255, 255])?,
                        shadow_color: parse_color4(be, "shadowColor", [0, 0, 0, 255])?,
                    })
                }
            };
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    layer.style.bevel_emboss = bevel;
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "layer.setStroke" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let stroke = match args.get("stroke") {
                None | Some(Value::Null) => None,
                Some(st) => {
                    let color = st.get("color").and_then(Value::as_array).context("stroke.color missing")?;
                    if color.len() != 4 {
                        bail!("stroke.color must be [r,g,b,a]");
                    }
                    let c: Vec<u8> = color.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                    Some(agenticart_core::document::StrokeStyle {
                        color: [c[0], c[1], c[2], c[3]],
                        width: st.get("width").and_then(Value::as_u64).unwrap_or(3) as u32,
                        opacity: st.get("opacity").and_then(Value::as_f64).unwrap_or(1.0) as f32,
                    })
                }
            };
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    layer.style.stroke = stroke;
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "paint.strokePath" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let (points, brush) = parse_points_and_brush(args)?;
            store.set_last_color(doc_id, brush.color.0);
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::paint::stroke_path(layer, &points, &brush, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "paint.dodgeBurn" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let mode = match args.get("mode").and_then(Value::as_str).context("missing 'mode'")? {
                "dodge" => agenticart_core::paint::ToneMode::Dodge,
                "burn" => agenticart_core::paint::ToneMode::Burn,
                other => bail!("'mode' must be 'dodge' or 'burn', got '{other}'"),
            };
            let strength = args.get("strength").and_then(Value::as_f64).unwrap_or(0.15) as f32;
            let (points, brush) = parse_points_and_brush(args)?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::paint::dodge_burn(layer, &points, &brush, mode, strength, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "paint.sponge" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let mode = match args.get("mode").and_then(Value::as_str).context("missing 'mode'")? {
                "saturate" => agenticart_core::paint::SpongeMode::Saturate,
                "desaturate" => agenticart_core::paint::SpongeMode::Desaturate,
                other => bail!("'mode' must be 'saturate' or 'desaturate', got '{other}'"),
            };
            let strength = args.get("strength").and_then(Value::as_f64).unwrap_or(0.3) as f32;
            let (points, brush) = parse_points_and_brush(args)?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::paint::sponge(layer, &points, &brush, mode, strength, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "paint.smudge" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let strength = args.get("strength").and_then(Value::as_f64).unwrap_or(0.5) as f32;
            let (points, brush) = parse_points_and_brush(args)?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::paint::smudge(layer, &points, &brush, strength, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "paint.cloneStamp" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let source_x = args.get("sourceX").and_then(Value::as_f64).context("missing 'sourceX'")? as f32;
            let source_y = args.get("sourceY").and_then(Value::as_f64).context("missing 'sourceY'")? as f32;
            let dest_points: Vec<BrushPoint> = args
                .get("destPoints")
                .and_then(Value::as_array)
                .context("missing 'destPoints'")?
                .iter()
                .map(|p| BrushPoint {
                    x: p.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                    y: p.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                    pressure: p.get("pressure").and_then(Value::as_f64).unwrap_or(1.0) as f32,
                })
                .collect();
            // Note: clone_stamp's brush.color is unused by the actual
            // algorithm (it copies existing pixels) - deliberately not
            // tracked as "last color used" here, unlike the paint tools
            // above, since it would show a color that has nothing to do
            // with what was actually painted.
            let brush = parse_brush(args)?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::paint::clone_stamp(layer, (source_x, source_y), &dest_points, &brush, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "paint.healingBrush" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let source_x = args.get("sourceX").and_then(Value::as_f64).context("missing 'sourceX'")? as f32;
            let source_y = args.get("sourceY").and_then(Value::as_f64).context("missing 'sourceY'")? as f32;
            let dest_points: Vec<BrushPoint> = args
                .get("destPoints")
                .and_then(Value::as_array)
                .context("missing 'destPoints'")?
                .iter()
                .map(|p| BrushPoint {
                    x: p.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                    y: p.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                    pressure: p.get("pressure").and_then(Value::as_f64).unwrap_or(1.0) as f32,
                })
                .collect();
            let brush = parse_brush(args)?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::paint::healing_brush(layer, (source_x, source_y), &dest_points, &brush, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "paint.patchTool" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let dest_x = args.get("destX").and_then(Value::as_i64).context("missing 'destX'")?;
            let dest_y = args.get("destY").and_then(Value::as_i64).context("missing 'destY'")?;
            let width = args.get("width").and_then(Value::as_u64).context("missing 'width'")? as u32;
            let height = args.get("height").and_then(Value::as_u64).context("missing 'height'")? as u32;
            let source_x = args.get("sourceX").and_then(Value::as_i64).context("missing 'sourceX'")?;
            let source_y = args.get("sourceY").and_then(Value::as_i64).context("missing 'sourceY'")?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::paint::patch(layer, agenticart_core::SelectionRect { x: dest_x, y: dest_y, width, height }, source_x, source_y);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "paint.erasePath" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let (points, brush) = parse_points_and_brush(args)?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::paint::erase_path(layer, &points, &brush, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "layer.duplicate" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let new_id = store
                .mutate(doc_id, move |doc| doc.duplicate_layer(layer_id))
                .context("document not found")?
                .context("layer not found")?;
            Ok(json!({"layerId": new_id.to_string()}))
        }
        "layer.delete" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let deleted = store
                .mutate(doc_id, move |doc| doc.delete_layer(layer_id))
                .context("document not found")?;
            Ok(json!({"deleted": deleted}))
        }
        "layer.reorder" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let new_index = args.get("newIndex").and_then(Value::as_u64).context("missing 'newIndex'")? as usize;
            let ok = store
                .mutate(doc_id, move |doc| doc.reorder_layer(layer_id, new_index))
                .context("document not found")?;
            Ok(json!({"ok": ok}))
        }
        "selection.setRect" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let x = args.get("x").and_then(Value::as_i64).context("missing 'x'")?;
            let y = args.get("y").and_then(Value::as_i64).context("missing 'y'")?;
            let width = args.get("width").and_then(Value::as_u64).context("missing 'width'")? as u32;
            let height = args.get("height").and_then(Value::as_u64).context("missing 'height'")? as u32;
            store
                .mutate(doc_id, move |doc| {
                    doc.selection = Some(agenticart_core::SelectionRect { x, y, width, height });
                })
                .context("document not found")?;
            Ok(json!({"ok": true}))
        }
        "selection.clear" => {
            let doc_id = parse_uuid(args, "documentId")?;
            store.mutate(doc_id, |doc| doc.selection = None).context("document not found")?;
            Ok(json!({"ok": true}))
        }
        "adjustment.brightnessContrast" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let brightness = args.get("brightness").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let contrast = args.get("contrast").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::brightness_contrast(layer, brightness, contrast, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.hueSaturation" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let hue = args.get("hue").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let saturation = args.get("saturation").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let lightness = args.get("lightness").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::hue_saturation(layer, hue, saturation, lightness, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.invert" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::invert(layer, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.levels" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let in_black = args.get("inBlack").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let in_white = args.get("inWhite").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            let gamma = args.get("gamma").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            let out_black = args.get("outBlack").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let out_white = args.get("outWhite").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::levels(layer, in_black, in_white, gamma, out_black, out_white, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.curves" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let points_val = args.get("points").and_then(Value::as_array).context("missing 'points'")?;
            let mut points = Vec::with_capacity(points_val.len());
            for p in points_val {
                let arr = p.as_array().context("each point must be [input, output]")?;
                if arr.len() != 2 {
                    bail!("each point must be [input, output]");
                }
                let input = arr[0].as_f64().context("point input must be a number")? as f32;
                let output = arr[1].as_f64().context("point output must be a number")? as f32;
                points.push((input, output));
            }
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::curves(layer, &points, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.exposure" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let exposure_stops = args.get("exposure").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let offset = args.get("offset").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let gamma = args.get("gamma").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::exposure(layer, exposure_stops, offset, gamma, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.colorBalance" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let cyan_red = args.get("cyanRed").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let magenta_green = args.get("magentaGreen").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let yellow_blue = args.get("yellowBlue").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::color_balance(layer, cyan_red, magenta_green, yellow_blue, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.blackAndWhite" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let red_weight = args.get("redWeight").and_then(Value::as_f64).unwrap_or(0.4) as f32;
            let green_weight = args.get("greenWeight").and_then(Value::as_f64).unwrap_or(0.4) as f32;
            let blue_weight = args.get("blueWeight").and_then(Value::as_f64).unwrap_or(0.2) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::black_and_white(layer, red_weight, green_weight, blue_weight, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.vibrance" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let amount = args.get("amount").and_then(Value::as_f64).context("missing 'amount'")? as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::vibrance(layer, amount, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.photoFilter" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let color_val = args.get("color").and_then(Value::as_array).context("missing 'color'")?;
            if color_val.len() != 3 {
                bail!("'color' must be [r,g,b]");
            }
            let color: Vec<u8> = color_val.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
            let color = [color[0], color[1], color[2]];
            let density = args.get("density").and_then(Value::as_f64).unwrap_or(0.25) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::photo_filter(layer, color, density, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.channelMixer" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let matrix_val = args.get("matrix").and_then(Value::as_array).context("missing 'matrix'")?;
            if matrix_val.len() != 3 {
                bail!("'matrix' must have 3 rows");
            }
            let mut matrix = [[0f32; 3]; 3];
            for (row_idx, row_val) in matrix_val.iter().enumerate() {
                let row = row_val.as_array().context("each matrix row must be an array")?;
                if row.len() != 3 {
                    bail!("each matrix row must have 3 numbers");
                }
                for (col_idx, v) in row.iter().enumerate() {
                    matrix[row_idx][col_idx] = v.as_f64().context("matrix values must be numbers")? as f32;
                }
            }
            let constants_val = args.get("constants").and_then(Value::as_array);
            let mut constants = [0f32; 3];
            if let Some(cv) = constants_val {
                if cv.len() != 3 {
                    bail!("'constants' must have 3 numbers");
                }
                for (i, v) in cv.iter().enumerate() {
                    constants[i] = v.as_f64().unwrap_or(0.0) as f32;
                }
            }
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::channel_mixer(layer, matrix, constants, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.gradientMap" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let stops_val = args.get("stops").and_then(Value::as_array).context("missing 'stops'")?;
            let mut stops = Vec::with_capacity(stops_val.len());
            for s in stops_val {
                let position = s.get("position").and_then(Value::as_f64).context("stop missing 'position'")? as f32;
                let color = s.get("color").and_then(Value::as_array).context("stop missing 'color'")?;
                if color.len() != 4 {
                    bail!("stop.color must be [r,g,b,a]");
                }
                let c: Vec<u8> = color.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                stops.push(agenticart_core::gradient::GradientStop { position, color: [c[0], c[1], c[2], c[3]] });
            }
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::gradient_map(layer, &stops, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.posterize" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let levels = args.get("levels").and_then(Value::as_u64).unwrap_or(4) as u32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::posterize(layer, levels, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.threshold" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let cutoff = args.get("cutoff").and_then(Value::as_f64).unwrap_or(0.5) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::threshold(layer, cutoff, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "adjustment.shadowsHighlights" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let shadows = args.get("shadows").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let highlights = args.get("highlights").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::adjustments::shadows_highlights(layer, shadows, highlights, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "filter.gaussianBlur" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let sigma = args.get("radius").and_then(Value::as_f64).unwrap_or(4.0) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::filters::gaussian_blur(layer, sigma, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "filter.sharpen" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let sigma = args.get("radius").and_then(Value::as_f64).unwrap_or(1.5) as f32;
            let threshold = args.get("threshold").and_then(Value::as_i64).unwrap_or(0) as i32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let clip = doc.selection;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::filters::sharpen(layer, sigma, threshold, clip);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "layer.addSmartFilter" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let filter_val = args.get("filter").context("missing 'filter'")?;
            let filter = parse_smart_filter(filter_val)?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    layer.smart_filters.push(filter);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "layer.listSmartFilters" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            let layer = doc.layers.iter().find(|l| l.id == layer_id).context("layer not found")?;
            Ok(json!(layer.smart_filters.iter().map(smart_filter_to_json).collect::<Vec<_>>()))
        }
        "layer.removeSmartFilter" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let index = args.get("index").and_then(Value::as_u64).context("missing 'index'")? as usize;
            let removed = store
                .mutate(doc_id, move |doc| -> Result<bool> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    if index >= layer.smart_filters.len() {
                        return Ok(false);
                    }
                    layer.smart_filters.remove(index);
                    Ok(true)
                })
                .context("document not found")??;
            Ok(json!({"removed": removed}))
        }
        "transform.apply" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let tx = args.get("translateX").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let ty = args.get("translateY").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let rotate_degrees = args.get("rotateDegrees").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let sx = args.get("scaleX").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            let sy = args.get("scaleY").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            let skew_x = args.get("skewX").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let skew_y = args.get("skewY").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    if tx != 0.0 || ty != 0.0 {
                        agenticart_core::transform::translate(layer, tx, ty);
                    }
                    if rotate_degrees != 0.0 {
                        agenticart_core::transform::rotate(layer, rotate_degrees);
                    }
                    if sx != 1.0 || sy != 1.0 {
                        agenticart_core::transform::scale(layer, sx, sy);
                    }
                    if skew_x != 0.0 || skew_y != 0.0 {
                        agenticart_core::transform::skew(layer, skew_x, skew_y);
                    }
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "transform.perspective" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let parse_point = |field: &str| -> Result<(f32, f32)> {
                let arr = args.get(field).and_then(Value::as_array).with_context(|| format!("missing '{field}'"))?;
                if arr.len() != 2 {
                    bail!("'{field}' must be [x, y]");
                }
                let x = arr[0].as_f64().context("point x must be a number")? as f32;
                let y = arr[1].as_f64().context("point y must be a number")? as f32;
                Ok((x, y))
            };
            let top_left = parse_point("topLeft")?;
            let top_right = parse_point("topRight")?;
            let bottom_right = parse_point("bottomRight")?;
            let bottom_left = parse_point("bottomLeft")?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::transform::perspective(layer, top_left, top_right, bottom_right, bottom_left)
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "transform.place3DPlane" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let pitch = args.get("pitchDegrees").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let yaw = args.get("yawDegrees").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let camera_distance = args.get("cameraDistance").and_then(Value::as_f64).unwrap_or(1000.0) as f32;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    agenticart_core::transform::place_as_3d_plane(layer, pitch, yaw, camera_distance)
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "document.crop" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let x = args.get("x").and_then(Value::as_i64).context("missing 'x'")?;
            let y = args.get("y").and_then(Value::as_i64).context("missing 'y'")?;
            let width = args.get("width").and_then(Value::as_u64).context("missing 'width'")? as u32;
            let height = args.get("height").and_then(Value::as_u64).context("missing 'height'")? as u32;
            store
                .mutate(doc_id, move |doc| agenticart_core::resize::resize_canvas(doc, width, height, -x, -y))
                .context("document not found")?;
            Ok(json!({"ok": true}))
        }
        "layer.mergeDown" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let result = store.mutate(doc_id, move |doc| doc.merge_down(layer_id)).context("document not found")?;
            match result {
                Some(surviving_id) => Ok(json!({"ok": true, "layerId": surviving_id.to_string()})),
                None => Ok(json!({"ok": false})),
            }
        }
        "document.flatten" => {
            let doc_id = parse_uuid(args, "documentId")?;
            store.mutate(doc_id, agenticart_core::compositor::flatten).context("document not found")?;
            Ok(json!({"ok": true}))
        }
        "document.createLayerComp" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let name = args.get("name").and_then(Value::as_str).unwrap_or("Comp").to_string();
            let comp_id = store.mutate(doc_id, move |doc| doc.create_layer_comp(name)).context("document not found")?;
            Ok(json!({"compId": comp_id.to_string()}))
        }
        "document.applyLayerComp" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let comp_id = parse_uuid(args, "compId")?;
            let ok = store.mutate(doc_id, move |doc| doc.apply_layer_comp(comp_id)).context("document not found")?;
            Ok(json!({"ok": ok}))
        }
        "document.listLayerComps" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            Ok(json!(doc
                .layer_comps
                .iter()
                .map(|c| json!({"id": c.id.to_string(), "name": c.name, "layerCount": c.entries.len()}))
                .collect::<Vec<_>>()))
        }
        "document.deleteLayerComp" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let comp_id = parse_uuid(args, "compId")?;
            let deleted = store.mutate(doc_id, move |doc| doc.delete_layer_comp(comp_id)).context("document not found")?;
            Ok(json!({"deleted": deleted}))
        }
        "layer.setVectorMask" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let mut path = parse_path(args)?;
            path.closed = true; // a mask is always a closed region
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    let (w, h) = layer.pixels.dimensions();
                    layer.mask = Some(agenticart_core::path::rasterize_mask(&path, w, h));
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "layer.setMask" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let b64 = args.get("gray8Base64").and_then(Value::as_str).context("missing 'gray8Base64'")?;
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64)?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    let expected = (layer.pixels.width() * layer.pixels.height()) as usize;
                    if bytes.len() != expected {
                        bail!("expected {expected} bytes, got {}", bytes.len());
                    }
                    let mask = image::GrayImage::from_raw(layer.pixels.width(), layer.pixels.height(), bytes)
                        .context("failed to build mask buffer")?;
                    layer.mask = Some(mask);
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "layer.maskFromSelection" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let selection = doc.selection.context("no active selection - call selection.setRect first")?;
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
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
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "layer.getMask" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            let layer = doc.layers.iter().find(|l| l.id == layer_id).context("layer not found")?;
            match &layer.mask {
                Some(mask) => Ok(json!({
                    "hasMask": true,
                    "width": mask.width(),
                    "height": mask.height(),
                    "gray8Base64": base64::engine::general_purpose::STANDARD.encode(mask.as_raw())
                })),
                None => Ok(json!({"hasMask": false})),
            }
        }
        "layer.clearMask" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    layer.mask = None;
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "plugin.runFilter" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let plugin_path = args.get("pluginPath").and_then(Value::as_str).context("missing 'pluginPath'")?;
            let wasm_bytes = std::fs::read(plugin_path).with_context(|| format!("reading plugin file '{plugin_path}'"))?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    let host = agenticart_core::plugin::PluginHost::new()?;
                    let mut plugin = host.load(&wasm_bytes)?;
                    plugin.run_filter(&mut layer.pixels)?;
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "canvas.render" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            let format_str = args.get("format").and_then(Value::as_str).unwrap_or("png");
            let format = match format_str {
                "jpeg" | "jpg" => image::ImageFormat::Jpeg,
                _ => image::ImageFormat::Png,
            };
            let bytes = agenticart_core::io::render_to_bytes(&doc, format)?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            Ok(json!({
                "imageBase64": b64,
                "mimeType": if matches!(format, image::ImageFormat::Jpeg) { "image/jpeg" } else { "image/png" }
            }))
        }
        "canvas.renderTile" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            let x = args.get("x").and_then(Value::as_i64).context("missing 'x'")?;
            let y = args.get("y").and_then(Value::as_i64).context("missing 'y'")?;
            let width = args.get("width").and_then(Value::as_u64).context("missing 'width'")? as u32;
            let height = args.get("height").and_then(Value::as_u64).context("missing 'height'")? as u32;
            let format_str = args.get("format").and_then(Value::as_str).unwrap_or("png");
            let format = match format_str {
                "jpeg" | "jpg" => image::ImageFormat::Jpeg,
                _ => image::ImageFormat::Png,
            };
            let region = agenticart_core::SelectionRect { x, y, width, height };
            let bytes = agenticart_core::io::render_region_to_bytes(&doc, &region, format)?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            Ok(json!({
                "imageBase64": b64,
                "mimeType": if matches!(format, image::ImageFormat::Jpeg) { "image/jpeg" } else { "image/png" }
            }))
        }
        "color.eyedropper" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let x = args.get("x").and_then(Value::as_u64).context("missing 'x'")? as u32;
            let y = args.get("y").and_then(Value::as_u64).context("missing 'y'")? as u32;
            let doc = store.get_clone(doc_id).context("document not found")?;
            let source = match args.get("layerId").and_then(Value::as_str) {
                Some(id_str) => {
                    let layer_id = Uuid::parse_str(id_str).map_err(|_| anyhow!("'layerId' is not a valid id"))?;
                    doc.layers.iter().find(|l| l.id == layer_id).context("layer not found")?.pixels.clone()
                }
                None => agenticart_core::compositor::render(&doc),
            };
            if x >= source.width() || y >= source.height() {
                bail!("({x}, {y}) is outside the {}x{} canvas", source.width(), source.height());
            }
            let p = source.get_pixel(x, y);
            Ok(json!({"color": [p[0], p[1], p[2], p[3]]}))
        }
        "canvas.getPixels" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            let layer = doc.layers.iter().find(|l| l.id == layer_id).context("layer not found")?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(layer.pixels.as_raw());
            Ok(json!({
                "width": layer.pixels.width(),
                "height": layer.pixels.height(),
                "rgba8Base64": b64
            }))
        }
        "canvas.setPixels" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let layer_id = parse_uuid(args, "layerId")?;
            let b64 = args.get("rgba8Base64").and_then(Value::as_str).context("missing 'rgba8Base64'")?;
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64)?;
            store
                .mutate(doc_id, move |doc| -> Result<()> {
                    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
                    let expected = (layer.pixels.width() * layer.pixels.height() * 4) as usize;
                    if bytes.len() != expected {
                        bail!("expected {expected} bytes, got {}", bytes.len());
                    }
                    let buf = image::RgbaImage::from_raw(layer.pixels.width(), layer.pixels.height(), bytes)
                        .context("failed to build image buffer")?;
                    layer.pixels = buf;
                    Ok(())
                })
                .context("document not found")??;
            Ok(json!({"ok": true}))
        }
        "export.toFile" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let path = args.get("path").and_then(Value::as_str).context("missing 'path'")?;
            let doc = store.get_clone(doc_id).context("document not found")?;
            agenticart_core::io::export_to_file(&doc, &PathBuf::from(path))?;
            Ok(json!({"ok": true, "path": path}))
        }
        "history.undo" => {
            let doc_id = parse_uuid(args, "documentId")?;
            Ok(json!({"undone": store.undo(doc_id)}))
        }
        "history.redo" => {
            let doc_id = parse_uuid(args, "documentId")?;
            Ok(json!({"redone": store.redo(doc_id)}))
        }
        "history.list" => {
            let doc_id = parse_uuid(args, "documentId")?;
            let (undo_depth, redo_depth) = store.history_depth(doc_id).context("document not found")?;
            Ok(json!({"undoDepth": undo_depth, "redoDepth": redo_depth}))
        }
        "selection.selectAll" => {
            let doc_id = parse_uuid(args, "documentId")?;
            store
                .mutate(doc_id, |doc| {
                    doc.selection = Some(agenticart_core::SelectionRect { x: 0, y: 0, width: doc.width, height: doc.height });
                })
                .context("document not found")?;
            Ok(json!({"ok": true}))
        }
        other => bail!("unknown tool '{other}'"),
    }
}

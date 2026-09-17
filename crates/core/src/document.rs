use image::{GrayImage, RgbaImage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Non-destructive layer effect, computed live at render time (see
/// `compositor::effective_pixels`) rather than baked into `Layer::pixels` —
/// re-editable and removable at any time, same spirit as Photoshop's layer
/// styles. Phase 1 ships drop shadow only; stroke/glow/bevel follow the
/// same pattern.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DropShadowStyle {
    pub color: [u8; 4],
    pub offset_x: i32,
    pub offset_y: i32,
    pub blur_radius: f32,
    pub opacity: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StrokeStyle {
    pub color: [u8; 4],
    pub width: u32,
    pub opacity: f32,
}

/// Outer glow: like a drop shadow with no offset - a blurred halo of
/// `color` radiating outward from the layer's opaque edges, rendered
/// underneath the layer's own content.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OuterGlowStyle {
    pub color: [u8; 4],
    pub radius: f32,
    pub opacity: f32,
}

/// Inner Bevel (Photoshop's default Bevel & Emboss style): a pseudo-3D
/// highlight/shadow painted along the layer's opaque edges, simulating a
/// raised surface lit from `angle_degrees` (0 = from the right, increasing
/// counter-clockwise, matching Photoshop's own angle dial). `depth`
/// controls how wide the beveled edge reads (it's the blur radius used to
/// turn the hard alpha edge into a soft height map before computing the
/// lighting gradient from it).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BevelEmbossStyle {
    pub depth: f32,
    pub angle_degrees: f32,
    pub highlight_color: [u8; 4],
    pub shadow_color: [u8; 4],
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayerStyle {
    pub drop_shadow: Option<DropShadowStyle>,
    pub stroke: Option<StrokeStyle>,
    pub outer_glow: Option<OuterGlowStyle>,
    pub bevel_emboss: Option<BevelEmbossStyle>,
}

/// One entry in a layer's non-destructive smart-filter stack (see
/// `Layer::smart_filters`). Applied in order, live at render time -
/// re-editable, reorderable, and removable without ever touching
/// `Layer::pixels`, same non-destructive spirit as `LayerStyle`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SmartFilter {
    GaussianBlur { radius: f32 },
    Sharpen { radius: f32, threshold: i32 },
    BrightnessContrast { brightness: f32, contrast: f32 },
    HueSaturation { hue: f32, saturation: f32, lightness: f32 },
    Invert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    // Photoshop's full blend-mode set, added alongside the original 12:
    // every deterministic mode except Dissolve, which is stochastic and
    // doesn't fit a deterministic compositor test suite.
    LinearBurn,
    DarkerColor,
    LinearDodge,
    LighterColor,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl Default for BlendMode {
    fn default() -> Self {
        BlendMode::Normal
    }
}

impl BlendMode {
    /// Canonical camelCase wire name, shared by the MCP server and the
    /// Tauri UI commands so both front-ends agree on one vocabulary.
    pub fn as_str(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
            BlendMode::ColorDodge => "colorDodge",
            BlendMode::ColorBurn => "colorBurn",
            BlendMode::HardLight => "hardLight",
            BlendMode::SoftLight => "softLight",
            BlendMode::Difference => "difference",
            BlendMode::Exclusion => "exclusion",
            BlendMode::LinearBurn => "linearBurn",
            BlendMode::DarkerColor => "darkerColor",
            BlendMode::LinearDodge => "linearDodge",
            BlendMode::LighterColor => "lighterColor",
            BlendMode::VividLight => "vividLight",
            BlendMode::LinearLight => "linearLight",
            BlendMode::PinLight => "pinLight",
            BlendMode::HardMix => "hardMix",
            BlendMode::Subtract => "subtract",
            BlendMode::Divide => "divide",
            BlendMode::Hue => "hue",
            BlendMode::Saturation => "saturation",
            BlendMode::Color => "color",
            BlendMode::Luminosity => "luminosity",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "normal" => BlendMode::Normal,
            "multiply" => BlendMode::Multiply,
            "screen" => BlendMode::Screen,
            "overlay" => BlendMode::Overlay,
            "darken" => BlendMode::Darken,
            "lighten" => BlendMode::Lighten,
            "colorDodge" => BlendMode::ColorDodge,
            "colorBurn" => BlendMode::ColorBurn,
            "hardLight" => BlendMode::HardLight,
            "softLight" => BlendMode::SoftLight,
            "difference" => BlendMode::Difference,
            "exclusion" => BlendMode::Exclusion,
            "linearBurn" => BlendMode::LinearBurn,
            "darkerColor" => BlendMode::DarkerColor,
            "linearDodge" => BlendMode::LinearDodge,
            "lighterColor" => BlendMode::LighterColor,
            "vividLight" => BlendMode::VividLight,
            "linearLight" => BlendMode::LinearLight,
            "pinLight" => BlendMode::PinLight,
            "hardMix" => BlendMode::HardMix,
            "subtract" => BlendMode::Subtract,
            "divide" => BlendMode::Divide,
            "hue" => BlendMode::Hue,
            "saturation" => BlendMode::Saturation,
            "color" => BlendMode::Color,
            "luminosity" => BlendMode::Luminosity,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub id: Uuid,
    pub name: String,
    /// Full-canvas-sized RGBA8 raster buffer. Later phases move this to a
    /// tiled/virtualized store; every layer is canvas-sized for now to keep
    /// compositing simple.
    pub pixels: RgbaImage,
    pub opacity: f32,
    pub visible: bool,
    pub blend_mode: BlendMode,
    /// When true, this layer is only visible where the composite *below*
    /// it already has coverage (Photoshop's "clipping mask"). This engine
    /// has no layer-group concept yet, so the clip is against the
    /// cumulative composite so far rather than a specific "base" layer —
    /// matches Photoshop exactly for the common single-base-layer case,
    /// approximates it otherwise.
    pub clip_to_below: bool,
    pub style: LayerStyle,
    /// Non-destructive filter stack, applied in order at render time (see
    /// `compositor::effective_pixels`) before layer styles and never baked
    /// into `pixels` - reorderable/removable/re-editable at any time.
    pub smart_filters: Vec<SmartFilter>,
    /// Non-destructive raster layer mask: canvas-sized 8-bit grayscale,
    /// white = fully visible, black = fully hidden. Multiplied into the
    /// layer's alpha at render time (after smart filters, before layer
    /// styles - matches Photoshop, where a drop shadow follows the masked
    /// silhouette). `None` means "fully visible", the common case, so it
    /// costs nothing until a mask is actually added.
    pub mask: Option<GrayImage>,
    /// When set, this layer is a live text layer: its rendered content
    /// comes from re-rendering this data at composite time
    /// (`compositor::text_source_pixels`), not from `pixels` (which is
    /// unused, same convention as a group's `pixels`).
    pub text: Option<TextLayerData>,
    /// True for a group ("folder") marker layer. A group's own `pixels`
    /// are unused; its rendered content is its children (see
    /// `Document::group_children`) composited together, with the group's
    /// own opacity/blend_mode/clip_to_below/style then applied to that
    /// result exactly as for a normal layer. Nested groups (a group whose
    /// child is itself a group) are not supported in this first version.
    pub is_group: bool,
    /// The group this layer belongs to, if any. Group members are kept
    /// contiguous in `Document::layers` by every operation that adds to a
    /// group; nothing currently *enforces* that invariant against an
    /// arbitrary `reorder_layer` call, so a careless reorder of a grouped
    /// layer to a distant index can break contiguity - a known limitation.
    pub parent_group: Option<Uuid>,
}

impl Layer {
    pub fn new_transparent(name: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            pixels: RgbaImage::new(width, height),
            opacity: 1.0,
            visible: true,
            blend_mode: BlendMode::Normal,
            clip_to_below: false,
            style: LayerStyle::default(),
            smart_filters: Vec::new(),
            mask: None,
            text: None,
            is_group: false,
            parent_group: None,
        }
    }

    pub fn duplicate(&self) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: format!("{} copy", self.name),
            pixels: self.pixels.clone(),
            opacity: self.opacity,
            visible: self.visible,
            blend_mode: self.blend_mode,
            clip_to_below: self.clip_to_below,
            style: self.style.clone(),
            smart_filters: self.smart_filters.clone(),
            mask: self.mask.clone(),
            text: self.text.clone(),
            is_group: self.is_group,
            parent_group: self.parent_group,
        }
    }
}

/// Rectangular selection in canvas pixel coordinates. Phase 1 keeps
/// selections axis-aligned; arbitrary-path (lasso/vector) selections are a
/// later addition that swaps this for a mask buffer without changing how
/// callers ask "is this pixel selected".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionRect {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

impl SelectionRect {
    pub fn contains(&self, px: i64, py: i64) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.width as i64 && py < self.y + self.height as i64
    }
}

/// Live, non-destructive text layer content (Photoshop's real text
/// layers, as opposed to `text::draw_text`'s one-shot raster stamp): the
/// string and its render params are stored, not baked pixels, so editing
/// `text` and re-rendering (`compositor::text_source_pixels`) is always
/// possible - a Photoshop-parity gap this closes without touching the
/// existing raster-stamp `text.draw` tool, which remains available for a
/// one-shot "just paint these pixels" use case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextLayerData {
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub size: f32,
    pub color: [u8; 4],
}

/// One layer's captured state within a `LayerComp` - Photoshop's "Layer
/// Comps" feature: a named snapshot of visibility/opacity/position an
/// agent or user can flip between (e.g. comparing design variants)
/// without actually editing the document, just recalling a prior state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerCompEntry {
    pub layer_id: Uuid,
    pub visible: bool,
    pub opacity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerComp {
    pub id: Uuid,
    pub name: String,
    pub entries: Vec<LayerCompEntry>,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub id: Uuid,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub layers: Vec<Layer>,
    pub active_layer: usize,
    pub selection: Option<SelectionRect>,
    pub layer_comps: Vec<LayerComp>,
}

impl Document {
    pub fn new(name: impl Into<String>, width: u32, height: u32) -> Self {
        let mut doc = Self {
            id: Uuid::new_v4(),
            name: name.into(),
            width,
            height,
            layers: Vec::new(),
            active_layer: 0,
            selection: None,
            layer_comps: Vec::new(),
        };
        doc.layers.push(Layer::new_transparent("Background", width, height));
        doc
    }

    pub fn active_layer_mut(&mut self) -> Option<&mut Layer> {
        self.layers.get_mut(self.active_layer)
    }

    pub fn find_layer_mut(&mut self, layer_id: Uuid) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == layer_id)
    }

    pub fn find_layer_index(&self, layer_id: Uuid) -> Option<usize> {
        self.layers.iter().position(|l| l.id == layer_id)
    }

    pub fn duplicate_layer(&mut self, layer_id: Uuid) -> Option<Uuid> {
        let idx = self.find_layer_index(layer_id)?;
        let copy = self.layers[idx].duplicate();
        let new_id = copy.id;
        self.layers.insert(idx + 1, copy);
        Some(new_id)
    }

    /// Deletes a layer. Deleting a group deletes its children with it
    /// (matches Photoshop's default "delete group" behavior - use
    /// `ungroup` first if the contents should survive).
    pub fn delete_layer(&mut self, layer_id: Uuid) -> bool {
        let Some(idx) = self.find_layer_index(layer_id) else { return false };
        let is_group = self.layers[idx].is_group;
        let removed_count = if is_group { self.layers.iter().filter(|l| l.id == layer_id || l.parent_group == Some(layer_id)).count() } else { 1 };
        if self.layers.len() <= removed_count {
            return false;
        }
        if is_group {
            self.layers.retain(|l| l.id != layer_id && l.parent_group != Some(layer_id));
        } else {
            self.layers.remove(idx);
        }
        if self.active_layer >= self.layers.len() {
            self.active_layer = self.layers.len() - 1;
        }
        true
    }

    /// Moves a layer to `new_index` in the stack (0 = bottom). Does not
    /// enforce group contiguity - see the `parent_group` doc comment.
    pub fn reorder_layer(&mut self, layer_id: Uuid, new_index: usize) -> bool {
        let Some(idx) = self.find_layer_index(layer_id) else { return false };
        let new_index = new_index.min(self.layers.len() - 1);
        let layer = self.layers.remove(idx);
        self.layers.insert(new_index, layer);
        true
    }

    /// Creates a new empty group ("folder") at the top of the stack and
    /// returns its id.
    pub fn create_group(&mut self, name: impl Into<String>) -> Uuid {
        let mut group = Layer::new_transparent(name, self.width, self.height);
        group.is_group = true;
        let id = group.id;
        self.layers.push(group);
        self.active_layer = self.layers.len() - 1;
        id
    }

    /// Moves an existing layer into a group, keeping the group's members
    /// contiguous in the stack. Fails if `group_id` doesn't name a group,
    /// or `layer_id` doesn't exist, or they're the same layer.
    pub fn move_layer_into_group(&mut self, layer_id: Uuid, group_id: Uuid) -> bool {
        if layer_id == group_id {
            return false;
        }
        if !self.layers.iter().any(|l| l.id == group_id && l.is_group) {
            return false;
        }
        let Some(idx) = self.find_layer_index(layer_id) else { return false };
        let mut layer = self.layers.remove(idx);
        layer.parent_group = Some(group_id);
        let group_idx = self.layers.iter().position(|l| l.id == group_id).expect("group present, checked above");
        self.layers.insert(group_idx, layer);
        true
    }

    /// Removes a layer from its group, leaving it in place as a top-level
    /// layer (physical stack position is unchanged).
    pub fn ungroup_layer(&mut self, layer_id: Uuid) -> bool {
        let Some(layer) = self.find_layer_mut(layer_id) else { return false };
        if layer.parent_group.is_none() {
            return false;
        }
        layer.parent_group = None;
        true
    }

    /// The direct children of a group, in stack order (bottom to top).
    pub fn group_children(&self, group_id: Uuid) -> Vec<&Layer> {
        self.layers.iter().filter(|l| l.parent_group == Some(group_id)).collect()
    }

    /// Snapshots every layer's current visibility/opacity into a new named
    /// `LayerComp` an agent or user can recall later with `apply_layer_comp`
    /// - Photoshop's "New Layer Comp".
    pub fn create_layer_comp(&mut self, name: impl Into<String>) -> Uuid {
        let entries = self.layers.iter().map(|l| LayerCompEntry { layer_id: l.id, visible: l.visible, opacity: l.opacity }).collect();
        let id = Uuid::new_v4();
        self.layer_comps.push(LayerComp { id, name: name.into(), entries });
        id
    }

    /// Restores the visibility/opacity every layer had when `comp_id` was
    /// captured. Entries whose layer no longer exists (deleted since) are
    /// silently skipped rather than treated as an error - the comp is
    /// still meaningfully applicable to the layers that remain.
    pub fn apply_layer_comp(&mut self, comp_id: Uuid) -> bool {
        let Some(comp) = self.layer_comps.iter().find(|c| c.id == comp_id) else { return false };
        let entries = comp.entries.clone();
        for entry in entries {
            if let Some(layer) = self.find_layer_mut(entry.layer_id) {
                layer.visible = entry.visible;
                layer.opacity = entry.opacity;
            }
        }
        true
    }

    pub fn delete_layer_comp(&mut self, comp_id: Uuid) -> bool {
        let before = self.layer_comps.len();
        self.layer_comps.retain(|c| c.id != comp_id);
        self.layer_comps.len() != before
    }

    /// Photoshop's "Merge Down": bakes `layer_id` onto the layer directly
    /// beneath it in the stack (`crate::compositor::merge_two_layers`,
    /// which respects both layers' smart filters/masks/styles), replaces
    /// the surviving (lower) layer's pixels with the result, and removes
    /// `layer_id`. Returns the surviving layer's id, or `None` if there is
    /// nothing to merge into: `layer_id` is the bottom of its stack
    /// (index 0, or the first layer in its group), the layer immediately
    /// below belongs to a different group, or either layer is a group
    /// marker itself (merging groups isn't supported - ungroup first).
    pub fn merge_down(&mut self, layer_id: Uuid) -> Option<Uuid> {
        let idx = self.find_layer_index(layer_id)?;
        if idx == 0 {
            return None;
        }
        let below_idx = idx - 1;
        if self.layers[idx].is_group || self.layers[below_idx].is_group {
            return None;
        }
        if self.layers[idx].parent_group != self.layers[below_idx].parent_group {
            return None;
        }

        let merged_pixels = crate::compositor::merge_two_layers(&self.layers[below_idx], &self.layers[idx]);
        let below = &mut self.layers[below_idx];
        below.pixels = merged_pixels;
        below.smart_filters.clear();
        below.mask = None;
        below.style = LayerStyle::default();
        let below_id = below.id;

        self.layers.remove(idx);
        if self.active_layer >= self.layers.len() {
            self.active_layer = self.layers.len() - 1;
        }
        Some(below_id)
    }
}

use crate::document::{BlendMode, Document};
use anyhow::Result;
use image::RgbaImage;
use std::path::Path;

/// Writes a Document to a .psd file (uncompressed/raw channel data, RGB
/// 8-bit). This is a real, if intentionally scoped, PSD writer - not a
/// re-export of our native project format under a different extension:
///
/// - Each layer (including nested ones) becomes a real PSD layer with its
///   own R/G/B/A channel data, name, opacity, blend mode, and visibility.
/// - Groups export as genuine PSD layer groups (the "lsct" section-divider
///   structure Photoshop itself uses - a bounding divider, the member
///   layers, then a folder-header layer), not flattened composites. This
///   was a deliberate upgrade over an earlier version that flattened
///   groups, once the independent `psd` crate reader was confirmed to
///   parse real PSD groups (its `PsdGroup`/`group_id()` API) - see the
///   round-trip test.
/// - Non-destructive layer styles (drop shadow/stroke) are NOT baked into
///   the exported pixels - a layer with a style exports its plain,
///   un-styled pixels. A documented simplification, not silent data loss.
/// - The required merged/composite image data is the same flattened
///   render `export.toFile` would produce.
///
/// Layer visibility uses flags-byte bit 1 (1 = visible), confirmed
/// against the independent `psd` crate's own parser source (this session
/// has no Photoshop install to check against directly) and covered by a
/// round-trip test in the test suite.
pub fn document_to_psd(doc: &Document, path: &Path) -> Result<()> {
    let bytes = encode_psd(doc)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

fn blend_mode_key(mode: BlendMode) -> &'static [u8; 4] {
    match mode {
        BlendMode::Normal => b"norm",
        BlendMode::Multiply => b"mul ",
        BlendMode::Screen => b"scrn",
        BlendMode::Overlay => b"over",
        BlendMode::Darken => b"dark",
        BlendMode::Lighten => b"lite",
        BlendMode::ColorDodge => b"div ",
        BlendMode::ColorBurn => b"idiv",
        BlendMode::HardLight => b"hLit",
        BlendMode::SoftLight => b"sLit",
        BlendMode::Difference => b"diff",
        BlendMode::Exclusion => b"smud",
        BlendMode::LinearBurn => b"lbrn",
        BlendMode::DarkerColor => b"dkCl",
        BlendMode::LinearDodge => b"lddg",
        BlendMode::LighterColor => b"lgCl",
        BlendMode::VividLight => b"vLit",
        BlendMode::LinearLight => b"lLit",
        BlendMode::PinLight => b"pLit",
        BlendMode::HardMix => b"hMix",
        BlendMode::Subtract => b"fsub",
        BlendMode::Divide => b"fdiv",
        BlendMode::Hue => b"hue ",
        BlendMode::Saturation => b"sat ",
        BlendMode::Color => b"colr",
        BlendMode::Luminosity => b"lum ",
    }
}

/// One PSD layer record to emit. Content layers carry real canvas-sized
/// pixel data; group boundary/header entries are zero-size structural
/// markers (Photoshop's own convention) carrying an "lsct" section-divider
/// tag instead.
///
/// Ordering matters and is *not* what a naive reading of the spec prose
/// suggests: confirmed against the independent `psd` crate's own group-
/// assembly algorithm (it walks records bottom-to-top, treating an
/// OpenFolder/CloseFolder record as "push a new group frame" and a
/// BoundingSection record as "pop it"). So a group's OPEN/CLOSE-FOLDER
/// record (which supplies the group's *name*) comes first, then its
/// member layers, then a BoundingSection record (which supplies the
/// group's opacity/blend-mode/visibility) closes it out.
enum PsdEntry<'a> {
    Content { name: &'a str, opacity: f32, blend_mode: BlendMode, visible: bool, pixels: &'a RgbaImage },
    GroupOpen { name: &'a str },
    GroupClose { opacity: f32, blend_mode: BlendMode, visible: bool },
}

fn build_entries(doc: &Document) -> Vec<PsdEntry<'_>> {
    let mut entries = Vec::new();
    for layer in doc.layers.iter().filter(|l| l.parent_group.is_none()) {
        if layer.is_group {
            entries.push(PsdEntry::GroupOpen { name: &layer.name });
            for child in doc.layers.iter().filter(|c| c.parent_group == Some(layer.id)) {
                entries.push(PsdEntry::Content { name: &child.name, opacity: child.opacity, blend_mode: child.blend_mode, visible: child.visible, pixels: &child.pixels });
            }
            entries.push(PsdEntry::GroupClose { opacity: layer.opacity, blend_mode: layer.blend_mode, visible: layer.visible });
        } else {
            entries.push(PsdEntry::Content { name: &layer.name, opacity: layer.opacity, blend_mode: layer.blend_mode, visible: layer.visible, pixels: &layer.pixels });
        }
    }
    entries
}

fn encode_psd(doc: &Document) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();

    // --- File Header ---
    out.extend_from_slice(b"8BPS");
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&[0u8; 6]); // reserved
    out.extend_from_slice(&4u16.to_be_bytes()); // channels (R,G,B,A)
    out.extend_from_slice(&doc.height.to_be_bytes());
    out.extend_from_slice(&doc.width.to_be_bytes());
    out.extend_from_slice(&8u16.to_be_bytes()); // depth
    out.extend_from_slice(&3u16.to_be_bytes()); // color mode: RGB

    // --- Color Mode Data (empty for RGB) ---
    out.extend_from_slice(&0u32.to_be_bytes());

    // --- Image Resources (empty) ---
    out.extend_from_slice(&0u32.to_be_bytes());

    // --- Layer and Mask Information ---
    // `encode_layer_info` already returns the "Layer info" section complete
    // with its own leading 4-byte length prefix - do not wrap it in a
    // second one here (that was a real bug: it corrupted byte offsets for
    // any document with more than one layer, caught by the round-trip
    // test once a multi-layer document was tried).
    let layer_info = encode_layer_info(doc);
    let mut layer_mask_info = Vec::new();
    layer_mask_info.extend_from_slice(&layer_info);
    layer_mask_info.extend_from_slice(&0u32.to_be_bytes()); // global layer mask info: empty
    out.extend_from_slice(&(layer_mask_info.len() as u32).to_be_bytes());
    out.extend_from_slice(&layer_mask_info);

    // --- Merged Image Data (required composite) ---
    let composite = crate::compositor::render(doc);
    out.extend_from_slice(&0u16.to_be_bytes()); // compression: raw
    for channel in 0..4u8 {
        write_channel_plane(&mut out, &composite, channel);
    }

    Ok(out)
}

fn pascal_string_padded(name: &str) -> Vec<u8> {
    // Pascal string (1-byte length + bytes), then padded so the *total*
    // (length byte + bytes) is a multiple of 4.
    let bytes = name.as_bytes();
    let len = bytes.len().min(255);
    let mut out = Vec::with_capacity(len + 4);
    out.push(len as u8);
    out.extend_from_slice(&bytes[..len]);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

fn write_channel_plane(out: &mut Vec<u8>, img: &RgbaImage, channel: u8) {
    for p in img.pixels() {
        out.push(p[channel as usize]);
    }
}

/// Appends an "lsct" (Section Divider Setting) additional-layer-info block:
/// the mechanism real PSD group folders/boundaries use. `divider_type`: 1 =
/// open folder (used for our group header layers), 3 = bounding section
/// divider (marks a group's start in the stack).
fn append_lsct_block(records: &mut Vec<u8>, divider_type: i32) {
    records.extend_from_slice(b"8BIM");
    records.extend_from_slice(b"lsct");
    records.extend_from_slice(&12u32.to_be_bytes()); // data length: type(4) + sig(4) + blend key(4)
    records.extend_from_slice(&divider_type.to_be_bytes());
    records.extend_from_slice(b"8BIM");
    records.extend_from_slice(b"norm");
}

fn encode_layer_info(doc: &Document) -> Vec<u8> {
    // The independent `psd` crate reader reverses the on-disk record order
    // before processing it (`result.reverse()` in `read_layer_records`,
    // matching real Photoshop's bottom-to-top storage convention). So the
    // order we WRITE to disk must be the reverse of the order we want
    // `decode_layers` to actually see - otherwise a group's BoundingSection
    // (close) record is processed before its OpenFolder (open) record,
    // popping the parser's root stack frame and panicking on the next
    // layer. Confirmed by tracing that exact panic (stack.last().unwrap()
    // == None) back through read_layer_records's reverse() call.
    let mut entries = build_entries(doc);
    entries.reverse();

    let mut records = Vec::new();
    let mut channel_data = Vec::new();

    for entry in &entries {
        let (name, opacity, blend_mode, visible, pixels, has_lsct, lsct_type): (&str, f32, BlendMode, bool, Option<&RgbaImage>, bool, i32) = match entry {
            PsdEntry::Content { name, opacity, blend_mode, visible, pixels } => (name, *opacity, *blend_mode, *visible, Some(pixels), false, 0),
            PsdEntry::GroupOpen { name } => (name, 1.0, BlendMode::Normal, true, None, true, 1),
            PsdEntry::GroupClose { opacity, blend_mode, visible } => ("</Layer group>", *opacity, *blend_mode, *visible, None, true, 3),
        };

        let (width, height) = pixels.map(|p| p.dimensions()).unwrap_or((0, 0));
        records.extend_from_slice(&0i32.to_be_bytes()); // top
        records.extend_from_slice(&0i32.to_be_bytes()); // left
        records.extend_from_slice(&(height as i32).to_be_bytes()); // bottom
        records.extend_from_slice(&(width as i32).to_be_bytes()); // right

        records.extend_from_slice(&4u16.to_be_bytes()); // channel count
        let plane_len = (width as usize) * (height as usize) + 2; // +2 for compression marker
        for channel_id in [0i16, 1, 2, -1] {
            records.extend_from_slice(&channel_id.to_be_bytes());
            records.extend_from_slice(&(plane_len as u32).to_be_bytes());
        }

        records.extend_from_slice(b"8BIM");
        records.extend_from_slice(blend_mode_key(blend_mode));
        records.push((opacity.clamp(0.0, 1.0) * 255.0).round() as u8);
        records.push(0); // clipping: base
        records.push(if visible { 0x02 } else { 0x00 }); // flags: bit 1 set = visible
        records.push(0); // filler/padding byte

        let name_block = pascal_string_padded(name);
        let lsct_block_len = if has_lsct { 4 + 4 + 12 } else { 0 }; // "8BIM" + "lsct" + 4-byte-len + 12 bytes data
        let extra_len = 4 + 4 + name_block.len() + lsct_block_len; // mask(0) + blending-ranges(0) + name [+ lsct]
        records.extend_from_slice(&(extra_len as u32).to_be_bytes());
        records.extend_from_slice(&0u32.to_be_bytes()); // layer mask data: none
        records.extend_from_slice(&0u32.to_be_bytes()); // layer blending ranges: none
        records.extend_from_slice(&name_block);
        if has_lsct {
            append_lsct_block(&mut records, lsct_type);
        }

        for channel in 0..4u8 {
            channel_data.extend_from_slice(&0u16.to_be_bytes()); // compression: raw
            if let Some(p) = pixels {
                write_channel_plane(&mut channel_data, p, channel);
            }
        }
    }

    let mut layer_info = Vec::new();
    layer_info.extend_from_slice(&(entries.len() as i16).to_be_bytes());
    layer_info.extend_from_slice(&records);
    layer_info.extend_from_slice(&channel_data);
    if layer_info.len() % 2 != 0 {
        layer_info.push(0);
    }

    let mut with_len = Vec::new();
    with_len.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
    with_len.extend_from_slice(&layer_info);
    with_len
}

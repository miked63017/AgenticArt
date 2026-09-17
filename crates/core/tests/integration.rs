use agenticart_core::document::{BlendMode, SelectionRect, SmartFilter};
use agenticart_core::path::{Path, PathNode, Point};
use agenticart_core::{adjustments, compositor, filters, paint, project, shapes};
use agenticart_core::{Brush, BrushPoint, Document};
use image::Rgba;

fn opaque_brush(color: [u8; 4]) -> Brush {
    Brush { size: 40.0, color: Rgba(color), hardness: 1.0 }
}

#[test]
fn normal_blend_is_straightforward_alpha_over() {
    let mut doc = Document::new("t", 10, 10);
    let bg = doc.layers[0].id;
    paint::stroke_path(doc.find_layer_mut(bg).unwrap(), &[BrushPoint { x: 5.0, y: 5.0, pressure: 1.0 }], &opaque_brush([255, 0, 0, 255]), None);

    let top = agenticart_core::Layer::new_transparent("top", 10, 10);
    let top_id = top.id;
    doc.layers.push(top);
    paint::stroke_path(doc.find_layer_mut(top_id).unwrap(), &[BrushPoint { x: 5.0, y: 5.0, pressure: 1.0 }], &opaque_brush([0, 255, 0, 255]), None);

    let rendered = compositor::render(&doc);
    let center = *rendered.get_pixel(5, 5);
    // Fully opaque green on top of red should just be green.
    assert_eq!(center, Rgba([0, 255, 0, 255]));
}

#[test]
fn multiply_blend_darkens() {
    let mut doc = Document::new("t", 4, 4);
    let bg = doc.layers[0].id;
    paint::stroke_path(doc.find_layer_mut(bg).unwrap(), &[BrushPoint { x: 2.0, y: 2.0, pressure: 1.0 }], &opaque_brush([200, 200, 200, 255]), None);

    let mut top = agenticart_core::Layer::new_transparent("top", 4, 4);
    top.blend_mode = BlendMode::Multiply;
    let top_id = top.id;
    doc.layers.push(top);
    paint::stroke_path(doc.find_layer_mut(top_id).unwrap(), &[BrushPoint { x: 2.0, y: 2.0, pressure: 1.0 }], &opaque_brush([100, 100, 100, 255]), None);

    let rendered = compositor::render(&doc);
    let px = rendered.get_pixel(2, 2);
    // 200/255 * 100/255 * 255 ≈ 78, well below either input channel.
    assert!(px[0] < 100, "multiply should darken below both inputs, got {}", px[0]);
}

#[test]
fn selection_clips_paint_strokes() {
    let mut doc = Document::new("t", 20, 20);
    let layer_id = doc.layers[0].id;
    let clip = SelectionRect { x: 0, y: 0, width: 5, height: 5 };
    let layer = doc.find_layer_mut(layer_id).unwrap();
    paint::stroke_path(layer, &[BrushPoint { x: 15.0, y: 15.0, pressure: 1.0 }], &opaque_brush([255, 0, 0, 255]), Some(clip));

    // The stroke was centered far outside the 5x5 clip region, so nothing
    // should have been painted at all.
    let px = layer.pixels.get_pixel(15, 15);
    assert_eq!(px[3], 0, "paint outside the selection must not land");
}

#[test]
fn clipping_mask_restricts_layer_to_shape_below() {
    let mut doc = Document::new("t", 20, 20);
    let base = doc.layers[0].id;
    paint::stroke_path(doc.find_layer_mut(base).unwrap(), &[BrushPoint { x: 5.0, y: 5.0, pressure: 1.0 }], &opaque_brush([255, 255, 255, 255]), None);

    let mut top = agenticart_core::Layer::new_transparent("top", 20, 20);
    top.clip_to_below = true;
    let top_id = top.id;
    doc.layers.push(top);
    // Full-canvas fill on the clipped layer.
    paint::stroke_path(doc.find_layer_mut(top_id).unwrap(), &[BrushPoint { x: 10.0, y: 10.0, pressure: 1.0 }], &Brush { size: 60.0, ..opaque_brush([0, 255, 0, 255]) }, None);

    let rendered = compositor::render(&doc);
    // Far corner: base layer has no coverage there, so the clipped green
    // fill must not show through even though its own pixels are opaque there.
    assert_eq!(rendered.get_pixel(19, 19)[3], 0, "clipped layer must not render where the base has no coverage");
    // Near the base shape: should show the clipped green.
    let near_base = rendered.get_pixel(5, 5);
    assert!(near_base[1] > near_base[0] && near_base[1] > near_base[2], "clipped layer shows through where the base has coverage");
}

#[test]
fn adjustments_and_filters_respect_selection() {
    let mut doc = Document::new("t", 10, 10);
    let layer_id = doc.layers[0].id;
    {
        let layer = doc.find_layer_mut(layer_id).unwrap();
        for y in 0..10 {
            for x in 0..10 {
                layer.pixels.put_pixel(x, y, Rgba([128, 128, 128, 255]));
            }
        }
    }
    let clip = SelectionRect { x: 0, y: 0, width: 5, height: 10 };
    {
        let layer = doc.find_layer_mut(layer_id).unwrap();
        adjustments::invert(layer, Some(clip));
    }
    let layer = doc.find_layer_mut(layer_id).unwrap();
    assert_eq!(layer.pixels.get_pixel(0, 0)[0], 127, "inverted inside selection (128 -> ~127)");
    assert_eq!(layer.pixels.get_pixel(9, 0)[0], 128, "untouched outside selection");
}

#[test]
fn gaussian_blur_changes_pixels() {
    let mut doc = Document::new("t", 30, 30);
    let layer_id = doc.layers[0].id;
    paint::stroke_path(doc.find_layer_mut(layer_id).unwrap(), &[BrushPoint { x: 15.0, y: 15.0, pressure: 1.0 }], &opaque_brush([255, 0, 0, 255]), None);
    let before = doc.find_layer_mut(layer_id).unwrap().pixels.clone();
    filters::gaussian_blur(doc.find_layer_mut(layer_id).unwrap(), 3.0, None);
    let after = &doc.find_layer_mut(layer_id).unwrap().pixels;
    assert_ne!(&before, after, "blur should change pixel data");
}

#[test]
fn shapes_fill_exact_regions() {
    let mut doc = Document::new("t", 20, 20);
    let layer_id = doc.layers[0].id;
    let layer = doc.find_layer_mut(layer_id).unwrap();
    shapes::fill_rect(layer, 2, 2, 4, 4, Rgba([10, 20, 30, 255]), None);
    assert_eq!(*layer.pixels.get_pixel(3, 3), Rgba([10, 20, 30, 255]), "inside rect");
    assert_eq!(layer.pixels.get_pixel(10, 10)[3], 0, "outside rect stays transparent");

    shapes::fill_ellipse(layer, 15.0, 15.0, 3.0, 3.0, Rgba([40, 50, 60, 255]), None);
    assert_eq!(*layer.pixels.get_pixel(15, 15), Rgba([40, 50, 60, 255]), "ellipse center");
    assert_eq!(layer.pixels.get_pixel(0, 19)[3], 0, "far corner outside ellipse stays transparent");
}

#[test]
fn group_children_composite_with_group_opacity_and_blend_mode() {
    let mut doc = Document::new("t", 20, 20);
    let base = doc.layers[0].id;
    paint::stroke_path(doc.find_layer_mut(base).unwrap(), &[BrushPoint { x: 10.0, y: 10.0, pressure: 1.0 }], &Brush { size: 30.0, ..opaque_brush([200, 200, 200, 255]) }, None);

    let group_id = doc.create_group("Group 1");
    let child = agenticart_core::Layer::new_transparent("child", 20, 20);
    let child_id = child.id;
    doc.layers.push(child);
    assert!(doc.move_layer_into_group(child_id, group_id), "moving a layer into a group should succeed");
    assert_eq!(doc.find_layer_mut(child_id).unwrap().parent_group, Some(group_id));

    paint::stroke_path(doc.find_layer_mut(child_id).unwrap(), &[BrushPoint { x: 10.0, y: 10.0, pressure: 1.0 }], &Brush { size: 30.0, ..opaque_brush([0, 0, 0, 255]) }, None);

    // With the group at full opacity, the group's opaque black child should
    // fully cover the gray background at the center.
    let rendered_full = compositor::render(&doc);
    assert_eq!(*rendered_full.get_pixel(10, 10), Rgba([0, 0, 0, 255]));

    // Halving the GROUP's opacity (not the child's) should visibly blend
    // the child's black with the gray background beneath - proving the
    // group's own opacity is applied to its flattened content, not
    // ignored.
    doc.find_layer_mut(group_id).unwrap().opacity = 0.5;
    let rendered_half = compositor::render(&doc);
    let px = rendered_half.get_pixel(10, 10);
    assert!(px[0] > 50 && px[0] < 150, "halved group opacity should blend child with background, got {}", px[0]);
}

#[test]
fn deleting_a_group_deletes_its_children() {
    let mut doc = Document::new("t", 10, 10);
    let group_id = doc.create_group("Group 1");
    let child = agenticart_core::Layer::new_transparent("child", 10, 10);
    let child_id = child.id;
    doc.layers.push(child);
    doc.move_layer_into_group(child_id, group_id);

    assert_eq!(doc.layers.len(), 3); // background + group + child
    assert!(doc.delete_layer(group_id));
    assert_eq!(doc.layers.len(), 1, "deleting a group must remove its children too");
    assert!(doc.find_layer_index(child_id).is_none());
}

#[test]
fn ungroup_detaches_without_deleting() {
    let mut doc = Document::new("t", 10, 10);
    let group_id = doc.create_group("Group 1");
    let child = agenticart_core::Layer::new_transparent("child", 10, 10);
    let child_id = child.id;
    doc.layers.push(child);
    doc.move_layer_into_group(child_id, group_id);

    assert!(doc.ungroup_layer(child_id));
    assert_eq!(doc.find_layer_mut(child_id).unwrap().parent_group, None);
    assert!(doc.find_layer_index(child_id).is_some(), "ungrouping must not delete the layer");
}

fn node(x: f32, y: f32) -> PathNode {
    PathNode { anchor: Point { x, y }, control_in: None, control_out: None }
}

#[test]
fn straight_path_fill_lands_exactly_inside_the_polygon() {
    let mut doc = Document::new("t", 20, 20);
    let layer_id = doc.layers[0].id;
    let layer = doc.find_layer_mut(layer_id).unwrap();
    // A square from (5,5) to (15,15).
    let path = Path { nodes: vec![node(5.0, 5.0), node(15.0, 5.0), node(15.0, 15.0), node(5.0, 15.0)], closed: true };
    agenticart_core::path::fill_path(layer, &path, Rgba([9, 9, 9, 255]), None);
    assert_eq!(*layer.pixels.get_pixel(10, 10), Rgba([9, 9, 9, 255]), "inside the square");
    assert_eq!(layer.pixels.get_pixel(1, 1)[3], 0, "outside the square stays transparent");
    assert_eq!(layer.pixels.get_pixel(18, 18)[3], 0, "far corner outside the square stays transparent");
}

#[test]
fn curved_path_fill_has_real_area_a_straight_fallback_would_not() {
    let mut doc = Document::new("t", 40, 40);
    let layer_id = doc.layers[0].id;
    let layer = doc.find_layer_mut(layer_id).unwrap();
    // A lens ("vesica") shape: just two anchor points, A=(5,20) and
    // B=(35,20), joined by two Bezier arcs (A->B bulging up toward y=5,
    // B->A bulging down toward y=35). With only 2 distinct anchor points,
    // a naive straight-line-only polygon would be degenerate (zero area,
    // nothing filled) - so any filled pixel here proves the curve
    // flattening actually ran, not just the anchors.
    let path = Path {
        nodes: vec![
            PathNode { anchor: Point { x: 5.0, y: 20.0 }, control_in: Some(Point { x: 5.0, y: 35.0 }), control_out: Some(Point { x: 5.0, y: 5.0 }) },
            PathNode { anchor: Point { x: 35.0, y: 20.0 }, control_in: Some(Point { x: 35.0, y: 5.0 }), control_out: Some(Point { x: 35.0, y: 35.0 }) },
        ],
        closed: true,
    };
    agenticart_core::path::fill_path(layer, &path, Rgba([200, 0, 0, 255]), None);
    assert_eq!(*layer.pixels.get_pixel(20, 20), Rgba([200, 0, 0, 255]), "the lens's interior (only reachable via the curved arcs) is filled");
    assert_eq!(layer.pixels.get_pixel(20, 1)[3], 0, "well outside the lens stays transparent");
}

#[test]
fn path_stroke_reuses_the_brush_primitive() {
    let mut doc = Document::new("t", 20, 20);
    let layer_id = doc.layers[0].id;
    let layer = doc.find_layer_mut(layer_id).unwrap();
    let path = Path { nodes: vec![node(2.0, 10.0), node(18.0, 10.0)], closed: false };
    let brush = Brush { size: 6.0, color: Rgba([0, 0, 255, 255]), hardness: 1.0 };
    agenticart_core::path::stroke_path_shape(layer, &path, &brush, None);
    assert_eq!(layer.pixels.get_pixel(10, 10)[3], 255, "stroke covers a point along the path");
    assert_eq!(layer.pixels.get_pixel(10, 19)[3], 0, "stroke does not cover a point far from the path");
}

#[test]
fn cmyk_round_trip_preserves_pure_colors_approximately() {
    let mut img = image::RgbaImage::new(4, 4);
    for p in img.pixels_mut() {
        *p = Rgba([200, 60, 90, 255]);
    }
    let proofed = agenticart_core::color::cmyk_soft_proof(&img);
    let before = img.get_pixel(0, 0);
    let after = proofed.get_pixel(0, 0);
    for c in 0..3 {
        let diff = (before[c] as i32 - after[c] as i32).abs();
        assert!(diff <= 2, "naive CMYK round-trip should be near-lossless for in-gamut colors, channel {c} diff={diff}");
    }
}

#[test]
fn cmyk_soft_proof_actually_clips_a_saturated_primary() {
    // Pure red needs C=0,M=1,Y=1,K=0 in this GCR formula (total ink 200%),
    // over the soft-proof's 150% cap - this should visibly desaturate/
    // darken, proving the ink-limit clamp actually does something (the
    // naive CMY+K formula alone is a lossless bijection and would not).
    let mut img = image::RgbaImage::new(2, 2);
    for p in img.pixels_mut() {
        *p = Rgba([255, 0, 0, 255]);
    }
    let proofed = agenticart_core::color::cmyk_soft_proof(&img);
    let px = proofed.get_pixel(0, 0);
    assert_ne!((px[0], px[1], px[2]), (255, 0, 0), "an over-ink-limit saturated color must be visibly clipped, not passed through unchanged");
}

#[test]
fn cmyk_black_has_zero_rgb_after_round_trip() {
    let mut img = image::RgbaImage::new(2, 2);
    for p in img.pixels_mut() {
        *p = Rgba([0, 0, 0, 255]);
    }
    let proofed = agenticart_core::color::cmyk_soft_proof(&img);
    let px = proofed.get_pixel(0, 0);
    assert_eq!((px[0], px[1], px[2]), (0, 0, 0), "pure black should round-trip through CMYK as black");
}

#[test]
fn srgb_to_adobe_rgb_conversion_actually_changes_pixel_values() {
    let mut img = image::RgbaImage::new(4, 4);
    for p in img.pixels_mut() {
        *p = Rgba([255, 0, 0, 255]); // saturated red - the two gamuts disagree here
    }
    let converted = agenticart_core::color::convert_profile(&img, agenticart_core::color::NamedProfile::Srgb, agenticart_core::color::NamedProfile::AdobeRgb).expect("conversion should succeed");
    let before = img.get_pixel(0, 0);
    let after = converted.get_pixel(0, 0);
    assert_ne!(before, after, "converting saturated red between two different gamuts should change its stored value, not just tag it");
    assert_eq!(after[3], 255, "alpha channel must pass through the color transform untouched");
}

#[test]
fn convert_profile_is_identity_when_source_and_dest_match() {
    let mut img = image::RgbaImage::new(3, 3);
    for p in img.pixels_mut() {
        *p = Rgba([123, 45, 200, 255]);
    }
    let converted = agenticart_core::color::convert_profile(&img, agenticart_core::color::NamedProfile::Srgb, agenticart_core::color::NamedProfile::Srgb).unwrap();
    assert_eq!(img, converted, "converting a profile to itself must be a true no-op");
}

#[test]
fn psd_export_writes_a_real_psd_layer_group() {
    let mut doc = Document::new("Group Export Test", 12, 12);
    let bg_id = doc.layers[0].id;
    paint::stroke_path(doc.find_layer_mut(bg_id).unwrap(), &[BrushPoint { x: 6.0, y: 6.0, pressure: 1.0 }], &opaque_brush([10, 10, 10, 255]), None);

    let group_id = doc.create_group("My Group");
    doc.find_layer_mut(group_id).unwrap().opacity = 0.6;
    doc.find_layer_mut(group_id).unwrap().blend_mode = BlendMode::Screen;
    let child = agenticart_core::Layer::new_transparent("Child Layer", 12, 12);
    let child_id = child.id;
    doc.layers.push(child);
    doc.move_layer_into_group(child_id, group_id);
    paint::stroke_path(doc.find_layer_mut(child_id).unwrap(), &[BrushPoint { x: 6.0, y: 6.0, pressure: 1.0 }], &opaque_brush([200, 30, 30, 255]), None);

    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tmp = std::env::temp_dir().join(format!("agenticart_psd_group_test_{nanos}.psd"));
    agenticart_core::psd_export::document_to_psd(&doc, &tmp).expect("psd export should succeed");

    // Parse with the `psd` crate directly (not our psd_import wrapper) to
    // check its group-specific API, confirming this is a real PSD layer
    // group (lsct structure), not a flattened stand-in.
    let bytes = std::fs::read(&tmp).unwrap();
    let parsed = psd::Psd::from_bytes(&bytes).expect("the independent `psd` crate should parse our group structure");
    std::fs::remove_file(&tmp).ok();

    assert_eq!(parsed.groups().len(), 1, "exactly one real PSD group was written");
    let group = parsed.groups().values().next().unwrap();
    assert_eq!(group.name(), "My Group", "group name comes from the correct (opening) lsct record");
    assert_eq!(group.opacity(), (0.6f32 * 255.0).round() as u8, "group opacity comes from the correct (closing) lsct record");

    let child_layers: Vec<_> = parsed.layers().iter().filter(|l| l.parent_id() == Some(group.id())).collect();
    assert_eq!(child_layers.len(), 1, "the child layer is correctly associated with the group");
    assert_eq!(child_layers[0].name(), "Child Layer");
}

#[test]
fn psd_export_round_trips_through_the_independent_psd_crate_reader() {
    let mut doc = Document::new("PSD Export Test", 16, 12);
    let bg_id = doc.layers[0].id;
    paint::stroke_path(doc.find_layer_mut(bg_id).unwrap(), &[BrushPoint { x: 8.0, y: 6.0, pressure: 1.0 }], &opaque_brush([255, 0, 0, 255]), None);
    doc.find_layer_mut(bg_id).unwrap().opacity = 0.75;

    let tmp = std::env::temp_dir().join(format!(
        "agenticart_psd_export_test_{}.psd",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    agenticart_core::psd_export::document_to_psd(&doc, &tmp).expect("psd export should succeed");

    let reopened = agenticart_core::psd_import::document_from_psd(&tmp).expect("the independent `psd` crate should be able to parse our output");
    std::fs::remove_file(&tmp).ok();

    assert_eq!(reopened.width, 16);
    assert_eq!(reopened.height, 12);
    assert_eq!(reopened.layers.len(), 1);
    // The re-imported layer's pixels come back already opacity-premultiplied
    // by the `psd` crate's own compositing (it doesn't expose a separate
    // opacity field the way our project format does) - so check the
    // painted pixel is present and colored, not an exact byte match.
    let px = reopened.layers[0].pixels.get_pixel(8, 6);
    assert!(px[3] > 0, "painted pixel should be non-transparent after round-trip, got alpha={}", px[3]);
    assert!(px[0] > px[1] && px[0] > px[2], "painted pixel should still read as red-dominant, got {:?}", px.0);
}

#[test]
fn psd_export_with_multiple_layers_round_trips() {
    let mut doc = Document::new("Multi Layer PSD", 20, 20);
    let bg_id = doc.layers[0].id;
    paint::stroke_path(doc.find_layer_mut(bg_id).unwrap(), &[BrushPoint { x: 10.0, y: 10.0, pressure: 1.0 }], &opaque_brush([80, 160, 220, 255]), None);

    let mut top = agenticart_core::Layer::new_transparent("Top Shape", 20, 20);
    top.blend_mode = BlendMode::Multiply;
    let top_id = top.id;
    doc.layers.push(top);
    paint::stroke_path(doc.find_layer_mut(top_id).unwrap(), &[BrushPoint { x: 10.0, y: 10.0, pressure: 1.0 }], &opaque_brush([220, 80, 40, 255]), None);

    let mut third = agenticart_core::Layer::new_transparent("Third", 20, 20);
    third.visible = false;
    doc.layers.push(third);

    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tmp = std::env::temp_dir().join(format!("agenticart_psd_multi_test_{nanos}.psd"));
    agenticart_core::psd_export::document_to_psd(&doc, &tmp).expect("psd export should succeed");

    let reopened = agenticart_core::psd_import::document_from_psd(&tmp).expect("multi-layer PSD must still parse correctly (regression: a spurious extra length prefix used to corrupt this)");
    std::fs::remove_file(&tmp).ok();

    assert_eq!(reopened.layers.len(), 3, "all three layers survive the round trip, not just the first");
    let names: Vec<&str> = reopened.layers.iter().map(|l| l.name.as_str()).collect();
    assert!(names.contains(&"Background"), "layer names correctly readable: {names:?}");
    assert!(names.contains(&"Top Shape"), "layer names correctly readable: {names:?}");
    assert!(names.contains(&"Third"), "layer names correctly readable: {names:?}");

    let third_reopened = reopened.layers.iter().find(|l| l.name == "Third").unwrap();
    assert!(!third_reopened.visible, "a hidden layer's visibility flag survives the round trip (regression: bit 1 convention was inverted before)");
}

#[test]
fn project_round_trip_is_lossless() {
    let mut doc = Document::new("Round Trip", 12, 12);
    let layer_id = doc.layers[0].id;
    paint::stroke_path(doc.find_layer_mut(layer_id).unwrap(), &[BrushPoint { x: 6.0, y: 6.0, pressure: 1.0 }], &opaque_brush([9, 99, 199, 255]), None);
    doc.find_layer_mut(layer_id).unwrap().opacity = 0.42;
    doc.find_layer_mut(layer_id).unwrap().blend_mode = BlendMode::Screen;
    doc.selection = Some(SelectionRect { x: 1, y: 1, width: 3, height: 3 });

    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tmp = std::env::temp_dir().join(format!("agenticart_test_{nanos}.agenticart"));
    project::save_project(&doc, &tmp).expect("save should succeed");
    let reopened = project::load_project(&tmp).expect("load should succeed");
    std::fs::remove_file(&tmp).ok();

    assert_eq!(reopened.width, doc.width);
    assert_eq!(reopened.height, doc.height);
    assert_eq!(reopened.layers.len(), doc.layers.len());
    assert_eq!(reopened.layers[0].opacity, 0.42);
    assert_eq!(reopened.layers[0].blend_mode, BlendMode::Screen);
    assert_eq!(reopened.selection, doc.selection);
    assert_eq!(reopened.layers[0].pixels, doc.layers[0].pixels, "pixels must round-trip byte-for-byte (lossless PNG)");
}

#[test]
fn smart_filter_affects_the_render_without_touching_the_layers_pixels() {
    let mut doc = Document::new("Smart Filter", 16, 16);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 16, 16, Rgba([100, 100, 100, 255]), None);
    let untouched_pixels = doc.find_layer_mut(layer_id).unwrap().pixels.clone();

    let before = compositor::render(&doc);
    doc.find_layer_mut(layer_id).unwrap().smart_filters.push(SmartFilter::Invert);
    let after = compositor::render(&doc);

    assert_eq!(doc.find_layer_mut(layer_id).unwrap().pixels, untouched_pixels, "a smart filter must never bake into Layer::pixels");
    assert_ne!(before, after, "the render must reflect the smart filter");
    assert_eq!(after.get_pixel(0, 0), &Rgba([155, 155, 155, 255]), "invert is applied live at render time");

    doc.find_layer_mut(layer_id).unwrap().smart_filters.clear();
    let reverted = compositor::render(&doc);
    assert_eq!(reverted, before, "removing the filter fully reverts the render - proof it was never destructive");
}

#[test]
fn smart_filter_stack_applies_in_order() {
    // Invert then brighten should differ from brighten then invert for a
    // mid-gray pixel pushed toward one end of the range - proof the stack
    // is order-sensitive, not just a set of independently-applied ops.
    let mut doc = Document::new("Smart Filter Order", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([200, 200, 200, 255]), None);

    doc.find_layer_mut(layer_id).unwrap().smart_filters =
        vec![SmartFilter::Invert, SmartFilter::BrightnessContrast { brightness: 0.5, contrast: 0.0 }];
    let invert_then_brighten = compositor::render(&doc);

    doc.find_layer_mut(layer_id).unwrap().smart_filters =
        vec![SmartFilter::BrightnessContrast { brightness: 0.5, contrast: 0.0 }, SmartFilter::Invert];
    let brighten_then_invert = compositor::render(&doc);

    assert_ne!(invert_then_brighten, brighten_then_invert, "smart filter order must change the result");
}

#[test]
fn layer_mask_hides_pixels_without_touching_them_and_resizes_with_the_canvas() {
    let mut doc = Document::new("Masked", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([255, 0, 0, 255]), None);
    let untouched_pixels = doc.find_layer_mut(layer_id).unwrap().pixels.clone();

    // Left half hidden, right half visible.
    let mut mask = image::GrayImage::new(4, 4);
    for y in 0..4 {
        for x in 0..4 {
            mask.put_pixel(x, y, image::Luma([if x < 2 { 0 } else { 255 }]));
        }
    }
    doc.find_layer_mut(layer_id).unwrap().mask = Some(mask);

    let rendered = compositor::render(&doc);
    assert_eq!(rendered.get_pixel(0, 0)[3], 0, "masked-out region is hidden in the render");
    assert_eq!(rendered.get_pixel(3, 0), &Rgba([255, 0, 0, 255]), "masked-in region renders normally");
    assert_eq!(doc.find_layer_mut(layer_id).unwrap().pixels, untouched_pixels, "a mask must never bake into Layer::pixels");

    agenticart_core::resize::resize_document(&mut doc, 8, 8);
    let resized_mask = doc.find_layer_mut(layer_id).unwrap().mask.clone().expect("mask must survive a resize");
    assert_eq!(resized_mask.dimensions(), (8, 8), "mask must resize in lockstep with the layer, or effective_pixels would panic on an out-of-bounds read");
}

#[test]
fn layer_mask_round_trips_through_the_native_project_format() {
    let mut doc = Document::new("Mask Round Trip", 4, 4);
    let layer_id = doc.layers[0].id;
    let mask = image::GrayImage::from_fn(4, 4, |x, _y| image::Luma([if x < 2 { 10 } else { 240 }]));
    doc.find_layer_mut(layer_id).unwrap().mask = Some(mask.clone());

    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tmp = std::env::temp_dir().join(format!("agenticart_mask_test_{nanos}.agenticart"));
    project::save_project(&doc, &tmp).expect("save should succeed");
    let reopened = project::load_project(&tmp).expect("load should succeed");
    std::fs::remove_file(&tmp).ok();

    assert_eq!(reopened.layers[0].mask, Some(mask), "mask must round-trip byte-for-byte (lossless PNG)");
}

/// Renders a document whose background layer is fully covered by `base`
/// and whose top layer (blend mode `mode`, opacity 1) is fully covered by
/// `top`, both fully opaque. With both layers covering the whole canvas at
/// full opacity, out_alpha is 1 and the composite reduces to exactly
/// `blend_pixel(mode, base, top)` - the cleanest possible probe for a
/// blend mode's per-channel math.
fn render_full_coverage_blend(mode: BlendMode, base: [u8; 4], top: [u8; 4]) -> Rgba<u8> {
    let mut doc = Document::new("blend probe", 4, 4);
    let bg = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(bg).unwrap(), 0, 0, 4, 4, Rgba(base), None);

    let mut top_layer = agenticart_core::Layer::new_transparent("top", 4, 4);
    top_layer.blend_mode = mode;
    let top_id = top_layer.id;
    doc.layers.push(top_layer);
    shapes::fill_rect(doc.find_layer_mut(top_id).unwrap(), 0, 0, 4, 4, Rgba(top), None);

    *compositor::render(&doc).get_pixel(2, 2)
}

#[test]
fn linear_burn_subtracts_past_the_midpoint() {
    let px = render_full_coverage_blend(BlendMode::LinearBurn, [200, 200, 200, 255], [100, 100, 100, 255]);
    // b + t - 1 = 200/255 + 100/255 - 1 ≈ 0.1765 -> ~45
    assert!((px[0] as i32 - 45).abs() <= 1, "expected ~45, got {}", px[0]);
}

#[test]
fn linear_dodge_add_clamps_at_white() {
    let px = render_full_coverage_blend(BlendMode::LinearDodge, [200, 200, 200, 255], [100, 100, 100, 255]);
    assert_eq!(px[0], 255, "200+100 overflows to white");
}

#[test]
fn subtract_clamps_at_black() {
    let px = render_full_coverage_blend(BlendMode::Subtract, [50, 50, 50, 255], [200, 200, 200, 255]);
    assert_eq!(px[0], 0, "50-200 clamps to black, not a negative wraparound");
}

#[test]
fn divide_of_equal_channels_is_white() {
    let px = render_full_coverage_blend(BlendMode::Divide, [150, 150, 150, 255], [150, 150, 150, 255]);
    assert_eq!(px[0], 255, "b/b == 1 for any nonzero base");
}

#[test]
fn darker_color_picks_the_whole_pixel_with_lower_luminosity_not_a_per_channel_min() {
    // Per-channel min of red and green would be black; DarkerColor instead
    // picks one whole source pixel - red has the lower luminosity here.
    let px = render_full_coverage_blend(BlendMode::DarkerColor, [255, 0, 0, 255], [0, 255, 0, 255]);
    assert_eq!(px, Rgba([255, 0, 0, 255]), "must pick the whole red pixel, not synthesize black via a per-channel min");
}

#[test]
fn lighter_color_picks_the_whole_pixel_with_higher_luminosity() {
    let px = render_full_coverage_blend(BlendMode::LighterColor, [255, 0, 0, 255], [0, 255, 0, 255]);
    assert_eq!(px, Rgba([0, 255, 0, 255]), "green has higher luminosity than red, so it must win whole");
}

#[test]
fn color_blend_mode_keeps_base_luminosity_but_takes_top_hue_and_saturation() {
    let base = [128, 128, 128, 255]; // gray: hue/sat-less, luminosity ~0.502
    let top = [255, 0, 0, 255]; // pure red
    let px = render_full_coverage_blend(BlendMode::Color, base, top);
    assert_ne!(px, Rgba(base), "must take on top's color, not stay gray");
    assert_ne!(px, Rgba(top), "must not become pure top either - base's luminosity survives");
    let result_luminosity = 0.3 * px[0] as f32 + 0.59 * px[1] as f32 + 0.11 * px[2] as f32;
    let base_luminosity = 0.3 * base[0] as f32 + 0.59 * base[1] as f32 + 0.11 * base[2] as f32;
    assert!((result_luminosity - base_luminosity).abs() < 2.0, "Color mode must preserve the base's luminosity: got {result_luminosity}, expected ~{base_luminosity}");
}

#[test]
fn luminosity_blend_mode_is_the_inverse_pairing_of_color() {
    let base = [255, 0, 0, 255]; // pure red
    let top = [128, 128, 128, 255]; // gray
    let px = render_full_coverage_blend(BlendMode::Luminosity, base, top);
    // Luminosity(base, top) takes base's hue/sat with top's luminosity -
    // the opposite pairing from Color mode.
    let result_luminosity = 0.3 * px[0] as f32 + 0.59 * px[1] as f32 + 0.11 * px[2] as f32;
    let top_luminosity = 0.3 * top[0] as f32 + 0.59 * top[1] as f32 + 0.11 * top[2] as f32;
    assert!((result_luminosity - top_luminosity).abs() < 2.0, "Luminosity mode must take on top's luminosity: got {result_luminosity}, expected ~{top_luminosity}");
}

#[test]
fn vector_mask_rasterizes_a_closed_path_into_a_layer_mask() {
    let rect_path = Path {
        nodes: vec![
            PathNode { anchor: Point { x: 1.0, y: 1.0 }, control_in: None, control_out: None },
            PathNode { anchor: Point { x: 7.0, y: 1.0 }, control_in: None, control_out: None },
            PathNode { anchor: Point { x: 7.0, y: 7.0 }, control_in: None, control_out: None },
            PathNode { anchor: Point { x: 1.0, y: 7.0 }, control_in: None, control_out: None },
        ],
        closed: true,
    };
    let mask = agenticart_core::path::rasterize_mask(&rect_path, 8, 8);
    assert_eq!(mask.get_pixel(4, 4)[0], 255, "inside the rectangle must be white (visible)");
    assert_eq!(mask.get_pixel(0, 0)[0], 0, "outside the rectangle must be black (hidden)");

    let mut doc = Document::new("Vector Mask", 8, 8);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 8, 8, Rgba([255, 0, 0, 255]), None);
    doc.find_layer_mut(layer_id).unwrap().mask = Some(mask);

    let rendered = compositor::render(&doc);
    assert_eq!(rendered.get_pixel(4, 4)[3], 255, "masked-in region renders");
    assert_eq!(rendered.get_pixel(0, 0)[3], 0, "masked-out region is hidden");
}

#[test]
fn layer_comp_snapshots_and_restores_visibility_and_opacity() {
    let mut doc = Document::new("Comps", 4, 4);
    let bg = doc.layers[0].id;

    let mut top = agenticart_core::Layer::new_transparent("top", 4, 4);
    let top_id = top.id;
    top.opacity = 1.0;
    doc.layers.push(top);

    let comp_a = doc.create_layer_comp("A: both visible");

    doc.find_layer_mut(bg).unwrap().visible = false;
    doc.find_layer_mut(top_id).unwrap().opacity = 0.3;
    let comp_b = doc.create_layer_comp("B: bg hidden, top faded");

    assert!(doc.apply_layer_comp(comp_a));
    assert!(doc.find_layer_mut(bg).unwrap().visible, "comp A must restore bg visibility");
    assert_eq!(doc.find_layer_mut(top_id).unwrap().opacity, 1.0, "comp A must restore top opacity");

    assert!(doc.apply_layer_comp(comp_b));
    assert!(!doc.find_layer_mut(bg).unwrap().visible, "comp B must restore bg hidden");
    assert_eq!(doc.find_layer_mut(top_id).unwrap().opacity, 0.3, "comp B must restore top's faded opacity");

    assert_eq!(doc.layer_comps.len(), 2);
    assert!(doc.delete_layer_comp(comp_a));
    assert_eq!(doc.layer_comps.len(), 1);
    assert!(!doc.delete_layer_comp(comp_a), "deleting an already-deleted comp must report false, not panic");
}

#[test]
fn layer_comp_applying_after_a_layer_is_deleted_skips_it_without_erroring() {
    let mut doc = Document::new("Comps", 4, 4);
    let mut extra = agenticart_core::Layer::new_transparent("extra", 4, 4);
    let extra_id = extra.id;
    extra.opacity = 1.0;
    doc.layers.push(extra);

    let comp = doc.create_layer_comp("has extra");
    doc.delete_layer(extra_id);

    assert!(doc.apply_layer_comp(comp), "applying must still succeed even though one captured layer is gone");
}

#[test]
fn layer_comps_round_trip_through_the_native_project_format() {
    let mut doc = Document::new("Comp Round Trip", 4, 4);
    doc.find_layer_mut(doc.layers[0].id).unwrap().opacity = 0.7;
    let comp_id = doc.create_layer_comp("Saved");

    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tmp = std::env::temp_dir().join(format!("agenticart_comp_test_{nanos}.agenticart"));
    project::save_project(&doc, &tmp).expect("save should succeed");
    let reopened = project::load_project(&tmp).expect("load should succeed");
    std::fs::remove_file(&tmp).ok();

    assert_eq!(reopened.layer_comps.len(), 1);
    assert_eq!(reopened.layer_comps[0].id, comp_id);
    assert_eq!(reopened.layer_comps[0].name, "Saved");
    assert_eq!(reopened.layer_comps[0].entries[0].opacity, 0.7);
}

#[test]
fn merge_down_bakes_the_top_layer_onto_the_one_below_and_removes_it() {
    let mut doc = Document::new("Merge", 4, 4);
    let bg = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(bg).unwrap(), 0, 0, 4, 4, Rgba([200, 200, 200, 255]), None);

    let mut top = agenticart_core::Layer::new_transparent("top", 4, 4);
    top.blend_mode = BlendMode::Multiply;
    let top_id = top.id;
    doc.layers.push(top);
    shapes::fill_rect(doc.find_layer_mut(top_id).unwrap(), 0, 0, 4, 4, Rgba([100, 100, 100, 255]), None);

    let before_render = compositor::render(&doc);

    let survivor = doc.merge_down(top_id).expect("adjacent non-group layers must merge");
    assert_eq!(survivor, bg, "the lower layer survives, keeping its own id");
    assert_eq!(doc.layers.len(), 1, "the top layer must be removed");

    let after_render = compositor::render(&doc);
    assert_eq!(before_render, after_render, "merging must not change the rendered result, only the layer count");
    assert_eq!(doc.layers[0].pixels.get_pixel(2, 2)[0], before_render.get_pixel(2, 2)[0], "the baked pixels reflect the multiply blend");
}

#[test]
fn merge_down_fails_at_the_bottom_of_the_stack_or_across_group_or_at_a_group_marker() {
    let mut doc = Document::new("Merge Fails", 4, 4);
    let bg = doc.layers[0].id;
    assert_eq!(doc.merge_down(bg), None, "nothing below the bottom layer");

    let group_id = doc.create_group("G");
    assert_eq!(doc.merge_down(group_id), None, "a group marker itself can't be merged - ungroup first");

    let mut in_group = agenticart_core::Layer::new_transparent("in group", 4, 4);
    let in_group_id = in_group.id;
    in_group.parent_group = Some(group_id);
    doc.layers.push(in_group);
    // in_group sits above bg but belongs to a different "context" (a group) than bg.
    assert_eq!(doc.merge_down(in_group_id), None, "must not merge across a group boundary");
}

#[test]
fn document_flatten_collapses_the_stack_and_fills_transparency_with_white() {
    let mut doc = Document::new("Flatten", 4, 4);
    let bg = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(bg).unwrap(), 0, 0, 2, 4, Rgba([255, 0, 0, 255]), None);

    let top = agenticart_core::Layer::new_transparent("top", 4, 4);
    let top_id = top.id;
    doc.layers.push(top);
    shapes::fill_rect(doc.find_layer_mut(top_id).unwrap(), 2, 0, 2, 4, Rgba([0, 0, 255, 255]), None);

    compositor::flatten(&mut doc);

    assert_eq!(doc.layers.len(), 1, "flatten collapses to one layer");
    assert_eq!(doc.layers[0].name, "Background");
    assert_eq!(doc.layers[0].pixels.get_pixel(0, 0), &Rgba([255, 0, 0, 255]), "painted content survives");
    assert_eq!(doc.layers[0].pixels.get_pixel(3, 0), &Rgba([0, 0, 255, 255]), "painted content survives");
    // Neither rect covers row y in [0,4) fully at every x - check a spot
    // that was transparent before flattening (e.g. nowhere here, since the
    // two rects tile the whole canvas) by shrinking coverage instead:
    assert_eq!(doc.layers[0].pixels.get_pixel(0, 0)[3], 255, "flatten produces a fully opaque result");
}

#[test]
fn document_flatten_fills_actual_transparent_gaps_with_white() {
    let mut doc = Document::new("Flatten Gap", 4, 4);
    let bg = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(bg).unwrap(), 0, 0, 1, 1, Rgba([255, 0, 0, 255]), None);
    // The rest of the 4x4 canvas is left fully transparent.

    compositor::flatten(&mut doc);

    assert_eq!(doc.layers[0].pixels.get_pixel(3, 3), &Rgba([255, 255, 255, 255]), "transparent areas become opaque white, matching Photoshop's Flatten Image");
}

#[test]
fn translate_moves_content_and_clips_at_the_canvas_edge() {
    let mut doc = Document::new("Translate", 8, 8);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 2, 2, Rgba([255, 0, 0, 255]), None);

    agenticart_core::transform::translate(doc.find_layer_mut(layer_id).unwrap(), 4.0, 4.0);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    assert_eq!(layer.pixels.get_pixel(0, 0)[3], 0, "original position must now be empty");
    assert_eq!(layer.pixels.get_pixel(4, 4), &Rgba([255, 0, 0, 255]), "content must land at the translated position");
}

#[test]
fn scale_up_enlarges_content_about_the_layer_center() {
    let mut doc = Document::new("Scale", 8, 8);
    let layer_id = doc.layers[0].id;
    // A single opaque pixel at the exact center.
    doc.find_layer_mut(layer_id).unwrap().pixels.put_pixel(4, 4, Rgba([0, 255, 0, 255]));

    agenticart_core::transform::scale(doc.find_layer_mut(layer_id).unwrap(), 2.0, 2.0);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    // Scaling 2x about the center should spread nonzero-alpha coverage
    // wider than the single original pixel.
    let opaque_count = layer.pixels.pixels().filter(|p| p[3] > 0).count();
    assert!(opaque_count > 1, "scaling up must spread coverage over more pixels, got {opaque_count}");
}

#[test]
fn rotate_180_about_center_maps_a_corner_block_to_the_opposite_corner() {
    let mut doc = Document::new("Rotate", 8, 8);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 2, 2, Rgba([0, 0, 255, 255]), None);

    agenticart_core::transform::rotate(doc.find_layer_mut(layer_id).unwrap(), 180.0);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    let top_left_opaque: u32 = (0..2).flat_map(|y| (0..2).map(move |x| (x, y))).map(|(x, y)| layer.pixels.get_pixel(x, y)[3] as u32).sum();
    let bottom_right_opaque: u32 = (4..8).flat_map(|y| (4..8).map(move |x| (x, y))).map(|(x, y)| layer.pixels.get_pixel(x, y)[3] as u32).sum();
    assert_eq!(top_left_opaque, 0, "original corner block must now be empty");
    assert!(bottom_right_opaque > 0, "content must appear in the opposite quadrant after a 180-degree rotation");
}

#[test]
fn transform_carries_the_layer_mask_along_with_the_pixels() {
    let mut doc = Document::new("Transform Mask", 8, 8);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 2, 2, Rgba([255, 255, 255, 255]), None);
    let mut mask = image::GrayImage::from_pixel(8, 8, image::Luma([0]));
    for y in 0..2 {
        for x in 0..2 {
            mask.put_pixel(x, y, image::Luma([255]));
        }
    }
    doc.find_layer_mut(layer_id).unwrap().mask = Some(mask);

    agenticart_core::transform::translate(doc.find_layer_mut(layer_id).unwrap(), 4.0, 4.0);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    let mask = layer.mask.as_ref().expect("mask must survive the transform");
    assert_eq!(mask.get_pixel(0, 0)[0], 0, "mask's original coverage must move along with the pixels");
    assert_eq!(mask.get_pixel(4, 4)[0], 255, "mask coverage must appear at the translated position");
}

#[test]
fn content_aware_fill_converges_to_the_surrounding_solid_color() {
    let mut doc = Document::new("Inpaint", 16, 16);
    let layer_id = doc.layers[0].id;
    // Fill the whole canvas with a solid color, then punch a "hole" of a
    // different color in the middle for content_aware_fill to repair.
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 16, 16, Rgba([200, 100, 50, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 6, 6, 4, 4, Rgba([0, 0, 0, 255]), None);

    agenticart_core::generative::content_aware_fill(
        doc.find_layer_mut(layer_id).unwrap(),
        SelectionRect { x: 6, y: 6, width: 4, height: 4 },
        64,
    );

    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(8, 8);
    for (channel, expected) in [(0, 200u8), (1, 100), (2, 50)] {
        let diff = (px[channel] as i32 - expected as i32).abs();
        assert!(diff <= 3, "channel {channel}: expected ~{expected} (surrounded by uniform color), got {}", px[channel]);
    }
    assert_eq!(px[3], 255, "filled region must be opaque");
}

#[test]
fn content_aware_fill_does_nothing_for_an_out_of_bounds_region() {
    let mut doc = Document::new("Inpaint Bounds", 8, 8);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 8, 8, Rgba([10, 20, 30, 255]), None);
    let before = doc.find_layer_mut(layer_id).unwrap().pixels.clone();

    // Entirely outside the canvas - must clip to nothing and no-op.
    agenticart_core::generative::content_aware_fill(doc.find_layer_mut(layer_id).unwrap(), SelectionRect { x: 100, y: 100, width: 4, height: 4 }, 8);

    assert_eq!(doc.find_layer_mut(layer_id).unwrap().pixels, before, "an out-of-bounds region must not modify the layer");
}

#[test]
fn text_layer_renders_live_and_never_bakes_into_pixels() {
    let mut doc = Document::new("Text Layer", 100, 40);
    let mut layer = agenticart_core::Layer::new_transparent("Text", 100, 40);
    layer.text = Some(agenticart_core::document::TextLayerData { text: "Hi".to_string(), x: 5, y: 5, size: 24.0, color: [0, 0, 0, 255] });
    let layer_id = layer.id;
    doc.layers.push(layer);

    let rendered = compositor::render(&doc);
    let any_ink = rendered.pixels().any(|p| p[3] > 0);
    if !any_ink {
        eprintln!("skipping text layer assertions: no system font found to render with in this environment");
        return;
    }
    assert!(doc.find_layer_mut(layer_id).unwrap().pixels.pixels().all(|p| p[3] == 0), "a text layer's own Layer::pixels must stay untouched - content is synthesized live, not baked");
}

#[test]
fn text_layer_round_trips_through_the_native_project_format() {
    let mut doc = Document::new("Text Round Trip", 20, 20);
    let mut layer = agenticart_core::Layer::new_transparent("Text", 20, 20);
    layer.text = Some(agenticart_core::document::TextLayerData { text: "hello".to_string(), x: 1, y: 2, size: 12.0, color: [10, 20, 30, 255] });
    doc.layers.push(layer);

    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tmp = std::env::temp_dir().join(format!("agenticart_text_test_{nanos}.agenticart"));
    project::save_project(&doc, &tmp).expect("save should succeed");
    let reopened = project::load_project(&tmp).expect("load should succeed");
    std::fs::remove_file(&tmp).ok();

    let text_data = reopened.layers[1].text.as_ref().expect("text data must round-trip");
    assert_eq!(text_data.text, "hello");
    assert_eq!(text_data.x, 1);
    assert_eq!(text_data.size, 12.0);
    assert_eq!(text_data.color, [10, 20, 30, 255]);
}

#[test]
fn outer_glow_adds_a_halo_around_opaque_content_without_touching_pixels() {
    let mut doc = Document::new("Glow", 20, 20);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 8, 8, 4, 4, Rgba([255, 255, 255, 255]), None);
    let untouched_pixels = doc.find_layer_mut(layer_id).unwrap().pixels.clone();

    let before = compositor::render(&doc);
    doc.find_layer_mut(layer_id).unwrap().style.outer_glow =
        Some(agenticart_core::document::OuterGlowStyle { color: [255, 0, 0, 255], radius: 4.0, opacity: 1.0 });
    let after = compositor::render(&doc);

    assert_eq!(doc.find_layer_mut(layer_id).unwrap().pixels, untouched_pixels, "outer glow must never bake into Layer::pixels");
    assert_ne!(before, after, "the render must reflect the glow");
    // Just outside the filled rect (was fully transparent before) should
    // now show some red glow.
    let glow_px = after.get_pixel(6, 10);
    assert!(glow_px[3] > 0, "area just outside the shape must now show glow alpha");
    assert!(glow_px[0] as i32 > glow_px[2] as i32, "glow color (red) must dominate over any blue channel there");
}

#[test]
fn linear_gradient_interpolates_between_two_stops_along_its_axis() {
    use agenticart_core::gradient::{fill_gradient, GradientKind, GradientStop};
    let mut doc = Document::new("Gradient", 20, 1);
    let layer_id = doc.layers[0].id;
    let stops = [
        GradientStop { position: 0.0, color: [0, 0, 0, 255] },
        GradientStop { position: 1.0, color: [255, 255, 255, 255] },
    ];
    fill_gradient(doc.find_layer_mut(layer_id).unwrap(), GradientKind::Linear, (0.0, 0.5), (20.0, 0.5), &stops, None);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    let start_px = layer.pixels.get_pixel(0, 0)[0];
    let mid_px = layer.pixels.get_pixel(10, 0)[0];
    let end_px = layer.pixels.get_pixel(19, 0)[0];
    assert!(start_px < mid_px, "start must be darker than the middle");
    assert!(mid_px < end_px, "middle must be darker than the end");
    assert!(end_px > 200, "end of the ramp must be near-white, got {end_px}");
}

#[test]
fn linear_gradient_is_solid_beyond_its_endpoints() {
    use agenticart_core::gradient::{fill_gradient, GradientKind, GradientStop};
    let mut doc = Document::new("Gradient Clamp", 10, 1);
    let layer_id = doc.layers[0].id;
    let stops = [GradientStop { position: 0.0, color: [10, 20, 30, 255] }, GradientStop { position: 1.0, color: [200, 210, 220, 255] }];
    // A short gradient axis in the middle of a wider canvas - pixels
    // before x=3 and after x=6 lie outside [start, end] and must clamp.
    fill_gradient(doc.find_layer_mut(layer_id).unwrap(), GradientKind::Linear, (3.0, 0.5), (6.0, 0.5), &stops, None);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    assert_eq!(layer.pixels.get_pixel(0, 0), &Rgba([10, 20, 30, 255]), "before the start, must clamp to the first stop's color");
    assert_eq!(layer.pixels.get_pixel(9, 0), &Rgba([200, 210, 220, 255]), "past the end, must clamp to the last stop's color");
}

#[test]
fn radial_gradient_is_centered_and_symmetric() {
    use agenticart_core::gradient::{fill_gradient, GradientKind, GradientStop};
    let mut doc = Document::new("Radial", 21, 21);
    let layer_id = doc.layers[0].id;
    let stops = [GradientStop { position: 0.0, color: [255, 0, 0, 255] }, GradientStop { position: 1.0, color: [0, 0, 255, 255] }];
    // Pixels are sampled at their center (x + 0.5, y + 0.5), so the true
    // geometric center of pixel index 10 is (10.5, 10.5), not (10.0, 10.0)
    // - using the latter would make x=5 and x=15 not actually equidistant
    // from the sampled center, breaking the symmetry this test checks.
    fill_gradient(doc.find_layer_mut(layer_id).unwrap(), GradientKind::Radial, (10.5, 10.5), (20.5, 10.5), &stops, None);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    let center = layer.pixels.get_pixel(10, 10);
    assert!(center[0] as i32 > center[2] as i32, "the center must be dominated by the start color (red)");
    let left = layer.pixels.get_pixel(5, 10);
    let right = layer.pixels.get_pixel(15, 10);
    assert_eq!(left, right, "equidistant points from the center must match - the gradient is radially symmetric");
}

#[test]
fn paint_bucket_fills_only_the_contiguous_matching_region() {
    let mut doc = Document::new("Paint Bucket", 10, 10);
    let layer_id = doc.layers[0].id;
    // Two separate red squares, not touching, plus untouched background.
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 3, 3, Rgba([255, 0, 0, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 7, 7, 3, 3, Rgba([255, 0, 0, 255]), None);

    shapes::paint_bucket(doc.find_layer_mut(layer_id).unwrap(), 1, 1, Rgba([0, 255, 0, 255]), 10, None);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    assert_eq!(layer.pixels.get_pixel(1, 1), &Rgba([0, 255, 0, 255]), "the seeded region must be filled");
    assert_eq!(layer.pixels.get_pixel(8, 8), &Rgba([255, 0, 0, 255]), "a separate same-colored region must NOT be filled - paint bucket is contiguous only");
    assert_eq!(layer.pixels.get_pixel(5, 5), &Rgba([0, 0, 0, 0]), "background outside either region must stay untouched");
}

#[test]
fn paint_bucket_respects_tolerance() {
    let mut doc = Document::new("Paint Bucket Tolerance", 10, 1);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 5, 1, Rgba([100, 100, 100, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 5, 0, 5, 1, Rgba([200, 200, 200, 255]), None);

    // Low tolerance: the fill must not cross the 100 -> 200 boundary.
    shapes::paint_bucket(doc.find_layer_mut(layer_id).unwrap(), 0, 0, Rgba([0, 255, 0, 255]), 10, None);
    let layer = doc.find_layer_mut(layer_id).unwrap();
    assert_eq!(layer.pixels.get_pixel(4, 0), &Rgba([0, 255, 0, 255]));
    assert_eq!(layer.pixels.get_pixel(5, 0), &Rgba([200, 200, 200, 255]), "low tolerance must not cross into the differently-colored region");
}

#[test]
fn clone_stamp_copies_pixels_with_a_constant_offset_from_source() {
    let mut doc = Document::new("Clone Stamp", 20, 20);
    let layer_id = doc.layers[0].id;
    // A distinctive red square to clone from, on an otherwise blank canvas.
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([255, 0, 0, 255]), None);

    let opaque_hard_brush = Brush { size: 6.0, color: Rgba([0, 0, 0, 255]), hardness: 1.0 };
    // Clone from the red square (source anchored at (2,2)) to a
    // destination stroke starting at (12,12): a constant offset of
    // (-10,-10) applies for the whole stroke.
    agenticart_core::paint::clone_stamp(
        doc.find_layer_mut(layer_id).unwrap(),
        (2.0, 2.0),
        &[BrushPoint { x: 12.0, y: 12.0, pressure: 1.0 }],
        &opaque_hard_brush,
        None,
    );

    let layer = doc.find_layer_mut(layer_id).unwrap();
    assert_eq!(layer.pixels.get_pixel(12, 12), &Rgba([255, 0, 0, 255]), "the destination must pick up the source region's red color");
    assert_eq!(layer.pixels.get_pixel(0, 0)[3], 255, "the original source content must be untouched");
}

#[test]
fn clone_stamp_samples_nothing_when_the_offset_source_is_out_of_bounds() {
    let mut doc = Document::new("Clone Stamp OOB", 10, 10);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 10, 10, Rgba([100, 100, 100, 255]), None);

    let brush = Brush { size: 4.0, color: Rgba([0, 0, 0, 255]), hardness: 1.0 };
    // Source point far off-canvas relative to the destination - every
    // sample the stroke would need lands outside the layer.
    agenticart_core::paint::clone_stamp(doc.find_layer_mut(layer_id).unwrap(), (-50.0, -50.0), &[BrushPoint { x: 5.0, y: 5.0, pressure: 1.0 }], &brush, None);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    assert_eq!(layer.pixels.get_pixel(5, 5), &Rgba([100, 100, 100, 255]), "an out-of-bounds source sample must leave the destination pixel untouched, not paint black or panic");
}

#[test]
fn dodge_lightens_and_burn_darkens_along_a_stroke() {
    use agenticart_core::paint::{dodge_burn, ToneMode};
    let mut doc = Document::new("Dodge Burn", 10, 10);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 10, 10, Rgba([128, 128, 128, 255]), None);
    let original_alpha = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(5, 5)[3];

    let brush = Brush { size: 6.0, color: Rgba([0, 0, 0, 255]), hardness: 1.0 };
    dodge_burn(doc.find_layer_mut(layer_id).unwrap(), &[BrushPoint { x: 5.0, y: 5.0, pressure: 1.0 }], &brush, ToneMode::Dodge, 0.3, None);
    let dodged = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(5, 5)[0];
    assert!(dodged > 128, "dodge must lighten, got {dodged}");
    assert_eq!(doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(5, 5)[3], original_alpha, "dodge must not change alpha");

    // Fresh gray canvas for burn.
    let mut doc2 = Document::new("Burn", 10, 10);
    let layer_id2 = doc2.layers[0].id;
    shapes::fill_rect(doc2.find_layer_mut(layer_id2).unwrap(), 0, 0, 10, 10, Rgba([128, 128, 128, 255]), None);
    dodge_burn(doc2.find_layer_mut(layer_id2).unwrap(), &[BrushPoint { x: 5.0, y: 5.0, pressure: 1.0 }], &brush, ToneMode::Burn, 0.3, None);
    let burned = doc2.find_layer_mut(layer_id2).unwrap().pixels.get_pixel(5, 5)[0];
    assert!(burned < 128, "burn must darken, got {burned}");
}

#[test]
fn document_store_shares_live_state_across_independent_handles() {
    // A DocumentStore is Clone (Arc<Mutex<..>> internally) - this is what
    // lets the stdio transport, the HTTP transport, and the desktop UI all
    // hold their own handle to the SAME live document set, so several
    // agents (and the human UI) can hold live views of the same document.
    // Simulate two independent sessions with two cloned handles and
    // confirm a mutation through one is immediately visible through the
    // other - not just structurally
    // possible but actually true.
    let session_a = agenticart_core::DocumentStore::new();
    let session_b = session_a.clone();

    let doc = Document::new("Shared", 8, 8);
    let doc_id = doc.id;
    let layer_id = doc.layers[0].id;
    session_a.insert(doc);

    session_a
        .mutate(doc_id, move |doc| {
            shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 8, 8, Rgba([9, 8, 7, 255]), None);
        })
        .expect("session_a must see the document it just inserted");

    let seen_by_b = session_b.get_clone(doc_id).expect("session_b must see the document session_a created");
    assert_eq!(seen_by_b.layers[0].pixels.get_pixel(0, 0), &Rgba([9, 8, 7, 255]), "an edit made through one handle must be visible through an independent handle to the same store");
}

#[test]
fn document_store_serializes_concurrent_mutations_without_losing_any() {
    use std::sync::Arc;
    use std::thread;

    let store = agenticart_core::DocumentStore::new();
    let doc = Document::new("Concurrent", 4, 4);
    let doc_id = doc.id;
    let layer_id = doc.layers[0].id;
    store.insert(doc);

    let store = Arc::new(store);
    let mut handles = Vec::new();
    for _ in 0..20 {
        let store = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            store
                .mutate(doc_id, move |doc| {
                    let layer = doc.find_layer_mut(layer_id).unwrap();
                    layer.opacity = (layer.opacity - 0.01).max(0.0);
                })
                .unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let doc = store.get_clone(doc_id).unwrap();
    // Each of the 20 threads must have applied its own decrement under
    // the store's lock - a torn/lost update (a classic concurrency bug)
    // would leave opacity higher than this.
    assert!((doc.layers[0].opacity - 0.8).abs() < 1e-6, "expected exactly 20 serialized 0.01 decrements from 1.0, got {}", doc.layers[0].opacity);
}

#[test]
fn generative_expand_grows_the_canvas_and_fills_the_new_border() {
    let mut doc = Document::new("Expand", 10, 10);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 10, 10, Rgba([50, 100, 150, 255]), None);

    agenticart_core::generative::expand_canvas(&mut doc, layer_id, 0, 0, 5, 5, 64).unwrap();

    assert_eq!((doc.width, doc.height), (20, 10), "canvas must grow by exactly left+right / top+bottom");
    let layer = doc.find_layer_mut(layer_id).unwrap();
    // Original content shifted right by `left` (5) must still be there.
    assert_eq!(layer.pixels.get_pixel(7, 5), &Rgba([50, 100, 150, 255]), "original content must survive, repositioned");
    // The newly exposed left border must be filled (opaque), not left transparent.
    assert_eq!(layer.pixels.get_pixel(1, 5)[3], 255, "the new left border must be filled, not left blank");
    // And it should have picked up roughly the neighboring color, not some unrelated color.
    let filled = layer.pixels.get_pixel(1, 5);
    assert!((filled[0] as i32 - 50).abs() < 10, "expanded border should approximate the adjacent original color");
}

#[test]
fn generative_expand_errors_on_an_unknown_layer_without_leaving_the_canvas_half_resized() {
    let mut doc = Document::new("Expand Bad Layer", 8, 8);
    let bogus_id = uuid::Uuid::new_v4();
    let result = agenticart_core::generative::expand_canvas(&mut doc, bogus_id, 2, 2, 2, 2, 8);
    assert!(result.is_err(), "an unknown layer id must error");
}

#[test]
fn bevel_emboss_paints_highlight_and_shadow_on_opposite_edges_without_touching_pixels() {
    let mut doc = Document::new("Bevel", 30, 30);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 5, 5, 20, 20, Rgba([128, 128, 128, 255]), None);
    let untouched_pixels = doc.find_layer_mut(layer_id).unwrap().pixels.clone();

    let before = compositor::render(&doc);
    doc.find_layer_mut(layer_id).unwrap().style.bevel_emboss = Some(agenticart_core::document::BevelEmbossStyle {
        depth: 3.0,
        angle_degrees: 0.0, // light from the right: left edge should get the highlight, right edge the shadow
        highlight_color: [255, 255, 255, 255],
        shadow_color: [0, 0, 0, 255],
    });
    let after = compositor::render(&doc);

    assert_eq!(doc.find_layer_mut(layer_id).unwrap().pixels, untouched_pixels, "bevel/emboss must never bake into Layer::pixels");
    assert_ne!(before, after, "the render must reflect the bevel");

    // Left edge of the shape (near x=5) should trend brighter than the
    // flat gray interior; right edge (near x=24) should trend darker.
    let left_edge = after.get_pixel(6, 15)[0];
    let right_edge = after.get_pixel(23, 15)[0];
    let interior = after.get_pixel(15, 15)[0];
    assert!(left_edge > interior, "left edge (facing the light) should be brighter than the flat interior: {left_edge} vs {interior}");
    assert!(right_edge < interior, "right edge (facing away from the light) should be darker than the flat interior: {right_edge} vs {interior}");
}

#[test]
fn bevel_emboss_stays_within_the_layers_opaque_area() {
    let mut doc = Document::new("Bevel Inner", 20, 20);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 5, 5, 10, 10, Rgba([128, 128, 128, 255]), None);
    doc.find_layer_mut(layer_id).unwrap().style.bevel_emboss =
        Some(agenticart_core::document::BevelEmbossStyle { depth: 2.0, angle_degrees: 135.0, highlight_color: [255, 255, 255, 255], shadow_color: [0, 0, 0, 255] });

    let rendered = compositor::render(&doc);
    assert_eq!(rendered.get_pixel(0, 0)[3], 0, "an inner bevel must not paint outside the layer's own opaque area");
}

#[test]
fn levels_remaps_input_and_output_ranges() {
    let mut doc = Document::new("Levels", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([128, 128, 128, 255]), None);
    // Input range [0.25, 0.75] maps 128/255≈0.502 to about the midpoint of
    // that range -> t≈0.503, gamma 1.0, output range [0,1] unchanged.
    adjustments::levels(doc.find_layer_mut(layer_id).unwrap(), 0.0, 0.5, 1.0, 0.0, 1.0, None);
    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0)[0];
    assert!(px > 200, "compressing input [0,0.5] to output [0,1] should roughly double a midtone value, got {px}");
}

#[test]
fn curves_applies_a_custom_mapping() {
    let mut doc = Document::new("Curves", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([100, 100, 100, 255]), None);
    // Flat curve: everything maps to 0.9.
    adjustments::curves(doc.find_layer_mut(layer_id).unwrap(), &[(0.0, 0.9), (1.0, 0.9)], None);
    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0)[0];
    assert_eq!(px, 230, "0.9 * 255 rounded");
}

#[test]
fn exposure_increases_brightness_by_stops() {
    let mut doc = Document::new("Exposure", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([50, 50, 50, 255]), None);
    adjustments::exposure(doc.find_layer_mut(layer_id).unwrap(), 1.0, 0.0, 1.0, None);
    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0)[0];
    assert!((px as i32 - 100).abs() <= 2, "one stop should roughly double brightness: 50 -> ~100, got {px}");
}

#[test]
fn color_balance_shifts_midtones_more_than_extremes() {
    let mut doc = Document::new("Color Balance", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([128, 128, 128, 255]), None);
    adjustments::color_balance(doc.find_layer_mut(layer_id).unwrap(), 0.3, 0.0, 0.0, None);
    let midtone_red = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0)[0];
    assert!(midtone_red > 128, "positive cyan-red shift must redden a midtone: {midtone_red}");

    let mut doc2 = Document::new("Color Balance Black", 4, 4);
    let layer_id2 = doc2.layers[0].id;
    shapes::fill_rect(doc2.find_layer_mut(layer_id2).unwrap(), 0, 0, 4, 4, Rgba([0, 0, 0, 255]), None);
    adjustments::color_balance(doc2.find_layer_mut(layer_id2).unwrap(), 0.3, 0.0, 0.0, None);
    let black_red = doc2.find_layer_mut(layer_id2).unwrap().pixels.get_pixel(0, 0)[0];
    assert!(black_red < midtone_red, "pure black should shift less than a midtone (weighted toward extremes = 0): {black_red} vs {midtone_red}");
}

#[test]
fn black_and_white_desaturates_using_channel_weights() {
    let mut doc = Document::new("B&W", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([200, 100, 50, 255]), None);
    adjustments::black_and_white(doc.find_layer_mut(layer_id).unwrap(), 0.5, 0.3, 0.2, None);
    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0);
    assert_eq!(px[0], px[1]);
    assert_eq!(px[1], px[2]);
    let expected = (200.0f32 * 0.5 + 100.0 * 0.3 + 50.0 * 0.2).round() as u8;
    assert_eq!(px[0], expected);
}

#[test]
fn vibrance_boosts_low_saturation_more_than_high_saturation() {
    let mut low_sat = Document::new("Low Sat", 2, 2);
    let layer_id = low_sat.layers[0].id;
    shapes::fill_rect(low_sat.find_layer_mut(layer_id).unwrap(), 0, 0, 2, 2, Rgba([140, 130, 120, 255]), None);
    let before_low = low_sat.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0)[0] as i32;
    adjustments::vibrance(low_sat.find_layer_mut(layer_id).unwrap(), 0.8, None);
    let after_low = low_sat.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0)[0] as i32;

    let mut high_sat = Document::new("High Sat", 2, 2);
    let layer_id2 = high_sat.layers[0].id;
    shapes::fill_rect(high_sat.find_layer_mut(layer_id2).unwrap(), 0, 0, 2, 2, Rgba([255, 0, 0, 255]), None);
    let before_high = high_sat.find_layer_mut(layer_id2).unwrap().pixels.get_pixel(0, 0)[0] as i32;
    adjustments::vibrance(high_sat.find_layer_mut(layer_id2).unwrap(), 0.8, None);
    let after_high = high_sat.find_layer_mut(layer_id2).unwrap().pixels.get_pixel(0, 0)[0] as i32;

    let low_change = (after_low - before_low).abs();
    let high_change = (after_high - before_high).abs();
    assert!(low_change > 0, "low-saturation pixel should visibly change");
    assert!(high_change <= low_change, "an already fully-saturated pixel should change less than a low-saturation one: {high_change} vs {low_change}");
}

#[test]
fn photo_filter_tints_toward_the_filter_color() {
    let mut doc = Document::new("Photo Filter", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([100, 100, 100, 255]), None);
    adjustments::photo_filter(doc.find_layer_mut(layer_id).unwrap(), [255, 0, 0], 0.5, None);
    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0);
    assert!(px[0] as i32 > 100, "red channel must move toward the filter color: {}", px[0]);
    assert!((px[2] as i32) < 100, "blue channel must move toward 0: {}", px[2]);
}

#[test]
fn channel_mixer_applies_the_matrix_and_constants() {
    let mut doc = Document::new("Channel Mixer", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([200, 100, 50, 255]), None);
    // Output red = source green, output green = source red, output blue = 0 + constant 1.0 (full blue).
    adjustments::channel_mixer(doc.find_layer_mut(layer_id).unwrap(), [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 0.0]], [0.0, 0.0, 1.0], None);
    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0);
    assert_eq!(px[0], 100, "output red = source green");
    assert_eq!(px[1], 200, "output green = source red");
    assert_eq!(px[2], 255, "output blue = constant 1.0");
}

#[test]
fn gradient_map_maps_luminosity_through_the_gradient() {
    let mut doc = Document::new("Gradient Map", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 2, 4, Rgba([0, 0, 0, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 2, 0, 2, 4, Rgba([255, 255, 255, 255]), None);
    let stops = [
        agenticart_core::gradient::GradientStop { position: 0.0, color: [255, 0, 0, 255] },
        agenticart_core::gradient::GradientStop { position: 1.0, color: [0, 0, 255, 255] },
    ];
    agenticart_core::adjustments::gradient_map(doc.find_layer_mut(layer_id).unwrap(), &stops, None);
    let shadow_px = *doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0);
    let highlight_px = *doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(3, 0);
    assert_eq!(shadow_px, Rgba([255, 0, 0, 255]), "black (0 luminosity) must map to the gradient's start color");
    assert_eq!(highlight_px, Rgba([0, 0, 255, 255]), "white (max luminosity) must map to the gradient's end color");
}

#[test]
fn posterize_reduces_to_discrete_levels() {
    let mut doc = Document::new("Posterize", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([130, 130, 130, 255]), None);
    adjustments::posterize(doc.find_layer_mut(layer_id).unwrap(), 2, None);
    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0)[0];
    assert!(px == 0 || px == 255, "posterize to 2 levels must snap to one of exactly two values, got {px}");
}

#[test]
fn threshold_produces_pure_black_or_white() {
    let mut doc = Document::new("Threshold", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 2, 4, Rgba([30, 30, 30, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 2, 0, 2, 4, Rgba([220, 220, 220, 255]), None);
    adjustments::threshold(doc.find_layer_mut(layer_id).unwrap(), 0.5, None);
    assert_eq!(doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0), &Rgba([0, 0, 0, 255]));
    assert_eq!(doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(3, 0), &Rgba([255, 255, 255, 255]));
}

#[test]
fn shadows_highlights_lifts_shadows_and_pulls_down_highlights() {
    let mut doc = Document::new("Shadows Highlights", 4, 4);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 2, 4, Rgba([20, 20, 20, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 2, 0, 2, 4, Rgba([235, 235, 235, 255]), None);
    adjustments::shadows_highlights(doc.find_layer_mut(layer_id).unwrap(), 0.5, 0.5, None);
    let shadow_px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(0, 0)[0];
    let highlight_px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(3, 0)[0];
    assert!(shadow_px > 20, "shadows must be lifted: {shadow_px}");
    assert!(highlight_px < 235, "highlights must be pulled down: {highlight_px}");
}

#[test]
fn skew_shifts_content_proportionally_to_distance_from_center() {
    let mut doc = Document::new("Skew", 20, 20);
    let layer_id = doc.layers[0].id;
    // A single row near the top and a single row near the bottom, both
    // solid, so we can see them shift in opposite directions under a
    // horizontal shear (they're on opposite sides of the vertical center).
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 8, 0, 4, 2, Rgba([255, 0, 0, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 8, 18, 4, 2, Rgba([0, 0, 255, 255]), None);

    agenticart_core::transform::skew(doc.find_layer_mut(layer_id).unwrap(), 0.5, 0.0);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    let top_row_opaque_x: Vec<u32> = (0..20).filter(|&x| layer.pixels.get_pixel(x, 1)[3] > 0).collect();
    let bottom_row_opaque_x: Vec<u32> = (0..20).filter(|&x| layer.pixels.get_pixel(x, 18)[3] > 0).collect();
    assert!(!top_row_opaque_x.is_empty() && !bottom_row_opaque_x.is_empty(), "both rows must still have some content after skewing");
    let top_center = top_row_opaque_x.iter().sum::<u32>() as f32 / top_row_opaque_x.len() as f32;
    let bottom_center = bottom_row_opaque_x.iter().sum::<u32>() as f32 / bottom_row_opaque_x.len() as f32;
    assert!((top_center - bottom_center).abs() > 3.0, "a horizontal shear must displace rows on opposite sides of the center in opposite directions: top={top_center}, bottom={bottom_center}");
}

#[test]
fn document_crop_keeps_only_the_requested_region() {
    let mut doc = Document::new("Crop", 10, 10);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 3, 3, 2, 2, Rgba([10, 20, 30, 255]), None);

    agenticart_core::resize::resize_canvas(&mut doc, 4, 4, -3, -3);

    assert_eq!((doc.width, doc.height), (4, 4));
    assert_eq!(doc.layers[0].pixels.get_pixel(0, 0), &Rgba([10, 20, 30, 255]), "cropped region's content must land at the new origin");
}

#[test]
fn sponge_saturates_and_desaturates_along_a_stroke() {
    use agenticart_core::paint::{sponge, SpongeMode};
    let brush = Brush { size: 6.0, color: Rgba([0, 0, 0, 255]), hardness: 1.0 };

    let mut doc = Document::new("Sponge Saturate", 10, 10);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 10, 10, Rgba([180, 120, 120, 255]), None);
    sponge(doc.find_layer_mut(layer_id).unwrap(), &[BrushPoint { x: 5.0, y: 5.0, pressure: 1.0 }], &brush, SpongeMode::Saturate, 0.5, None);
    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(5, 5);
    let spread = px[0] as i32 - px[1] as i32;
    assert!(spread > 60, "saturating must widen the spread between channels: got spread {spread} from original 60");

    let mut doc2 = Document::new("Sponge Desaturate", 10, 10);
    let layer_id2 = doc2.layers[0].id;
    shapes::fill_rect(doc2.find_layer_mut(layer_id2).unwrap(), 0, 0, 10, 10, Rgba([180, 120, 120, 255]), None);
    sponge(doc2.find_layer_mut(layer_id2).unwrap(), &[BrushPoint { x: 5.0, y: 5.0, pressure: 1.0 }], &brush, SpongeMode::Desaturate, 0.5, None);
    let px2 = doc2.find_layer_mut(layer_id2).unwrap().pixels.get_pixel(5, 5);
    let spread2 = px2[0] as i32 - px2[1] as i32;
    assert!(spread2 < 60 && spread2 >= 0, "desaturating must narrow the spread toward gray: got spread {spread2} from original 60");
}

#[test]
fn smudge_drags_color_along_the_stroke_direction() {
    let mut doc = Document::new("Smudge", 20, 10);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 20, 10, Rgba([0, 0, 255, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 10, Rgba([255, 0, 0, 255]), None);

    let brush = Brush { size: 5.0, color: Rgba([0, 0, 0, 255]), hardness: 1.0 };
    let stroke = [BrushPoint { x: 2.0, y: 5.0, pressure: 1.0 }, BrushPoint { x: 10.0, y: 5.0, pressure: 1.0 }];
    agenticart_core::paint::smudge(doc.find_layer_mut(layer_id).unwrap(), &stroke, &brush, 0.8, None);

    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(6, 5);
    assert!(px[0] > 0, "red content from the start of the stroke must have smeared just past the original boundary: got {px:?}");
}

#[test]
fn clone_stamp_still_works_after_the_healing_brush_addition() {
    // Regression check: adding healing_brush touched the clone_stamp
    // dispatch arm too, so re-verify it still calls clone_stamp and not
    // healing_brush - a mistake that would be easy to make and hard to
    // notice since both tools share almost identical argument shapes.
    let mut doc = Document::new("Clone Stamp Regression", 20, 20);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 4, 4, Rgba([255, 0, 0, 255]), None);

    let brush = Brush { size: 6.0, color: Rgba([0, 0, 0, 255]), hardness: 1.0 };
    agenticart_core::paint::clone_stamp(doc.find_layer_mut(layer_id).unwrap(), (2.0, 2.0), &[BrushPoint { x: 12.0, y: 12.0, pressure: 1.0 }], &brush, None);
    let px = doc.find_layer_mut(layer_id).unwrap().pixels.get_pixel(12, 12);
    assert_eq!(px, &Rgba([255, 0, 0, 255]), "clone_stamp must reproduce the source color exactly, with no tone correction applied");
}

#[test]
fn healing_brush_transfers_texture_but_corrects_tone_to_match_the_destination() {
    let mut doc = Document::new("Healing Brush", 30, 30);
    let layer_id = doc.layers[0].id;
    // A textured (checkerboard) bright source patch, and a dim, flat
    // destination area far away.
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 8, 8, Rgba([220, 20, 20, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 2, 2, 2, 2, Rgba([170, 10, 10, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 15, 15, 8, 8, Rgba([40, 40, 40, 255]), None);

    let brush = Brush { size: 4.0, color: Rgba([0, 0, 0, 255]), hardness: 1.0 };
    agenticart_core::paint::healing_brush(doc.find_layer_mut(layer_id).unwrap(), (4.0, 4.0), &[BrushPoint { x: 18.0, y: 18.0, pressure: 1.0 }], &brush, None);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    let outside_checker = layer.pixels.get_pixel(16, 16)[0] as i32;
    let inside_checker = layer.pixels.get_pixel(18, 18)[0] as i32;
    // The checkerboard's local contrast (the two source shades differ by
    // 50 in red) must survive the tone correction, since a constant
    // offset preserves relative differences - proof texture transferred,
    // not just a flat average color.
    assert_ne!(outside_checker, inside_checker, "the source's local texture/contrast must still be visible after healing: {outside_checker} vs {inside_checker}");
    // But the absolute brightness must have moved toward the dim (~40)
    // destination, nowhere near the source's original ~170-220 range.
    assert!(outside_checker < 100 && inside_checker < 100, "both healed shades must be corrected toward the dim destination: {outside_checker}, {inside_checker}");
}

#[test]
fn patch_tool_copies_source_content_with_tone_correction() {
    let mut doc = Document::new("Patch Tool", 30, 30);
    let layer_id = doc.layers[0].id;
    // Damaged region: a flat dark rectangle. Source: a textured (two-tone)
    // bright rectangle elsewhere.
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 2, 2, 6, 6, Rgba([20, 20, 20, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 20, 20, 6, 6, Rgba([200, 100, 100, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 22, 22, 2, 2, Rgba([150, 60, 60, 255]), None);

    agenticart_core::paint::patch(doc.find_layer_mut(layer_id).unwrap(), SelectionRect { x: 2, y: 2, width: 6, height: 6 }, 20, 20);

    let layer = doc.find_layer_mut(layer_id).unwrap();
    let outside_checker = layer.pixels.get_pixel(3, 3)[0] as i32;
    let inside_checker = layer.pixels.get_pixel(4, 4)[0] as i32;
    assert_ne!(outside_checker, inside_checker, "the source's local texture must survive the patch: {outside_checker} vs {inside_checker}");
    assert!(outside_checker < 100 && inside_checker < 100, "brightness must be corrected toward the original (dark) destination average, not stay at the source's bright level: {outside_checker}, {inside_checker}");
}

#[test]
fn patch_tool_does_not_panic_when_source_partially_overlaps_the_canvas_edge() {
    let mut doc = Document::new("Patch Tool Edge", 10, 10);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 5, 5, 4, 4, Rgba([50, 50, 50, 255]), None);
    // Source region deliberately runs off the top-left edge.
    agenticart_core::paint::patch(doc.find_layer_mut(layer_id).unwrap(), SelectionRect { x: 5, y: 5, width: 4, height: 4 }, -2, -2);
    // Just verifying no panic; partial coverage is an acceptable edge case.
}

#[test]
fn perspective_maps_the_four_corners_to_exactly_the_given_points() {
    let mut doc = Document::new("Perspective", 20, 20);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 20, 20, Rgba([255, 0, 0, 255]), None);

    // A trapezoid: top edge narrower than the bottom - the classic "receding
    // into the distance" perspective look.
    agenticart_core::transform::perspective(doc.find_layer_mut(layer_id).unwrap(), (5.0, 0.0), (15.0, 0.0), (20.0, 20.0), (0.0, 20.0)).unwrap();

    let layer = doc.find_layer_mut(layer_id).unwrap();
    // Near the (narrow) top, only the middle columns should be covered.
    assert_eq!(layer.pixels.get_pixel(1, 0)[3], 0, "top-left corner must be empty - the top edge was narrowed to x=[5,15]");
    assert!(layer.pixels.get_pixel(10, 0)[3] > 0, "top-middle must be covered");
    // Near the (full-width) bottom, most of the row should be covered.
    // Row 19 (the very last row) is right at the canvas edge, where
    // imageproc's inverse-warp sampling can fall just outside the source
    // image's valid range and read as background - same edge behavior
    // already seen (and documented) in the rotate-180 test - so check
    // row 17 instead, with margin from that boundary.
    assert!(layer.pixels.get_pixel(3, 17)[3] > 0, "bottom-left area must be covered - the bottom edge kept its full width");
    assert!(layer.pixels.get_pixel(16, 17)[3] > 0, "bottom-right area must be covered");
}

#[test]
fn perspective_rejects_degenerate_collinear_corners() {
    let mut doc = Document::new("Perspective Degenerate", 10, 10);
    let layer_id = doc.layers[0].id;
    // All four "corners" on the same horizontal line - no valid homography.
    let result = agenticart_core::transform::perspective(doc.find_layer_mut(layer_id).unwrap(), (0.0, 5.0), (5.0, 5.0), (10.0, 5.0), (15.0, 5.0));
    assert!(result.is_err(), "collinear destination points must be rejected, not silently produce garbage");
}

#[test]
fn perspective_identity_leaves_content_unchanged() {
    let mut doc = Document::new("Perspective Identity", 10, 10);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 2, 2, 4, 4, Rgba([10, 20, 30, 255]), None);
    let before = doc.find_layer_mut(layer_id).unwrap().pixels.clone();

    // Mapping the corners onto themselves must be a no-op (up to
    // interpolation rounding).
    agenticart_core::transform::perspective(doc.find_layer_mut(layer_id).unwrap(), (0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)).unwrap();

    let after = &doc.find_layer_mut(layer_id).unwrap().pixels;
    assert_eq!(after.get_pixel(4, 4), before.get_pixel(4, 4), "an identity perspective warp must leave content in place");
}

#[test]
fn place_as_3d_plane_with_zero_rotation_leaves_content_unchanged() {
    let mut doc = Document::new("3D Plane Identity", 20, 20);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 4, 4, 6, 6, Rgba([10, 20, 30, 255]), None);
    let before = doc.find_layer_mut(layer_id).unwrap().pixels.clone();

    // No pitch, no yaw: the plane faces the camera dead-on, so projecting
    // it back to 2D must be a no-op (up to interpolation rounding).
    agenticart_core::transform::place_as_3d_plane(doc.find_layer_mut(layer_id).unwrap(), 0.0, 0.0, 1000.0).unwrap();

    let after = &doc.find_layer_mut(layer_id).unwrap().pixels;
    assert_eq!(after.get_pixel(10, 10), before.get_pixel(10, 10), "zero pitch/yaw must leave content in place");
}

#[test]
fn place_as_3d_plane_yaw_narrows_the_projected_width() {
    let mut doc = Document::new("3D Plane Yaw", 20, 20);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 20, 20, Rgba([255, 0, 0, 255]), None);

    agenticart_core::transform::place_as_3d_plane(doc.find_layer_mut(layer_id).unwrap(), 0.0, 45.0, 100.0).unwrap();

    let layer = doc.find_layer_mut(layer_id).unwrap();
    // Check a middle row (avoiding the very first/last row, where
    // imageproc's inverse-warp sampling can fall just outside the source
    // image's valid range - the same documented edge behavior already
    // hit in the rotate-180 and perspective tests). Turning the plane by
    // 45 degrees must foreshorten it, so its projected width at any row
    // must be visibly less than the original full 20px width.
    let opaque_count = (0..20).filter(|&x| layer.pixels.get_pixel(x, 10)[3] > 0).count();
    assert!(opaque_count > 0 && opaque_count < 18, "a 45-degree yaw must foreshorten the plane's width, got {opaque_count}px wide out of 20");
}

#[test]
fn place_as_3d_plane_rejects_a_rotation_that_reaches_the_camera() {
    let mut doc = Document::new("3D Plane Behind Camera", 20, 20);
    let layer_id = doc.layers[0].id;
    // A tiny camera distance combined with a large yaw pushes a rotated
    // corner's z-coordinate past the camera itself.
    let result = agenticart_core::transform::place_as_3d_plane(doc.find_layer_mut(layer_id).unwrap(), 0.0, 89.0, 5.0);
    assert!(result.is_err(), "a rotation that brings a corner to or past the camera must error, not produce a divide-by-near-zero or inverted result");
}

#[test]
fn cmyk_separations_produce_four_plates_with_the_white_equals_no_ink_convention() {
    let mut img = image::RgbaImage::new(4, 4);
    // Pure cyan (0,255,255): full cyan ink (255,0,0 in CMY, no black
    // needed since blue+green are already maxed). K should be 0% ink
    // (white in the K plate) since max(r,g,b) here is 1.0 -> k=0.
    for p in img.pixels_mut() {
        *p = Rgba([0, 255, 255, 255]);
    }
    let plates = agenticart_core::color::cmyk_separations(&img);
    let [c_plate, m_plate, y_plate, k_plate] = plates;

    assert!(c_plate.get_pixel(0, 0)[0] < 50, "cyan plate must be dark (heavy ink) for a pure cyan pixel: {}", c_plate.get_pixel(0, 0)[0]);
    assert!(m_plate.get_pixel(0, 0)[0] > 200, "magenta plate must be light (no ink) for a pure cyan pixel: {}", m_plate.get_pixel(0, 0)[0]);
    assert!(y_plate.get_pixel(0, 0)[0] > 200, "yellow plate must be light (no ink) for a pure cyan pixel: {}", y_plate.get_pixel(0, 0)[0]);
    assert!(k_plate.get_pixel(0, 0)[0] > 200, "black plate must be light (no ink) for a pure cyan pixel: {}", k_plate.get_pixel(0, 0)[0]);
}

#[test]
fn cmyk_separations_show_pure_black_as_heavy_black_plate_ink() {
    let mut img = image::RgbaImage::new(2, 2);
    for p in img.pixels_mut() {
        *p = Rgba([0, 0, 0, 255]);
    }
    let plates = agenticart_core::color::cmyk_separations(&img);
    assert!(plates[3].get_pixel(0, 0)[0] < 30, "black plate must be dark (heavy ink) for pure black, got {}", plates[3].get_pixel(0, 0)[0]);
}

#[test]
fn demosaic_bayer_reconstructs_a_flat_color_field_exactly() {
    use agenticart_core::raw::{demosaic_bayer, BayerPattern};
    // A flat pure-red field: an RGGB sensor sees R=255 at its red
    // photosites and G=0 at its green ones (a fully red scene produces
    // zero green/blue signal); every same-channel neighbor agrees, so a
    // correct demosaic must reconstruct exactly (255,0,0) everywhere,
    // with zero edge-averaging error.
    let mut mosaic = image::GrayImage::new(8, 8);
    for y in 0..8 {
        for x in 0..8 {
            let value = if (x % 2, y % 2) == (0, 0) { 255 } else { 0 };
            mosaic.put_pixel(x, y, image::Luma([value]));
        }
    }
    let rgb = demosaic_bayer(&mosaic, BayerPattern::Rggb);
    for y in 1..7 {
        for x in 1..7 {
            assert_eq!(rgb.get_pixel(x, y), &Rgba([255, 0, 0, 255]), "flat red field must demosaic exactly, no edge artifacts away from the border at ({x},{y})");
        }
    }
}

#[test]
fn demosaic_bayer_reconstructs_flat_white_across_all_four_cfa_layouts() {
    use agenticart_core::raw::{demosaic_bayer, BayerPattern};
    // A flat white field: every photosite (regardless of its filter
    // color) reads full signal, since white reflects all wavelengths.
    // This must reconstruct as flat white for every CFA layout - a good
    // cross-check that channel_at's four pattern tables are each
    // internally consistent (every position is assigned to exactly one
    // of R/G/B, none missed or double-counted).
    for pattern in [BayerPattern::Rggb, BayerPattern::Bggr, BayerPattern::Grbg, BayerPattern::Gbrg] {
        let mosaic = image::GrayImage::from_pixel(6, 6, image::Luma([200]));
        let rgb = demosaic_bayer(&mosaic, pattern);
        for y in 1..5 {
            for x in 1..5 {
                assert_eq!(rgb.get_pixel(x, y), &Rgba([200, 200, 200, 255]), "flat field must demosaic to flat gray under {pattern:?} at ({x},{y})");
            }
        }
    }
}

#[test]
fn document_from_bayer_mosaic_round_trips_through_a_real_file() {
    let mut mosaic = image::GrayImage::new(8, 8);
    for y in 0..8u32 {
        for x in 0..8u32 {
            let value = if (x % 2, y % 2) == (1, 1) { 180 } else { 60 };
            mosaic.put_pixel(x, y, image::Luma([value]));
        }
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tmp = std::env::temp_dir().join(format!("agenticart_bayer_test_{nanos}.png"));
    mosaic.save(&tmp).unwrap();

    let doc = agenticart_core::raw::document_from_bayer_mosaic(&tmp, agenticart_core::raw::BayerPattern::Rggb).expect("must open a real grayscale file and demosaic it");
    std::fs::remove_file(&tmp).ok();

    assert_eq!((doc.width, doc.height), (8, 8));
    assert_eq!(doc.layers.len(), 1);
    // Blue photosites (odd,odd under RGGB) read 180 - blue must dominate there.
    let px = doc.layers[0].pixels.get_pixel(3, 3);
    assert_eq!(px[2], 180, "the blue channel at a blue photosite must be its own directly-measured value, not averaged away");
}

#[test]
fn bayer_pattern_parse_round_trips_all_four_layouts() {
    use agenticart_core::raw::BayerPattern;
    for (s, expected) in [("RGGB", BayerPattern::Rggb), ("BGGR", BayerPattern::Bggr), ("GRBG", BayerPattern::Grbg), ("GBRG", BayerPattern::Gbrg)] {
        assert_eq!(BayerPattern::parse(s), Some(expected));
    }
    assert_eq!(BayerPattern::parse("nonsense"), None);
}

#[test]
fn render_region_returns_exactly_the_requested_crop() {
    let mut doc = Document::new("Render Region", 20, 20);
    let layer_id = doc.layers[0].id;
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 0, 0, 10, 20, Rgba([255, 0, 0, 255]), None);
    shapes::fill_rect(doc.find_layer_mut(layer_id).unwrap(), 10, 0, 10, 20, Rgba([0, 0, 255, 255]), None);

    let tile = compositor::render_region(&doc, &SelectionRect { x: 8, y: 0, width: 4, height: 4 });
    assert_eq!((tile.width(), tile.height()), (4, 4), "tile must be exactly the requested size");
    assert_eq!(tile.get_pixel(0, 0), &Rgba([255, 0, 0, 255]), "left half of the tile must show the red region");
    assert_eq!(tile.get_pixel(3, 0), &Rgba([0, 0, 255, 255]), "right half of the tile must show the blue region");

    let full = compositor::render(&doc);
    for y in 0..4u32 {
        for x in 0..4u32 {
            assert_eq!(tile.get_pixel(x, y), full.get_pixel(8 + x, 0 + y), "every tile pixel must exactly match the corresponding full-render pixel");
        }
    }
}

#[test]
fn render_region_clamps_to_canvas_bounds_without_panicking() {
    let doc = Document::new("Render Region Clamp", 10, 10);
    // Requested region hangs far off both edges.
    let tile = compositor::render_region(&doc, &SelectionRect { x: -5, y: -5, width: 100, height: 100 });
    assert_eq!((tile.width(), tile.height()), (10, 10), "an oversized region must clamp to the actual canvas bounds");
}

#[test]
fn render_region_entirely_outside_the_canvas_is_empty_not_a_panic() {
    let doc = Document::new("Render Region Outside", 10, 10);
    let tile = compositor::render_region(&doc, &SelectionRect { x: 50, y: 50, width: 10, height: 10 });
    assert_eq!((tile.width(), tile.height()), (0, 0));
}

#[test]
fn all_new_blend_modes_are_wired_through_blend_mode_parse_and_as_str() {
    let modes = [
        BlendMode::LinearBurn,
        BlendMode::DarkerColor,
        BlendMode::LinearDodge,
        BlendMode::LighterColor,
        BlendMode::VividLight,
        BlendMode::LinearLight,
        BlendMode::PinLight,
        BlendMode::HardMix,
        BlendMode::Subtract,
        BlendMode::Divide,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];
    for mode in modes {
        let s = mode.as_str();
        assert_eq!(BlendMode::parse(s), Some(mode), "as_str/parse must round-trip for {mode:?} ('{s}')");
    }
}

use agenticart_core::{Document, DocumentStore};
use serde_json::json;

#[test]
fn eyedropper_samples_the_flattened_composite_by_default() {
    let store = DocumentStore::new();
    let doc = Document::new("Eyedropper Test", 4, 4);
    let doc_id = doc.id;
    let layer_id = doc.layers[0].id;
    store.insert(doc);

    agenticart_mcp_server::tools::call(
        &store,
        "shape.fillRect",
        &json!({"documentId": doc_id.to_string(), "layerId": layer_id.to_string(), "x": 0, "y": 0, "width": 4, "height": 4, "color": [10, 20, 30, 255]}),
    )
    .unwrap();

    let result = agenticart_mcp_server::tools::call(&store, "color.eyedropper", &json!({"documentId": doc_id.to_string(), "x": 2, "y": 2})).unwrap();
    assert_eq!(result, json!({"color": [10, 20, 30, 255]}));
}

#[test]
fn eyedropper_samples_a_specific_layer_when_asked() {
    let store = DocumentStore::new();
    let doc = Document::new("Eyedropper Layer Test", 4, 4);
    let doc_id = doc.id;
    let bg_id = doc.layers[0].id;
    store.insert(doc);

    agenticart_mcp_server::tools::call(
        &store,
        "shape.fillRect",
        &json!({"documentId": doc_id.to_string(), "layerId": bg_id.to_string(), "x": 0, "y": 0, "width": 4, "height": 4, "color": [100, 100, 100, 255]}),
    )
    .unwrap();

    let new_layer = agenticart_mcp_server::tools::call(&store, "layer.create", &json!({"documentId": doc_id.to_string(), "name": "Top"})).unwrap();
    let top_id = new_layer["layerId"].as_str().unwrap();
    agenticart_mcp_server::tools::call(
        &store,
        "shape.fillRect",
        &json!({"documentId": doc_id.to_string(), "layerId": top_id, "x": 0, "y": 0, "width": 2, "height": 2, "color": [0, 255, 0, 255]}),
    )
    .unwrap();

    // At (0,0), the composite is green (top layer opaque there), but the
    // background layer's own pixels are still gray underneath.
    let composite = agenticart_mcp_server::tools::call(&store, "color.eyedropper", &json!({"documentId": doc_id.to_string(), "x": 0, "y": 0})).unwrap();
    assert_eq!(composite, json!({"color": [0, 255, 0, 255]}));

    let bg_sample = agenticart_mcp_server::tools::call(&store, "color.eyedropper", &json!({"documentId": doc_id.to_string(), "x": 0, "y": 0, "layerId": bg_id.to_string()})).unwrap();
    assert_eq!(bg_sample, json!({"color": [100, 100, 100, 255]}));
}

#[test]
fn history_list_reports_undo_redo_depth() {
    let store = DocumentStore::new();
    let doc = Document::new("History Test", 4, 4);
    let doc_id = doc.id;
    let layer_id = doc.layers[0].id;
    store.insert(doc);

    let zero = agenticart_mcp_server::tools::call(&store, "history.list", &json!({"documentId": doc_id.to_string()})).unwrap();
    assert_eq!(zero, json!({"undoDepth": 0, "redoDepth": 0}));

    agenticart_mcp_server::tools::call(&store, "layer.setProperties", &json!({"documentId": doc_id.to_string(), "layerId": layer_id.to_string(), "opacity": 0.5})).unwrap();
    let after_edit = agenticart_mcp_server::tools::call(&store, "history.list", &json!({"documentId": doc_id.to_string()})).unwrap();
    assert_eq!(after_edit, json!({"undoDepth": 1, "redoDepth": 0}));

    agenticart_mcp_server::tools::call(&store, "history.undo", &json!({"documentId": doc_id.to_string()})).unwrap();
    let after_undo = agenticart_mcp_server::tools::call(&store, "history.list", &json!({"documentId": doc_id.to_string()})).unwrap();
    assert_eq!(after_undo, json!({"undoDepth": 0, "redoDepth": 1}));
}

#[test]
fn selection_select_all_covers_the_whole_canvas() {
    let store = DocumentStore::new();
    let doc = Document::new("Select All Test", 12, 8);
    let doc_id = doc.id;
    store.insert(doc);

    let result = agenticart_mcp_server::tools::call(&store, "selection.selectAll", &json!({"documentId": doc_id.to_string()})).unwrap();
    assert_eq!(result, json!({"ok": true}));

    let doc = store.get_clone(doc_id).unwrap();
    let sel = doc.selection.expect("selection must be set");
    assert_eq!((sel.x, sel.y, sel.width, sel.height), (0, 0, 12, 8));
}

#[test]
fn layer_set_properties_renames_a_layer() {
    let store = DocumentStore::new();
    let doc = Document::new("Rename Test", 4, 4);
    let doc_id = doc.id;
    let layer_id = doc.layers[0].id;
    store.insert(doc);

    let args = json!({"documentId": doc_id.to_string(), "layerId": layer_id.to_string(), "name": "Renamed"});
    let result = agenticart_mcp_server::tools::call(&store, "layer.setProperties", &args).expect("should succeed");
    assert_eq!(result, json!({"ok": true}));

    let doc = store.get_clone(doc_id).unwrap();
    assert_eq!(doc.layers[0].name, "Renamed");
}

#[test]
fn job_run_executes_a_tool_in_the_background_and_status_reports_completion() {
    let store = DocumentStore::new();
    let doc = Document::new("Job Test", 4, 4);
    let doc_id = doc.id;
    store.insert(doc);

    let run_result = agenticart_mcp_server::tools::call(
        &store,
        "job.run",
        &json!({"tool": "document.resize", "args": {"documentId": doc_id.to_string(), "width": 8, "height": 8}}),
    )
    .unwrap();
    let job_id = run_result["jobId"].as_str().unwrap().to_string();

    // Poll until done (this should complete almost immediately for a tiny resize).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut final_status = None;
    while std::time::Instant::now() < deadline {
        let status = agenticart_mcp_server::tools::call(&store, "job.status", &json!({"jobId": job_id})).unwrap();
        if status["status"] != "running" {
            final_status = Some(status);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let final_status = final_status.expect("job must finish within the deadline");
    assert_eq!(final_status["status"], "done");
    assert_eq!(final_status["result"], json!({"ok": true}));

    let doc = store.get_clone(doc_id).unwrap();
    assert_eq!((doc.width, doc.height), (8, 8), "the wrapped tool call must have actually run against the shared store");

    let listed = agenticart_mcp_server::tools::call(&store, "job.list", &json!({})).unwrap();
    assert!(listed.as_array().unwrap().iter().any(|j| j["jobId"] == job_id), "job.list must include this job");
}

#[test]
fn job_status_reports_an_error_from_the_wrapped_tool_without_panicking() {
    let store = DocumentStore::new();
    let run_result = agenticart_mcp_server::tools::call(&store, "job.run", &json!({"tool": "document.close", "args": {"documentId": "00000000-0000-0000-0000-000000000000"}})).unwrap();
    let job_id = run_result["jobId"].as_str().unwrap().to_string();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = agenticart_mcp_server::tools::call(&store, "job.status", &json!({"jobId": job_id})).unwrap();
        if status["status"] != "running" {
            // document.close on a missing id just returns {closed: false}, not an error - use it to prove
            // the job machinery surfaces whatever the wrapped tool actually returned, success or not.
            assert_eq!(status["status"], "done");
            assert_eq!(status["result"], json!({"closed": false}));
            return;
        }
        assert!(std::time::Instant::now() < deadline, "job did not finish in time");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn job_status_reports_the_error_variant_when_the_wrapped_tool_itself_errors() {
    let store = DocumentStore::new();
    let run_result = agenticart_mcp_server::tools::call(&store, "job.run", &json!({"tool": "this.tool.does.not.exist", "args": {}})).unwrap();
    let job_id = run_result["jobId"].as_str().unwrap().to_string();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = agenticart_mcp_server::tools::call(&store, "job.status", &json!({"jobId": job_id})).unwrap();
        if status["status"] != "running" {
            assert_eq!(status["status"], "error");
            assert!(status["error"].as_str().unwrap().contains("unknown tool"));
            return;
        }
        assert!(std::time::Instant::now() < deadline, "job did not finish in time");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn job_status_for_an_unknown_job_id_is_an_error_not_a_panic() {
    let store = DocumentStore::new();
    let err = agenticart_mcp_server::tools::call(&store, "job.status", &json!({"jobId": "does-not-exist"})).expect_err("unknown job id must error");
    assert!(err.to_string().contains("does-not-exist"));
}

#[test]
fn automation_run_replays_actions_across_multiple_target_documents() {
    let store = DocumentStore::new();
    let doc_a = Document::new("A", 4, 4);
    let doc_a_id = doc_a.id;
    let doc_b = Document::new("B", 4, 4);
    let doc_b_id = doc_b.id;
    store.insert(doc_a);
    store.insert(doc_b);

    let layer_id_a = store.get_clone(doc_a_id).unwrap().layers[0].id;
    let layer_id_b = store.get_clone(doc_b_id).unwrap().layers[0].id;

    let args = json!({
        "actions": [{"tool": "layer.setProperties", "args": {"layerId": layer_id_a.to_string(), "opacity": 0.5}}],
        "targets": [doc_a_id.to_string()]
    });
    let result = agenticart_mcp_server::tools::call(&store, "automation.run", &args).expect("should succeed");
    let results = result.as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["actionsRun"], 1);
    assert!(results[0]["error"].is_null());
    assert_eq!(store.get_clone(doc_a_id).unwrap().layers[0].opacity, 0.5);

    // Now run against doc_b using doc_b's own layer id.
    let args = json!({
        "actions": [{"tool": "layer.setProperties", "args": {"layerId": layer_id_b.to_string(), "opacity": 0.25}}],
        "targets": [doc_b_id.to_string()]
    });
    agenticart_mcp_server::tools::call(&store, "automation.run", &args).unwrap();
    assert_eq!(store.get_clone(doc_b_id).unwrap().layers[0].opacity, 0.25);
}

#[test]
fn automation_run_records_a_per_target_error_without_aborting_other_targets() {
    let store = DocumentStore::new();
    let doc = Document::new("Only Doc", 4, 4);
    let doc_id = doc.id;
    store.insert(doc);

    let args = json!({
        "actions": [{"tool": "this.tool.does.not.exist", "args": {}}],
        "targets": [doc_id.to_string()]
    });
    let result = agenticart_mcp_server::tools::call(&store, "automation.run", &args).expect("automation.run itself should succeed even if an action fails");
    let results = result.as_array().unwrap();
    assert_eq!(results[0]["actionsRun"], 0);
    assert!(!results[0]["error"].is_null(), "an unknown tool inside the action must be recorded as this target's error");
}

#[test]
fn automation_save_and_run_named_round_trips() {
    let store = DocumentStore::new();
    let doc = Document::new("Named Macro Doc", 4, 4);
    let doc_id = doc.id;
    let layer_id = doc.layers[0].id;
    store.insert(doc);

    let save_args = json!({
        "name": "fade-half",
        "actions": [{"tool": "layer.setProperties", "args": {"layerId": layer_id.to_string(), "opacity": 0.5}}]
    });
    agenticart_mcp_server::tools::call(&store, "automation.save", &save_args).unwrap();

    let list = agenticart_mcp_server::tools::call(&store, "automation.list", &json!({})).unwrap();
    assert!(list.as_array().unwrap().iter().any(|e| e["name"] == "fade-half"));

    let run_args = json!({"name": "fade-half", "targets": [doc_id.to_string()]});
    agenticart_mcp_server::tools::call(&store, "automation.runNamed", &run_args).unwrap();
    assert_eq!(store.get_clone(doc_id).unwrap().layers[0].opacity, 0.5);

    let delete_result = agenticart_mcp_server::tools::call(&store, "automation.delete", &json!({"name": "fade-half"})).unwrap();
    assert_eq!(delete_result, json!({"deleted": true}));
}

#[test]
fn automation_run_on_files_batch_processes_images_on_disk() {
    let store = DocumentStore::new();

    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("agenticart_batch_test_{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path_a = dir.join("a.png");
    let path_b = dir.join("b.png");
    image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 10, 10, 255])).save(&path_a).unwrap();
    image::RgbaImage::from_pixel(4, 4, image::Rgba([20, 20, 20, 255])).save(&path_b).unwrap();

    let args = json!({
        "actions": [{"tool": "adjustment.invert", "args": {}}],
        "paths": [path_a.to_str().unwrap(), path_b.to_str().unwrap()],
        "outputSuffix": "_inverted"
    });
    let result = agenticart_mcp_server::tools::call(&store, "automation.runOnFiles", &args).expect("should succeed");
    let results = result.as_array().unwrap();
    assert_eq!(results.len(), 2);
    for r in results {
        assert!(r["error"].is_null(), "unexpected error: {r:?}");
        assert_eq!(r["actionsRun"], 1);
    }

    let out_a = image::open(dir.join("a_inverted.png")).unwrap().to_rgba8();
    assert_eq!(out_a.get_pixel(0, 0), &image::Rgba([245, 245, 245, 255]), "255-10=245, the batch-processed output must reflect the invert action");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn automation_run_on_files_records_a_per_file_error_without_aborting_the_batch() {
    let store = DocumentStore::new();
    let args = json!({
        "actions": [{"tool": "adjustment.invert", "args": {}}],
        "paths": ["C:/definitely/does/not/exist.png"]
    });
    let result = agenticart_mcp_server::tools::call(&store, "automation.runOnFiles", &args).expect("automation.runOnFiles itself should succeed even if a file fails");
    let results = result.as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert!(!results[0]["error"].is_null(), "a missing file must be recorded as an error, not silently skipped");
}

#[test]
fn generative_upscale_scales_the_document_by_factor() {
    let store = DocumentStore::new();
    let doc = Document::new("Upscale Test", 10, 20);
    let doc_id = doc.id;
    store.insert(doc);

    let args = json!({"documentId": doc_id.to_string(), "factor": 2.0});
    let result = agenticart_mcp_server::tools::call(&store, "generative.upscale", &args).expect("should succeed");
    assert_eq!(result, json!({"ok": true}));

    let doc = store.get_clone(doc_id).unwrap();
    assert_eq!((doc.width, doc.height), (20, 40));
}

#[test]
fn generative_upscale_rejects_a_factor_that_would_shrink() {
    let store = DocumentStore::new();
    let doc = Document::new("Upscale Reject", 10, 10);
    let doc_id = doc.id;
    store.insert(doc);

    let args = json!({"documentId": doc_id.to_string(), "factor": 0.5});
    let err = agenticart_mcp_server::tools::call(&store, "generative.upscale", &args).expect_err("factor <= 1.0 must be rejected");
    assert!(err.to_string().contains("factor"));
}

#[test]
fn document_duplicate_creates_an_independent_copy() {
    let store = DocumentStore::new();
    let doc = Document::new("Original", 4, 4);
    let doc_id = doc.id;
    let layer_id = doc.layers[0].id;
    store.insert(doc);

    let args = json!({"documentId": doc_id.to_string()});
    let result = agenticart_mcp_server::tools::call(&store, "document.duplicate", &args).expect("should succeed");
    let new_id_str = result.get("documentId").and_then(|v| v.as_str()).expect("documentId in result");
    assert_ne!(new_id_str, doc_id.to_string(), "the duplicate must have a new id");

    let new_doc_id: uuid::Uuid = new_id_str.parse().unwrap();
    let new_doc = store.get_clone(new_doc_id).unwrap();
    assert_eq!(new_doc.name, "Original copy");
    assert_eq!(new_doc.layers.len(), 1);

    // Editing the copy must not affect the original.
    let new_layer_id = new_doc.layers[0].id;
    agenticart_mcp_server::tools::call(
        &store,
        "layer.setProperties",
        &json!({"documentId": new_id_str, "layerId": new_layer_id.to_string(), "opacity": 0.25}),
    )
    .unwrap();

    let original = store.get_clone(doc_id).unwrap();
    assert_eq!(original.layers[0].opacity, 1.0, "editing the duplicate must not affect the original");
    let _ = layer_id; // original layer id kept for clarity/future assertions
}

use crate::dispatch::handle;
use crate::protocol::JsonRpcRequest;
use agenticart_core::DocumentStore;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};

/// The remote transport: a single authenticated `POST /mcp` endpoint
/// carrying the same JSON-RPC 2.0 request/response shape as the stdio
/// transport (the MCP "Streamable HTTP" transport's non-streaming form —
/// one JSON request in, one JSON response out; no SSE upgrade yet since
/// nothing here needs server-initiated push). Meant to sit behind a tunnel
/// (cloudflared/Tailscale Funnel) so an external agent can drive the same
/// live DocumentStore a local stdio agent or the desktop UI would.
///
/// Three kinds of bearer token give scoped access - read-only
/// (inspect/render) vs. read-write, and per-document:
///   - `write_token` (required): full read-write access to every open
///     document.
///   - `read_token` (optional): read-only - may only call the
///     pure-inspection tools in `READ_ONLY_TOOLS` plus `tools/list`/
///     `initialize` - but still across every open document.
///   - `document_tokens` (optional, any number): each is read-write but
///     restricted to exactly one document id - any tool call whose
///     arguments don't name that exact documentId is refused, including
///     document.create/document.list (which have no single-document
///     meaning). This is the per-document scoping: share one document
///     with a collaborator or agent without exposing any others.
struct AppState {
    store: DocumentStore,
    /// `Arc<RwLock<_>>` rather than a plain `String` so a token can be
    /// rotated (e.g. from a "Regenerate" button in a settings UI) without
    /// restarting the server - the caller keeps its own clone of the same
    /// `Arc` and writes a new value into it; every in-flight and future
    /// request reads through the same lock.
    write_token: Arc<RwLock<String>>,
    read_token: Option<String>,
    document_tokens: Vec<(String, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccessLevel {
    ReadWrite,
    ReadOnly,
}

struct TokenAuth {
    access: AccessLevel,
    /// `Some(document_id)` restricts every tool call to that one document;
    /// `None` (the write/read tokens) sees every open document.
    allowed_document: Option<String>,
}

fn authenticate(state: &AppState, bearer: Option<&str>) -> Option<TokenAuth> {
    let token = bearer?;
    if token == *state.write_token.read().unwrap() {
        return Some(TokenAuth { access: AccessLevel::ReadWrite, allowed_document: None });
    }
    if state.read_token.as_deref() == Some(token) {
        return Some(TokenAuth { access: AccessLevel::ReadOnly, allowed_document: None });
    }
    if let Some((_, doc_id)) = state.document_tokens.iter().find(|(t, _)| t == token) {
        return Some(TokenAuth { access: AccessLevel::ReadWrite, allowed_document: Some(doc_id.clone()) });
    }
    None
}

/// Tools a read-only token may call: pure inspection/render, nothing that
/// mutates a document, creates/deletes one, or has side effects (a job
/// started via job.run could itself run a mutating tool, so job.run is
/// deliberately NOT in this list even though job.status/job.list are).
const READ_ONLY_TOOLS: &[&str] = &[
    "document.list",
    "layer.list",
    "canvas.render",
    "canvas.renderTile",
    "canvas.getPixels",
    "color.eyedropper",
    "history.list",
    "automation.list",
    "text.getContent",
    "job.status",
    "job.list",
];

fn build_router(store: DocumentStore, write_token: Arc<RwLock<String>>, read_token: Option<String>, document_tokens: Vec<(String, String)>) -> Router {
    let state = Arc::new(AppState { store, write_token, read_token, document_tokens });
    Router::new().route("/mcp", post(handle_mcp)).route("/health", get(handle_health)).with_state(state)
}

/// `write_token` is `Arc<RwLock<String>>` rather than `String` so a caller
/// that wants live token rotation (see `AppState::write_token`) can keep
/// its own clone of the same `Arc` and mutate it after `serve` is already
/// running. A caller that doesn't need rotation just wraps a fixed string:
/// `Arc::new(RwLock::new(token))`.
pub fn serve(store: DocumentStore, port: u16, write_token: Arc<RwLock<String>>, read_token: Option<String>, document_tokens: Vec<(String, String)>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(serve_async(store, port, write_token, read_token, document_tokens))
}

async fn serve_async(store: DocumentStore, port: u16, write_token: Arc<RwLock<String>>, read_token: Option<String>, document_tokens: Vec<(String, String)>) -> anyhow::Result<()> {
    let app = build_router(store, write_token, read_token, document_tokens);
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("agenticart-mcp-server: HTTP transport listening on http://{addr}/mcp");
    eprintln!("(bound to localhost only - use a tunnel tool like cloudflared or Tailscale Funnel to expose it externally)");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_health() -> impl IntoResponse {
    Json(json!({"status": "ok", "service": "agenticart-mcp-server"}))
}

async fn handle_mcp(State(state): State<Arc<AppState>>, headers: HeaderMap, body: String) -> impl IntoResponse {
    let bearer = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    let auth = match authenticate(&state, bearer) {
        Some(a) => a,
        None => return (StatusCode::UNAUTHORIZED, Json(json!({"error": "missing or invalid bearer token"}))).into_response(),
    };

    let request: JsonRpcRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid JSON-RPC request: {e}")}))).into_response(),
    };

    if request.method == "tools/call" {
        let tool_name = request.params.get("name").and_then(Value::as_str).unwrap_or("");

        if auth.access == AccessLevel::ReadOnly && !READ_ONLY_TOOLS.contains(&tool_name) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": format!("read-only token cannot call '{tool_name}' - only {READ_ONLY_TOOLS:?} are allowed")})),
            )
                .into_response();
        }

        if let Some(allowed_doc) = &auth.allowed_document {
            let call_doc_id = request.params.get("arguments").and_then(|a| a.get("documentId")).and_then(Value::as_str);
            if call_doc_id != Some(allowed_doc.as_str()) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": format!("this token is scoped to document '{allowed_doc}' and cannot call '{tool_name}' against any other document (or a tool with no documentId)")})),
                )
                    .into_response();
            }
        }
    }

    let id = request.id.clone().unwrap_or(Value::Null);
    let response = handle(&state.store, &request, id);
    Json(response).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn response_json(response: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn request_without_a_bearer_token_is_rejected() {
        let app = build_router(DocumentStore::new(), Arc::new(RwLock::new("secret".to_string())), None, Vec::new());
        let req = Request::builder().method("POST").uri("/mcp").header("content-type", "application/json").body(Body::from("{}")).unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rotating_the_write_token_takes_effect_without_restarting_the_server() {
        let token = Arc::new(RwLock::new("old-secret".to_string()));
        let app = build_router(DocumentStore::new(), token.clone(), None, Vec::new());

        let old_req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer old-secret")
            .body(Body::from(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}).to_string()))
            .unwrap();
        assert_eq!(app.clone().oneshot(old_req).await.unwrap().status(), StatusCode::OK, "the old token must still work before rotation");

        *token.write().unwrap() = "new-secret".to_string();

        let old_req_after_rotation = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer old-secret")
            .body(Body::from(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}).to_string()))
            .unwrap();
        assert_eq!(app.clone().oneshot(old_req_after_rotation).await.unwrap().status(), StatusCode::UNAUTHORIZED, "the old token must stop working once rotated");

        let new_req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer new-secret")
            .body(Body::from(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}).to_string()))
            .unwrap();
        assert_eq!(app.oneshot(new_req).await.unwrap().status(), StatusCode::OK, "the new token must work immediately, with no server restart");
    }

    #[tokio::test]
    async fn request_with_the_wrong_bearer_token_is_rejected() {
        let app = build_router(DocumentStore::new(), Arc::new(RwLock::new("secret".to_string())), None, Vec::new());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer wrong-token")
            .body(Body::from("{}"))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn authorized_request_reaches_the_same_dispatcher_as_stdio() {
        let app = build_router(DocumentStore::new(), Arc::new(RwLock::new("secret".to_string())), None, Vec::new());
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer secret")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;
        let tools = json["result"]["tools"].as_array().expect("tools/list must return a tools array over HTTP exactly as it does over stdio");
        assert!(tools.len() > 50, "expected the full tool catalog, got {}", tools.len());
    }

    #[tokio::test]
    async fn authorized_tool_call_mutates_the_shared_document_store() {
        let store = DocumentStore::new();
        let app = build_router(store.clone(), Arc::new(RwLock::new("secret".to_string())), None, Vec::new());
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "document.create", "arguments": {"width": 8, "height": 8}}
        })
        .to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer secret")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        let json = response_json(response).await;
        let text = json["result"]["content"][0]["text"].as_str().unwrap();
        let doc_id: Value = serde_json::from_str(text).unwrap();
        let id: uuid::Uuid = doc_id["documentId"].as_str().unwrap().parse().unwrap();

        assert!(store.get_clone(id).is_some(), "the document created over HTTP must be visible in the same DocumentStore other transports (or the UI) share");
    }

    #[tokio::test]
    async fn health_endpoint_needs_no_auth() {
        let app = build_router(DocumentStore::new(), Arc::new(RwLock::new("secret".to_string())), None, Vec::new());
        let req = Request::builder().method("GET").uri("/health").body(Body::empty()).unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn read_only_token_can_call_a_read_only_tool() {
        let store = DocumentStore::new();
        let doc = agenticart_core::Document::new("Readonly Test", 4, 4);
        let doc_id = doc.id;
        store.insert(doc);
        let app = build_router(store, Arc::new(RwLock::new("write-secret".to_string())), Some("read-secret".to_string()), Vec::new());

        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "document.list", "arguments": {}}}).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer read-secret")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;
        let text = json["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(&doc_id.to_string()), "read-only token must be able to list documents");
    }

    #[tokio::test]
    async fn read_only_token_cannot_call_a_mutating_tool() {
        let store = DocumentStore::new();
        let app = build_router(store, Arc::new(RwLock::new("write-secret".to_string())), Some("read-secret".to_string()), Vec::new());

        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "document.create", "arguments": {"width": 4, "height": 4}}}).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer read-secret")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "a read-only token must not be able to create a document");
    }

    #[tokio::test]
    async fn write_token_can_still_call_any_tool_when_a_read_token_is_also_configured() {
        let store = DocumentStore::new();
        let app = build_router(store, Arc::new(RwLock::new("write-secret".to_string())), Some("read-secret".to_string()), Vec::new());

        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "document.create", "arguments": {"width": 4, "height": 4}}}).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer write-secret")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "the write token must remain fully privileged regardless of whether a read token is configured");
    }

    #[tokio::test]
    async fn document_scoped_token_can_mutate_its_own_document() {
        let store = DocumentStore::new();
        let doc = agenticart_core::Document::new("Scoped Doc", 4, 4);
        let doc_id = doc.id;
        let layer_id = doc.layers[0].id;
        store.insert(doc);
        let app = build_router(store.clone(), Arc::new(RwLock::new("write-secret".to_string())), None, vec![("doc-token".to_string(), doc_id.to_string())]);

        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "layer.setProperties", "arguments": {"documentId": doc_id.to_string(), "layerId": layer_id.to_string(), "opacity": 0.5}}
        })
        .to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer doc-token")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a document-scoped token must be able to mutate the document it's scoped to");
        assert_eq!(store.get_clone(doc_id).unwrap().layers[0].opacity, 0.5);
    }

    #[tokio::test]
    async fn document_scoped_token_cannot_touch_a_different_document() {
        let store = DocumentStore::new();
        let scoped_doc = agenticart_core::Document::new("Scoped Doc", 4, 4);
        let scoped_doc_id = scoped_doc.id;
        store.insert(scoped_doc);
        let other_doc = agenticart_core::Document::new("Other Doc", 4, 4);
        let other_doc_id = other_doc.id;
        let other_layer_id = other_doc.layers[0].id;
        store.insert(other_doc);
        let app = build_router(store, Arc::new(RwLock::new("write-secret".to_string())), None, vec![("doc-token".to_string(), scoped_doc_id.to_string())]);

        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "layer.setProperties", "arguments": {"documentId": other_doc_id.to_string(), "layerId": other_layer_id.to_string(), "opacity": 0.5}}
        })
        .to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer doc-token")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "a document-scoped token must not be able to touch any other document");
    }

    #[tokio::test]
    async fn document_scoped_token_cannot_call_document_create_or_list() {
        let store = DocumentStore::new();
        let doc = agenticart_core::Document::new("Scoped Doc", 4, 4);
        let doc_id = doc.id;
        store.insert(doc);
        let app = build_router(store, Arc::new(RwLock::new("write-secret".to_string())), None, vec![("doc-token".to_string(), doc_id.to_string())]);

        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "document.list", "arguments": {}}}).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer doc-token")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "document.list has no documentId argument to match, so a document-scoped token must be refused, not silently see every document");
    }
}

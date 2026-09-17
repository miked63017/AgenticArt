use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::tools;
use agenticart_core::DocumentStore;
use serde_json::{json, Value};

/// Single JSON-RPC method dispatcher shared by every transport (stdio,
/// HTTP+SSE). All transports resolve to the same `tools::call` against the
/// same `DocumentStore`, so a request that arrives over a tunnel from a
/// remote agent has identical semantics to one piped in over stdio.
pub fn handle(store: &DocumentStore, request: &JsonRpcRequest, id: Value) -> JsonRpcResponse {
    match request.method.as_str() {
        "initialize" => JsonRpcResponse::ok(
            id,
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "agenticart-mcp-server", "version": env!("CARGO_PKG_VERSION")}
            }),
        ),
        "tools/list" => {
            let tools: Vec<Value> = tools::catalog()
                .into_iter()
                .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.input_schema}))
                .collect();
            JsonRpcResponse::ok(id, json!({"tools": tools}))
        }
        "tools/call" => {
            let name = match request.params.get("name").and_then(Value::as_str) {
                Some(n) => n,
                None => return JsonRpcResponse::err(id, -32602, "missing 'name'"),
            };
            let empty = json!({});
            let args = request.params.get("arguments").unwrap_or(&empty);
            match tools::call(store, name, args) {
                Ok(result) => JsonRpcResponse::ok(
                    id,
                    json!({
                        "content": [{"type": "text", "text": result.to_string()}],
                        "isError": false
                    }),
                ),
                Err(e) => JsonRpcResponse::ok(
                    id,
                    json!({
                        "content": [{"type": "text", "text": format!("{e:#}")}],
                        "isError": true
                    }),
                ),
            }
        }
        other => JsonRpcResponse::err(id, -32601, format!("method not found: {other}")),
    }
}

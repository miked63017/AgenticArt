use agenticart_core::DocumentStore;
use agenticart_mcp_server::{dispatch, http, protocol::JsonRpcRequest};
use std::io::{self, BufRead, Write};
use std::sync::{Arc, RwLock};

/// AgenticArt's MCP server. Two transports share one dispatcher
/// (`dispatch::handle`) and tool catalog (`tools::call`):
///   - stdio (default): newline-delimited JSON-RPC 2.0, for a local agent
///     process (e.g. spawned by an editor or CLI agent).
///   - HTTP (`--http-port <PORT> [--token <TOKEN>]`): a single authenticated
///     endpoint for remote agents reached through a tunnel.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if let Some(port) = flag(&args, "--http-port").and_then(|v| v.parse::<u16>().ok()) {
        let token = flag(&args, "--token").unwrap_or_else(|| {
            let generated = uuid::Uuid::new_v4().to_string();
            eprintln!("agenticart-mcp-server: no --token given; generated one for this session:");
            eprintln!("  {generated}");
            eprintln!("Send it as `Authorization: Bearer {generated}` on every request.");
            generated
        });
        // Optional second, less-privileged token (scoped read-only access
        // vs. read-write) - pass --read-only-token to hand a viewer
        // inspection access (list documents/layers, render, sample pixels,
        // check history/job status) without any ability to mutate a
        // document.
        let read_only_token = flag(&args, "--read-only-token");
        // Per-document tokens (the other half of that scoping): repeat
        // --document-token TOKEN:DOCUMENT_ID for each one to share a
        // single document with a collaborator or agent - that token can
        // fully edit that one document and literally cannot touch any
        // other (including document.create/list, which have no
        // single-document meaning).
        let document_tokens = repeated_flag(&args, "--document-token")
            .into_iter()
            .filter_map(|v| v.split_once(':').map(|(t, d)| (t.to_string(), d.to_string())))
            .collect();
        let store = DocumentStore::new();
        return http::serve(store, port, Arc::new(RwLock::new(token)), read_only_token, document_tokens);
    }

    run_stdio()
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn repeated_flag(args: &[String], name: &str) -> Vec<String> {
    args.iter().zip(args.iter().skip(1)).filter(|(a, _)| *a == name).map(|(_, v)| v.clone()).collect()
}

fn run_stdio() -> anyhow::Result<()> {
    let store = DocumentStore::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("failed to parse request: {e}");
                continue;
            }
        };

        // Notifications (no id) get no response, per JSON-RPC.
        let Some(id) = request.id.clone() else {
            continue;
        };

        let response = dispatch::handle(&store, &request, id);
        let out = serde_json::to_string(&response)?;
        stdout.write_all(out.as_bytes())?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }

    Ok(())
}

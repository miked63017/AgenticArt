import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface McpInfo {
  port: number;
  token: string;
  url: string;
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  const doCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard API can be unavailable in some webview contexts - fail quietly.
    }
  }, [text]);
  return (
    <button onClick={doCopy} className="copy-btn">
      {copied ? "Copied!" : "Copy"}
    </button>
  );
}

export default function Settings({ onClose }: { onClose: () => void }) {
  const [info, setInfo] = useState<McpInfo | null>(null);
  const [showToken, setShowToken] = useState(false);
  const [regenerating, setRegenerating] = useState(false);

  const refresh = useCallback(async () => {
    const i = await invoke<McpInfo>("get_mcp_info");
    setInfo(i);
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  async function regenerate() {
    if (!confirm("Regenerate the MCP token? Any agent using the current token will stop working until you give it the new one.")) {
      return;
    }
    setRegenerating(true);
    try {
      const i = await invoke<McpInfo>("regenerate_mcp_token");
      setInfo(i);
      setShowToken(true);
    } finally {
      setRegenerating(false);
    }
  }

  const maskedToken = info ? (showToken ? info.token : "•".repeat(info.token.length)) : "";

  return (
    <div className="settings-overlay" onClick={onClose}>
      <div className="settings-panel" onClick={(e) => e.stopPropagation()}>
        <div className="settings-header">
          <h2>Settings</h2>
          <button onClick={onClose}>Close</button>
        </div>

        <section className="settings-section">
          <h3>MCP Server (for AI agents)</h3>
          <p className="settings-hint">
            This app runs an MCP server in-process so an AI agent can create and edit documents live, in this same window. Point any MCP-capable
            client at the connection below.
          </p>

          {info && (
            <>
              <div className="settings-field">
                <label>Server URL</label>
                <div className="settings-row">
                  <input readOnly value={info.url} />
                  <CopyButton text={info.url} />
                </div>
              </div>

              <div className="settings-field">
                <label>Bearer Token</label>
                <div className="settings-row">
                  <input readOnly value={maskedToken} />
                  <button onClick={() => setShowToken((s) => !s)}>{showToken ? "Hide" : "Show"}</button>
                  <CopyButton text={info.token} />
                </div>
              </div>

              <button onClick={regenerate} disabled={regenerating} className="regenerate-btn">
                {regenerating ? "Regenerating..." : "Regenerate Token"}
              </button>
              <p className="settings-hint">
                The token is stable across restarts (stored on disk), so you only need to configure your agent once. Regenerating it takes effect
                immediately, with no app restart needed - but any agent still using the old token will be rejected on its next call.
              </p>

              <div className="settings-field">
                <label>Codex CLI config (~/.codex/config.toml)</label>
                <pre className="settings-code">
                  {`[mcp_servers.agenticart]\nurl = "${info.url}"\nbearer_token = "${info.token}"`}
                </pre>
                <CopyButton text={`[mcp_servers.agenticart]\nurl = "${info.url}"\nbearer_token = "${info.token}"`} />
              </div>

              <div className="settings-field">
                <label>curl smoke test</label>
                <pre className="settings-code">
                  {`curl -X POST ${info.url} \\\n  -H "Authorization: Bearer ${info.token}" \\\n  -H "Content-Type: application/json" \\\n  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'`}
                </pre>
              </div>
            </>
          )}
        </section>
      </div>
    </div>
  );
}

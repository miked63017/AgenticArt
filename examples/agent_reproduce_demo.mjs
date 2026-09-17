// Demonstrates the core promise: an agent reproducing a reference image
// using only manual tools (paint.strokePath) driven purely via MCP calls -
// sample pixels, paint dots, render, repeat - not a single generative call.
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const exeName = process.platform === "win32" ? "agenticart-mcp-server.exe" : "agenticart-mcp-server";
const exe = path.join(repoRoot, "target", "debug", exeName);
const tmp = (name) => path.join(os.tmpdir(), name);

const child = spawn(exe, [], { stdio: ["pipe", "pipe", "pipe"] });
child.stderr.on("data", (d) => process.stderr.write(`[server stderr] ${d}`));

const rl = createInterface({ input: child.stdout });
const pending = new Map();
let nextId = 1;
rl.on("line", (line) => {
  if (!line.trim()) return;
  const msg = JSON.parse(line);
  const resolve = pending.get(msg.id);
  if (resolve) { pending.delete(msg.id); resolve(msg); }
});

function call(method, params) {
  const id = nextId++;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
  });
}

function toolCall(name, args) {
  return call("tools/call", { name, arguments: args }).then((resp) => {
    const text = resp.result.content[0].text;
    let data;
    try { data = JSON.parse(text); } catch { data = text; }
    if (resp.result.isError) throw new Error(`${name} failed: ${text}`);
    return data;
  });
}

const W = 200, H = 200;

async function main() {
  await call("initialize", {});

  // --- Step 1: build a "reference image" the agent has never seen the
  // construction of - a simple scene (sky gradient stand-in via bands,
  // a sun, a hill) - painted with ordinary tools, just like a human would
  // hand an agent an arbitrary photo.
  console.log("Building reference scene...");
  const { documentId: refDoc } = await toolCall("document.create", { name: "Reference", width: W, height: H });
  const refLayers = await toolCall("layer.list", { documentId: refDoc });
  const refLayerId = refLayers[0].id;

  await toolCall("paint.strokePath", { documentId: refDoc, layerId: refLayerId, points: [{ x: 100, y: 60 }], brush: { size: 240, color: [135, 206, 235, 255], hardness: 1 } }); // sky
  await toolCall("paint.strokePath", { documentId: refDoc, layerId: refLayerId, points: [{ x: 160, y: 30 }], brush: { size: 50, color: [255, 221, 51, 255], hardness: 1 } }); // sun
  await toolCall("paint.strokePath", { documentId: refDoc, layerId: refLayerId, points: [{ x: 0, y: 160 }, { x: 200, y: 160 }], brush: { size: 90, color: [76, 153, 76, 255], hardness: 1 } }); // hill
  await toolCall("paint.strokePath", { documentId: refDoc, layerId: refLayerId, points: [{ x: 60, y: 150 }, { x: 60, y: 110 }], brush: { size: 10, color: [102, 51, 0, 255], hardness: 1 } }); // tree trunk
  await toolCall("paint.strokePath", { documentId: refDoc, layerId: refLayerId, points: [{ x: 60, y: 100 }], brush: { size: 60, color: [34, 102, 34, 255], hardness: 1 } }); // tree canopy

  const refPath = tmp("agenticart_agent_reference.png");
  await toolCall("export.toFile", { documentId: refDoc, path: refPath });
  console.log(`Reference exported to ${refPath}`);

  // --- Step 2: the "agent" side. It only knows there is a reference image
  // file on disk - it does NOT know how it was constructed. It opens it,
  // reads raw pixels via canvas.getPixels (its only way to "see"), and
  // reproduces it into a brand new document using nothing but
  // paint.strokePath dots - the same primitive a human brush tool uses.
  console.log("\nAgent: opening reference file to inspect it...");
  const opened = await toolCall("document.openFile", { path: refPath });
  const openedLayers = await toolCall("layer.list", { documentId: opened.documentId });
  const refPixelsResp = await toolCall("canvas.getPixels", { documentId: opened.documentId, layerId: openedLayers[0].id });
  const refBytes = Buffer.from(refPixelsResp.rgba8Base64, "base64");

  console.log("Agent: creating a blank document and reproducing the reference stroke-by-stroke...");
  const { documentId: reproDoc } = await toolCall("document.create", { name: "Agent Reproduction", width: W, height: H });
  const reproLayers = await toolCall("layer.list", { documentId: reproDoc });
  const reproLayerId = reproLayers[0].id;

  const STEP = 5; // grid sampling stride - smaller = higher fidelity, more calls
  let dotsPainted = 0;
  for (let y = 0; y < H; y += STEP) {
    for (let x = 0; x < W; x += STEP) {
      const idx = (y * W + x) * 4;
      const [r, g, b, a] = [refBytes[idx], refBytes[idx + 1], refBytes[idx + 2], refBytes[idx + 3]];
      if (a === 0) continue;
      await toolCall("paint.strokePath", {
        documentId: reproDoc,
        layerId: reproLayerId,
        points: [{ x, y }],
        brush: { size: STEP + 2, color: [r, g, b, a], hardness: 1 },
      });
      dotsPainted++;
    }
  }
  console.log(`Agent: painted ${dotsPainted} dots to reproduce the reference.`);

  const reproPath = tmp("agenticart_agent_reproduction.png");
  await toolCall("export.toFile", { documentId: reproDoc, path: reproPath });
  console.log(`Reproduction exported to ${reproPath}`);

  // --- Step 3: quantify fidelity (mean absolute pixel difference) as
  // evidence, not just an eyeballed screenshot.
  const reproPixelsResp = await toolCall("canvas.getPixels", { documentId: reproDoc, layerId: reproLayerId });
  const reproBytes = Buffer.from(reproPixelsResp.rgba8Base64, "base64");
  let totalDiff = 0;
  for (let i = 0; i < refBytes.length; i++) totalDiff += Math.abs(refBytes[i] - reproBytes[i]);
  const meanAbsDiff = totalDiff / refBytes.length;
  console.log(`\nMean absolute per-channel pixel difference: ${meanAbsDiff.toFixed(2)} / 255 (${((1 - meanAbsDiff / 255) * 100).toFixed(1)}% fidelity)`);

  child.stdin.end();
  child.kill();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

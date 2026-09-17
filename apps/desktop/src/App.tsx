import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import Settings from "./Settings";
import CloseTabDialog from "./CloseTabDialog";
import "./App.css";

interface LayerInfo {
  id: string;
  name: string;
  opacity: number;
  visible: boolean;
  blend_mode: string;
  clip_to_below: boolean;
  has_drop_shadow: boolean;
  has_stroke: boolean;
  has_outer_glow: boolean;
  smart_filter_count: number;
  has_mask: boolean;
  has_text: boolean;
  is_group: boolean;
  parent_group: string | null;
}

interface DocumentInfo {
  id: string;
  name: string;
  width: number;
  height: number;
  layers: LayerInfo[];
}

interface DocumentSummary {
  id: string;
  name: string;
}

type Tool = "brush" | "eraser" | "select" | "text" | "rect" | "ellipse" | "pen" | "gradient" | "paintBucket" | "cloneStamp" | "dodge" | "burn";

const CANVAS_WIDTH = 720;
const CANVAS_HEIGHT = 520;
const MAX_DISPLAY_WIDTH = 720;
const MAX_DISPLAY_HEIGHT = 520;

function fitDisplaySize(width: number, height: number) {
  const scale = Math.min(MAX_DISPLAY_WIDTH / width, MAX_DISPLAY_HEIGHT / height, 1);
  return { width: Math.round(width * scale), height: Math.round(height * scale) };
}
const BLEND_MODES = [
  "normal", "multiply", "screen", "overlay", "darken", "lighten", "colorDodge", "colorBurn", "hardLight", "softLight",
  "difference", "exclusion", "linearBurn", "darkerColor", "linearDodge", "lighterColor", "vividLight", "linearLight",
  "pinLight", "hardMix", "subtract", "divide", "hue", "saturation", "color", "luminosity",
];

function App() {
  const [doc, setDoc] = useState<DocumentInfo | null>(null);
  const [renderUrl, setRenderUrl] = useState<string | null>(null);
  const [brushSize, setBrushSize] = useState(18);
  const [brushColor, setBrushColor] = useState("#e0303c");
  const [tool, setTool] = useState<Tool>("brush");
  const [activeLayerId, setActiveLayerId] = useState<string | null>(null);
  const [selectionRect, setSelectionRect] = useState<{ x: number; y: number; w: number; h: number } | null>(null);
  const [brightness, setBrightness] = useState(0);
  const [contrast, setContrast] = useState(0);
  const [hue, setHue] = useState(0);
  const [saturation, setSaturation] = useState(0);
  const [blurRadius, setBlurRadius] = useState(4);
  const [sharpenRadius, setSharpenRadius] = useState(1.5);
  const [textValue, setTextValue] = useState("Hello");
  const [textSize, setTextSize] = useState(48);
  const [resizeW, setResizeW] = useState(CANVAS_WIDTH);
  const [resizeH, setResizeH] = useState(CANVAS_HEIGHT);
  const [groupTarget, setGroupTarget] = useState<string>("");
  const [penPoints, setPenPoints] = useState<{ x: number; y: number }[]>([]);
  const [fromProfile, setFromProfile] = useState("sRGB");
  const [toProfile, setToProfile] = useState("Adobe RGB");
  const PROFILES = ["sRGB", "Adobe RGB", "Display P3", "ProPhoto RGB"];
  const [status, setStatus] = useState("");
  const [gradientColor2, setGradientColor2] = useState("#ffffff");
  const [gradientKind, setGradientKind] = useState<"linear" | "radial">("linear");
  const [fillTolerance, setFillTolerance] = useState(32);
  const [toneStrength, setToneStrength] = useState(0.15);
  const [cloneSource, setCloneSource] = useState<{ x: number; y: number } | null>(null);
  const [smartFilterType, setSmartFilterType] = useState("gaussianBlur");
  const [layerComps, setLayerComps] = useState<{ id: string; name: string }[]>([]);
  const [compName, setCompName] = useState("Comp 1");
  const [openDocs, setOpenDocs] = useState<DocumentSummary[]>([]);
  const [showSettings, setShowSettings] = useState(false);
  // Documents this session knows have a real on-disk project path (set
  // by Save Project / Open Project / Open PSD / Export PSD). A document
  // never in this map hasn't been saved to disk, so closing its tab
  // should offer to save first rather than silently discarding it - a
  // document that HAS been saved to disk before can be closed without
  // that prompt (this app doesn't track dirty-since-last-save state, so
  // "has a save path" is the closest cheap proxy for "already saved").
  const [savedPaths, setSavedPaths] = useState<Record<string, string>>({});
  const [fillMethod, setFillMethod] = useState<"classical" | "ml">("ml");
  const [fillIterations, setFillIterations] = useState(300);

  const drawing = useRef(false);
  const lastPoint = useRef<{ x: number; y: number } | null>(null);
  const selectStart = useRef<{ x: number; y: number } | null>(null);
  const canvasRef = useRef<HTMLDivElement>(null);
  const cloneAnchor = useRef<{ x: number; y: number } | null>(null);
  const cloneSourceStart = useRef<{ x: number; y: number } | null>(null);
  const gradientStart = useRef<{ x: number; y: number } | null>(null);
  const [gradientEnd, setGradientEnd] = useState<{ x: number; y: number } | null>(null);

  const refreshCanvas = useCallback(async (documentId: string) => {
    const png: string = await invoke("render_canvas", { documentId });
    setRenderUrl(`data:image/png;base64,${png}`);
  }, []);

  const refreshLayerComps = useCallback(async (documentId: string) => {
    const comps = await invoke<{ id: string; name: string }[]>("list_layer_comps", { documentId });
    setLayerComps(comps);
  }, []);

  const refreshDoc = useCallback(
    async (documentId: string) => {
      const info = await invoke<DocumentInfo>("get_document", { documentId });
      setDoc(info);
      await refreshCanvas(documentId);
      await refreshLayerComps(documentId);
      return info;
    },
    [refreshCanvas, refreshLayerComps]
  );

  const newDocument = useCallback(async () => {
    const info = await invoke<DocumentInfo>("create_document", {
      name: "Untitled",
      width: CANVAS_WIDTH,
      height: CANVAS_HEIGHT,
    });
    setDoc(info);
    setActiveLayerId(info.layers[info.layers.length - 1].id);
    setSelectionRect(null);
    await refreshCanvas(info.id);
    setStatus(`Created "${info.name}" (${info.width}x${info.height})`);
  }, [refreshCanvas]);

  useEffect(() => {
    newDocument();
  }, [newDocument]);

  const switchDocument = useCallback(
    async (documentId: string) => {
      const info = await refreshDoc(documentId);
      setActiveLayerId(info.layers.length > 0 ? info.layers[info.layers.length - 1].id : null);
      setSelectionRect(null);
      setStatus(`Switched to "${info.name}"`);
    },
    [refreshDoc]
  );

  const saveProjectAs = useCallback(
    async (documentId: string, docName: string) => {
      const path = await save({ defaultPath: `${docName}.agenticart`, filters: [{ name: "AgenticArt Project", extensions: ["agenticart"] }] });
      if (!path) return null;
      await invoke("save_project", { documentId, path });
      setSavedPaths((prev) => ({ ...prev, [documentId]: path }));
      return path;
    },
    []
  );

  const finishCloseTab = useCallback(
    async (documentId: string) => {
      await invoke("close_document", { documentId });
      const remaining = await invoke<DocumentSummary[]>("list_documents");
      setOpenDocs(remaining);
      setSavedPaths((prev) => {
        const next = { ...prev };
        delete next[documentId];
        return next;
      });

      if (docRef.current?.id === documentId) {
        if (remaining.length > 0) {
          await switchDocument(remaining[0].id);
        } else {
          await newDocument();
        }
      }
    },
    [switchDocument, newDocument]
  );

  // A document with no known save path shows the Save/Don't Save/Cancel
  // dialog (see CloseTabDialog) instead of closing immediately - one
  // decision, not a forced march through the close. A document that's
  // already been saved to disk before just closes right away.
  const [closeTabTarget, setCloseTabTarget] = useState<{ id: string; name: string } | null>(null);

  const closeTab = useCallback(
    (documentId: string, docName: string) => {
      if (!savedPaths[documentId]) {
        setCloseTabTarget({ id: documentId, name: docName });
        return;
      }
      finishCloseTab(documentId);
    },
    [savedPaths, finishCloseTab]
  );

  // Keeps the latest `doc` reachable from the polling interval below
  // without re-registering that interval on every edit.
  const docRef = useRef<DocumentInfo | null>(null);
  useEffect(() => {
    docRef.current = doc;
  }, [doc]);

  // Both the tab list and the currently-displayed canvas can change from
  // outside this window - an MCP agent calling document.create or
  // paint.strokePath (etc.) against the same embedded store this UI reads
  // from. There's no push channel from that side, so a light poll is how
  // a new tab, or a stroke an agent just painted into the open document,
  // shows up here without the human needing to trigger some other action
  // first.
  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try {
        const docs = await invoke<DocumentSummary[]>("list_documents");
        if (!cancelled) setOpenDocs(docs);
        if (docRef.current) await refreshCanvas(docRef.current.id);

        // An MCP agent calling document.focus wants a specific tab/window
        // brought to the front. Drain that request here (same poll cadence
        // as everything else) rather than adding a second interval.
        const focusId = await invoke<string | null>("take_focus_request");
        if (focusId && !cancelled) {
          if (focusId !== docRef.current?.id) {
            await switchDocument(focusId);
          }
          await getCurrentWindow().unminimize();
          await getCurrentWindow().setFocus();
        }
      } catch {
        // Ignore transient errors (e.g. during shutdown).
      }
    };
    poll();
    const interval = setInterval(poll, 1500);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [refreshCanvas, switchDocument]);

  // The real "live" path: the Rust side emits this the instant any
  // mutation lands on any document (see DocumentStore::on_change), whether
  // it came from an MCP tool call or this window's own UI - no agent needs
  // to call canvas.render, and no waiting on the poll interval above.
  // Only touches the color swatch (not the active tool - see toolbar
  // comment) and only while the human isn't mid-gesture themselves, so an
  // agent's edit can never yank the swatch out from under a human's own
  // in-progress stroke.
  // Coalesces rapid refresh requests (a fast brush stroke can fire a
  // "document-changed" event faster than render_canvas - a full recomposite
  // + PNG encode of the whole canvas - can complete). Never queues more
  // than one pending request: if several events land while a render is
  // in flight, only the latest is rendered once that one finishes, so the
  // UI never falls further and further behind under fast input.
  const renderInFlight = useRef(false);
  const queuedRenderDocId = useRef<string | null>(null);
  const requestCanvasRefresh = useCallback(
    async (documentId: string) => {
      if (renderInFlight.current) {
        queuedRenderDocId.current = documentId;
        return;
      }
      renderInFlight.current = true;
      try {
        await refreshCanvas(documentId);
        while (queuedRenderDocId.current) {
          const next = queuedRenderDocId.current;
          queuedRenderDocId.current = null;
          await refreshCanvas(next);
        }
      } finally {
        renderInFlight.current = false;
      }
    },
    [refreshCanvas]
  );

  useEffect(() => {
    const unlistenPromise = listen<string>("document-changed", async (event) => {
      const changedId = event.payload;
      if (changedId !== docRef.current?.id) return;
      await requestCanvasRefresh(changedId);
      if (!drawing.current) {
        const color = await invoke<[number, number, number, number] | null>("get_last_color", { documentId: changedId });
        if (color) setBrushColor(rgbaToHex(color));
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [requestCanvasRefresh]);

  useEffect(() => {
    if (doc) {
      setResizeW(doc.width);
      setResizeH(doc.height);
    }
  }, [doc?.id, doc?.width, doc?.height]);

  // Standard Ctrl/Cmd shortcuts. Only combinations with a modifier key are
  // handled, so ordinary typing in a text field (the Text tool's input,
  // Settings, layer renames, etc.) is never intercepted.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      const mod = e.ctrlKey || e.metaKey;
      if (!mod) return;
      const key = e.key.toLowerCase();
      if (key === "z" && e.shiftKey) {
        e.preventDefault();
        doRedo();
      } else if (key === "z") {
        e.preventDefault();
        doUndo();
      } else if (key === "y") {
        e.preventDefault();
        doRedo();
      } else if (key === "s") {
        e.preventDefault();
        saveProject();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [doUndo, doRedo, saveProject]);

  async function resizeImage() {
    if (!doc) return;
    const info = await invoke<DocumentInfo>("resize_document", { documentId: doc.id, width: resizeW, height: resizeH });
    setDoc(info);
    setSelectionRect(null);
    await refreshCanvas(info.id);
    setStatus(`Resized image to ${resizeW}x${resizeH}`);
  }

  async function resizeCanvasFn() {
    if (!doc) return;
    const info = await invoke<DocumentInfo>("resize_canvas", { documentId: doc.id, width: resizeW, height: resizeH, offsetX: 0, offsetY: 0 });
    setDoc(info);
    setSelectionRect(null);
    await refreshCanvas(info.id);
    setStatus(`Resized canvas to ${resizeW}x${resizeH}`);
  }

  function rgbaToHex([r, g, b]: [number, number, number, number]): string {
    const toHex = (n: number) => n.toString(16).padStart(2, "0");
    return `#${toHex(r)}${toHex(g)}${toHex(b)}`;
  }

  function hexToRgba(hex: string): [number, number, number, number] {
    const r = parseInt(hex.slice(1, 3), 16);
    const g = parseInt(hex.slice(3, 5), 16);
    const b = parseInt(hex.slice(5, 7), 16);
    return [r, g, b, 255];
  }

  function pointFromEvent(e: React.PointerEvent<HTMLDivElement>) {
    const rect = canvasRef.current!.getBoundingClientRect();
    const scaleX = (doc?.width ?? CANVAS_WIDTH) / rect.width;
    const scaleY = (doc?.height ?? CANVAS_HEIGHT) / rect.height;
    return {
      x: (e.clientX - rect.left) * scaleX,
      y: (e.clientY - rect.top) * scaleY,
    };
  }

  // `from === null` marks the first segment of a gesture (pointer-down);
  // every following segment of the same drag passes `continueStroke: true`
  // so the whole gesture undoes as one step and skips the expensive
  // per-segment undo-snapshot clone (see DocumentStore::mutate_continue).
  // The canvas itself updates via the "document-changed" push event (see
  // that effect above), not an explicit refresh here - doing both would
  // render the same frame twice on every single mouse-move.
  async function strokeSegment(from: { x: number; y: number } | null, to: { x: number; y: number }) {
    if (!doc || !activeLayerId) return;
    const points = from ? [from, to] : [to];
    const command = tool === "eraser" ? "erase_stroke" : "paint_stroke";
    await invoke(command, {
      documentId: doc.id,
      layerId: activeLayerId,
      points: points.map((p) => ({ x: p.x, y: p.y, pressure: 1.0 })),
      brush: { size: brushSize, color: hexToRgba(brushColor), hardness: 1.0 },
      continueStroke: from !== null,
    });
  }

  async function toneSegment(to: { x: number; y: number }, continueStroke: boolean) {
    if (!doc || !activeLayerId) return;
    await invoke("dodge_burn", {
      documentId: doc.id,
      layerId: activeLayerId,
      mode: tool === "burn" ? "burn" : "dodge",
      strength: toneStrength,
      points: [{ x: to.x, y: to.y, pressure: 1.0 }],
      brush: { size: brushSize, color: hexToRgba(brushColor), hardness: 1.0 },
      continueStroke,
    });
  }

  async function cloneSegment(to: { x: number; y: number }, continueStroke: boolean) {
    if (!doc || !activeLayerId || !cloneAnchor.current || !cloneSourceStart.current) return;
    // Keep the source-to-destination offset fixed for the whole stroke:
    // the source point for `to` is the original clone source shifted by
    // how far `to` has moved from the stroke's own starting point.
    const sourceX = cloneSourceStart.current.x + (to.x - cloneAnchor.current.x);
    const sourceY = cloneSourceStart.current.y + (to.y - cloneAnchor.current.y);
    await invoke("clone_stamp", {
      documentId: doc.id,
      layerId: activeLayerId,
      sourceX,
      sourceY,
      destPoints: [{ x: to.x, y: to.y, pressure: 1.0 }],
      brush: { size: brushSize, color: hexToRgba(brushColor), hardness: 1.0 },
      continueStroke,
    });
  }

  async function placeText(p: { x: number; y: number }) {
    if (!doc || !activeLayerId || !textValue) return;
    await invoke("draw_text", {
      documentId: doc.id,
      layerId: activeLayerId,
      text: textValue,
      x: Math.round(p.x),
      y: Math.round(p.y - textSize),
      size: textSize,
      color: hexToRgba(brushColor),
    });
    await refreshCanvas(doc.id);
    setStatus(`Placed text "${textValue}"`);
  }

  const isDragRectTool = (t: Tool) => t === "select" || t === "rect" || t === "ellipse";

  async function finishPen(action: "fill" | "stroke") {
    if (!doc || !activeLayerId || penPoints.length < 2) {
      setPenPoints([]);
      return;
    }
    const color = hexToRgba(brushColor);
    if (action === "fill") {
      await invoke("fill_path", { documentId: doc.id, layerId: activeLayerId, nodes: penPoints, color });
    } else {
      await invoke("stroke_path_shape", { documentId: doc.id, layerId: activeLayerId, nodes: penPoints, brushSize, color });
    }
    setPenPoints([]);
    await refreshCanvas(doc.id);
    setStatus(`Path ${action === "fill" ? "filled" : "stroked"} (${penPoints.length} anchors)`);
  }

  function onPointerDown(e: React.PointerEvent<HTMLDivElement>) {
    if (!doc) return;
    const p = pointFromEvent(e);
    if (tool === "pen") {
      setPenPoints((pts) => [...pts, p]);
      return;
    }
    if (isDragRectTool(tool)) {
      selectStart.current = p;
      setSelectionRect({ x: p.x, y: p.y, w: 0, h: 0 });
      return;
    }
    if (tool === "gradient") {
      gradientStart.current = p;
      setGradientEnd(p);
      return;
    }
    if (tool === "text") {
      placeText(p);
      return;
    }
    if (tool === "paintBucket") {
      if (!activeLayerId) return;
      invoke("paint_bucket", {
        documentId: doc.id,
        layerId: activeLayerId,
        x: Math.round(p.x),
        y: Math.round(p.y),
        color: hexToRgba(brushColor),
        tolerance: fillTolerance,
      }).then(() => refreshCanvas(doc.id));
      return;
    }
    if (tool === "cloneStamp") {
      if (e.shiftKey || !cloneSource) {
        setCloneSource(p);
        setStatus(`Clone source set at (${Math.round(p.x)}, ${Math.round(p.y)}) - now drag to paint`);
        return;
      }
      cloneAnchor.current = p;
      cloneSourceStart.current = cloneSource;
      drawing.current = true;
      cloneSegment(p, false);
      return;
    }
    if (tool === "dodge" || tool === "burn") {
      drawing.current = true;
      toneSegment(p, false);
      return;
    }
    drawing.current = true;
    lastPoint.current = p;
    strokeSegment(null, p);
  }

  function onPointerMove(e: React.PointerEvent<HTMLDivElement>) {
    if (!doc) return;
    const p = pointFromEvent(e);
    if (isDragRectTool(tool) && selectStart.current) {
      const start = selectStart.current;
      setSelectionRect({
        x: Math.min(start.x, p.x),
        y: Math.min(start.y, p.y),
        w: Math.abs(p.x - start.x),
        h: Math.abs(p.y - start.y),
      });
      return;
    }
    if (tool === "gradient" && gradientStart.current) {
      setGradientEnd(p);
      return;
    }
    if (!drawing.current) return;
    if (tool === "cloneStamp") {
      cloneSegment(p, true);
      return;
    }
    if (tool === "dodge" || tool === "burn") {
      toneSegment(p, true);
      return;
    }
    strokeSegment(lastPoint.current, p);
    lastPoint.current = p;
  }

  async function onPointerUp() {
    drawing.current = false;
    lastPoint.current = null;
    if (tool === "gradient" && gradientStart.current && doc && activeLayerId && gradientEnd) {
      const start = gradientStart.current;
      gradientStart.current = null;
      const dist = Math.hypot(gradientEnd.x - start.x, gradientEnd.y - start.y);
      if (dist > 1) {
        await invoke("fill_gradient", {
          documentId: doc.id,
          layerId: activeLayerId,
          kind: gradientKind,
          startX: start.x,
          startY: start.y,
          endX: gradientEnd.x,
          endY: gradientEnd.y,
          stops: [
            { position: 0, color: hexToRgba(brushColor) },
            { position: 1, color: hexToRgba(gradientColor2) },
          ],
        });
        await refreshCanvas(doc.id);
        setStatus(`Filled ${gradientKind} gradient`);
      }
      setGradientEnd(null);
      return;
    }
    if (tool === "cloneStamp") {
      cloneAnchor.current = null;
      cloneSourceStart.current = null;
    }
    if ((tool === "rect" || tool === "ellipse") && selectStart.current && doc && activeLayerId && selectionRect) {
      selectStart.current = null;
      if (selectionRect.w > 1 && selectionRect.h > 1) {
        const color = hexToRgba(brushColor);
        if (tool === "rect") {
          await invoke("fill_rect", {
            documentId: doc.id,
            layerId: activeLayerId,
            x: Math.round(selectionRect.x),
            y: Math.round(selectionRect.y),
            width: Math.round(selectionRect.w),
            height: Math.round(selectionRect.h),
            color,
          });
        } else {
          await invoke("fill_ellipse", {
            documentId: doc.id,
            layerId: activeLayerId,
            cx: selectionRect.x + selectionRect.w / 2,
            cy: selectionRect.y + selectionRect.h / 2,
            rx: selectionRect.w / 2,
            ry: selectionRect.h / 2,
            color,
          });
        }
        await refreshCanvas(doc.id);
        setStatus(`Filled ${tool}`);
      }
      setSelectionRect(null);
      return;
    }
    if (tool === "select" && selectStart.current && doc && selectionRect) {
      selectStart.current = null;
      if (selectionRect.w > 1 && selectionRect.h > 1) {
        await invoke("set_selection_rect", {
          documentId: doc.id,
          x: Math.round(selectionRect.x),
          y: Math.round(selectionRect.y),
          width: Math.round(selectionRect.w),
          height: Math.round(selectionRect.h),
        });
        setStatus(`Selection set: ${Math.round(selectionRect.w)}x${Math.round(selectionRect.h)}`);
      }
    }
  }

  async function clearSelection() {
    if (!doc) return;
    await invoke("clear_selection", { documentId: doc.id });
    setSelectionRect(null);
    setStatus("Selection cleared");
  }

  async function doUndo() {
    if (!doc) return;
    const undone = await invoke<boolean>("undo", { documentId: doc.id });
    if (undone) await refreshDoc(doc.id);
    setStatus(undone ? "Undone" : "Nothing to undo");
  }

  async function doRedo() {
    if (!doc) return;
    const redone = await invoke<boolean>("redo", { documentId: doc.id });
    if (redone) await refreshDoc(doc.id);
    setStatus(redone ? "Redone" : "Nothing to redo");
  }

  async function doExport() {
    if (!doc) return;
    const path = await save({ defaultPath: `${doc.name}.png`, filters: [{ name: "PNG", extensions: ["png"] }] });
    if (!path) return;
    await invoke("export_document", { documentId: doc.id, path });
    setStatus(`Exported to ${path}`);
  }

  async function addLayer() {
    if (!doc) return;
    const layerId = await invoke<string>("create_layer", { documentId: doc.id, name: `Layer ${doc.layers.length + 1}` });
    setActiveLayerId(layerId);
    await refreshDoc(doc.id);
  }

  async function addGroup() {
    if (!doc) return;
    const groupId = await invoke<string>("create_group", { documentId: doc.id, name: `Group ${doc.layers.filter((l) => l.is_group).length + 1}` });
    setActiveLayerId(groupId);
    setGroupTarget(groupId);
    await refreshDoc(doc.id);
  }

  async function moveIntoGroup(layerId: string) {
    if (!doc || !groupTarget) return;
    await invoke("move_into_group", { documentId: doc.id, layerId, groupId: groupTarget });
    await refreshDoc(doc.id);
  }

  async function ungroupLayer(layerId: string) {
    if (!doc) return;
    await invoke("ungroup_layer", { documentId: doc.id, layerId });
    await refreshDoc(doc.id);
  }

  async function duplicateLayer(layerId: string) {
    if (!doc) return;
    const newId = await invoke<string>("duplicate_layer", { documentId: doc.id, layerId });
    setActiveLayerId(newId);
    await refreshDoc(doc.id);
  }

  async function deleteLayer(layerId: string) {
    if (!doc) return;
    await invoke("delete_layer", { documentId: doc.id, layerId });
    await refreshDoc(doc.id);
  }

  async function mergeLayerDown(layerId: string) {
    if (!doc) return;
    const survivorId = await invoke<string | null>("merge_down", { documentId: doc.id, layerId });
    if (survivorId) {
      setActiveLayerId(survivorId);
    }
    await refreshDoc(doc.id);
  }

  async function flattenDocument() {
    if (!doc) return;
    await invoke("flatten_image", { documentId: doc.id });
    await refreshDoc(doc.id);
  }

  async function moveLayer(layerId: string, direction: 1 | -1) {
    if (!doc) return;
    const idx = doc.layers.findIndex((l) => l.id === layerId);
    const newIndex = idx + direction;
    if (newIndex < 0 || newIndex >= doc.layers.length) return;
    await invoke("reorder_layer", { documentId: doc.id, layerId, newIndex });
    await refreshDoc(doc.id);
  }

  async function toggleVisible(layer: LayerInfo) {
    if (!doc) return;
    await invoke("set_layer_properties", { documentId: doc.id, layerId: layer.id, visible: !layer.visible });
    await refreshDoc(doc.id);
  }

  async function setOpacity(layer: LayerInfo, opacity: number) {
    if (!doc) return;
    await invoke("set_layer_properties", { documentId: doc.id, layerId: layer.id, opacity });
    await refreshDoc(doc.id);
  }

  async function setBlendMode(layer: LayerInfo, blendMode: string) {
    if (!doc) return;
    await invoke("set_layer_properties", { documentId: doc.id, layerId: layer.id, blendMode });
    await refreshDoc(doc.id);
  }

  async function toggleClipToBelow(layer: LayerInfo) {
    if (!doc) return;
    await invoke("set_layer_properties", { documentId: doc.id, layerId: layer.id, clipToBelow: !layer.clip_to_below });
    await refreshDoc(doc.id);
  }

  async function toggleDropShadow(layer: LayerInfo) {
    if (!doc) return;
    await invoke("set_drop_shadow", {
      documentId: doc.id,
      layerId: layer.id,
      enabled: !layer.has_drop_shadow,
      color: [0, 0, 0, 255],
      offsetX: 6,
      offsetY: 6,
      blurRadius: 6,
      opacity: 0.6,
    });
    await refreshDoc(doc.id);
  }

  async function toggleStroke(layer: LayerInfo) {
    if (!doc) return;
    await invoke("set_stroke", {
      documentId: doc.id,
      layerId: layer.id,
      enabled: !layer.has_stroke,
      color: hexToRgba(brushColor),
      width: 3,
      opacity: 1.0,
    });
    await refreshDoc(doc.id);
  }

  async function toggleOuterGlow(layer: LayerInfo) {
    if (!doc) return;
    await invoke("set_outer_glow", {
      documentId: doc.id,
      layerId: layer.id,
      enabled: !layer.has_outer_glow,
      color: hexToRgba(brushColor),
      radius: 8,
      opacity: 0.75,
    });
    await refreshDoc(doc.id);
  }

  async function toggleMask(layer: LayerInfo) {
    if (!doc) return;
    if (layer.has_mask) {
      await invoke("clear_mask", { documentId: doc.id, layerId: layer.id });
      setStatus("Mask cleared");
    } else {
      try {
        await invoke("mask_from_selection", { documentId: doc.id, layerId: layer.id });
        setStatus("Mask created from selection");
      } catch (e) {
        setStatus(`Could not create mask: ${e}`);
        return;
      }
    }
    await refreshDoc(doc.id);
  }

  async function addSmartFilterToLayer(layer: LayerInfo) {
    if (!doc) return;
    await invoke("add_smart_filter", {
      documentId: doc.id,
      layerId: layer.id,
      filterType: smartFilterType,
      radius: blurRadius,
      threshold: 0,
      brightness,
      contrast,
      hue,
      saturation,
      lightness: 0,
    });
    setStatus(`Added ${smartFilterType} smart filter`);
    await refreshDoc(doc.id);
  }

  async function createTextLayerFn() {
    if (!doc) return;
    const layerId = await invoke<string>("create_text_layer", {
      documentId: doc.id,
      text: textValue,
      x: 20,
      y: 20,
      size: textSize,
      color: hexToRgba(brushColor),
    });
    setActiveLayerId(layerId);
    await refreshDoc(doc.id);
  }

  async function createComp() {
    if (!doc) return;
    await invoke("create_layer_comp", { documentId: doc.id, name: compName });
    await refreshLayerComps(doc.id);
  }

  async function applyComp(compId: string) {
    if (!doc) return;
    await invoke("apply_layer_comp", { documentId: doc.id, compId });
    await refreshDoc(doc.id);
  }

  async function deleteComp(compId: string) {
    if (!doc) return;
    await invoke("delete_layer_comp", { documentId: doc.id, compId });
    await refreshLayerComps(doc.id);
  }

  async function convertProfile() {
    if (!doc) return;
    await invoke("convert_color_profile", { documentId: doc.id, from: fromProfile, to: toProfile });
    await refreshCanvas(doc.id);
    setStatus(`Converted ${fromProfile} -> ${toProfile}`);
  }

  async function applyCmykSoftProof() {
    if (!doc || !activeLayerId) return;
    await invoke("cmyk_soft_proof", { documentId: doc.id, layerId: activeLayerId });
    await refreshCanvas(doc.id);
    setStatus("Applied CMYK soft proof");
  }

  async function exportHighBitDepth() {
    if (!doc) return;
    const path = await save({ defaultPath: `${doc.name}_16bit.png`, filters: [{ name: "PNG", extensions: ["png"] }] });
    if (!path) return;
    await invoke("export_high_bit_depth", { documentId: doc.id, path });
    setStatus(`Exported 16-bit PNG to ${path}`);
  }

  async function cutoutSubject() {
    if (!doc || !activeLayerId) return;
    setStatus("Running subject segmentation...");
    const newLayerId = await invoke<string>("cutout_subject", { documentId: doc.id, layerId: activeLayerId });
    setActiveLayerId(newLayerId);
    await refreshDoc(doc.id);
    setStatus("Cut out subject into a new layer");
  }

  async function selectSubject() {
    if (!doc || !activeLayerId) return;
    setStatus("Running subject segmentation...");
    const rect = await invoke<{ x: number; y: number; width: number; height: number } | null>("select_subject", {
      documentId: doc.id,
      layerId: activeLayerId,
    });
    if (rect) {
      setSelectionRect({ x: rect.x, y: rect.y, w: rect.width, h: rect.height });
      setStatus(`Selected subject bounding box: ${rect.width}x${rect.height}`);
    } else {
      setSelectionRect(null);
      setStatus("No subject detected");
    }
  }

  async function applyBrightnessContrast() {
    if (!doc || !activeLayerId) return;
    await invoke("apply_brightness_contrast", { documentId: doc.id, layerId: activeLayerId, brightness, contrast });
    await refreshCanvas(doc.id);
    setStatus("Applied brightness/contrast");
  }

  async function applyHueSaturation() {
    if (!doc || !activeLayerId) return;
    await invoke("apply_hue_saturation", { documentId: doc.id, layerId: activeLayerId, hue, saturation, lightness: 0 });
    await refreshCanvas(doc.id);
    setStatus("Applied hue/saturation");
  }

  async function applyBlur() {
    if (!doc || !activeLayerId) return;
    await invoke("apply_gaussian_blur", { documentId: doc.id, layerId: activeLayerId, radius: blurRadius });
    await refreshCanvas(doc.id);
    setStatus(`Applied Gaussian blur (radius ${blurRadius})`);
  }

  async function applySharpenFilter() {
    if (!doc || !activeLayerId) return;
    await invoke("apply_sharpen", { documentId: doc.id, layerId: activeLayerId, radius: sharpenRadius, threshold: 0 });
    await refreshCanvas(doc.id);
    setStatus(`Applied sharpen (radius ${sharpenRadius})`);
  }

  async function saveProject() {
    if (!doc) return;
    const path = await saveProjectAs(doc.id, doc.name);
    if (path) setStatus(`Saved project to ${path}`);
  }

  async function openProject() {
    const path = await open({ multiple: false, filters: [{ name: "AgenticArt Project", extensions: ["agenticart"] }] });
    if (!path || typeof path !== "string") return;
    const info = await invoke<DocumentInfo>("open_project", { path });
    setDoc(info);
    setActiveLayerId(info.layers[info.layers.length - 1].id);
    setSelectionRect(null);
    setSavedPaths((prev) => ({ ...prev, [info.id]: path }));
    await refreshCanvas(info.id);
    setStatus(`Opened project "${path}"`);
  }

  async function openImageAsDocument() {
    const path = await open({ multiple: false, filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff"] }] });
    if (!path || typeof path !== "string") return;
    const info = await invoke<DocumentInfo>("open_document_from_file", { path });
    setDoc(info);
    setActiveLayerId(info.layers[0].id);
    setSelectionRect(null);
    await refreshCanvas(info.id);
    setStatus(`Opened "${path}" (${info.width}x${info.height})`);
  }

  async function exportPsd() {
    if (!doc) return;
    const path = await save({ defaultPath: `${doc.name}.psd`, filters: [{ name: "Photoshop", extensions: ["psd"] }] });
    if (!path) return;
    await invoke("export_psd", { documentId: doc.id, path });
    setSavedPaths((prev) => ({ ...prev, [doc.id]: path }));
    setStatus(`Exported PSD to ${path}`);
  }

  async function openPsd() {
    const path = await open({ multiple: false, filters: [{ name: "Photoshop", extensions: ["psd"] }] });
    if (!path || typeof path !== "string") return;
    const info = await invoke<DocumentInfo>("open_psd", { path });
    setDoc(info);
    setActiveLayerId(info.layers[info.layers.length - 1].id);
    setSelectionRect(null);
    setSavedPaths((prev) => ({ ...prev, [info.id]: path }));
    await refreshCanvas(info.id);
    setStatus(`Opened "${path}" (${info.layers.length} layers)`);
  }

  async function generativeFill() {
    if (!doc || !activeLayerId || !selectionRect) return;
    const args = {
      documentId: doc.id,
      layerId: activeLayerId,
      x: Math.round(selectionRect.x),
      y: Math.round(selectionRect.y),
      width: Math.round(selectionRect.w),
      height: Math.round(selectionRect.h),
    };
    if (fillMethod === "ml") {
      await invoke("ml_inpaint", args);
      setStatus("Filled selection with MI-GAN (ML inpainting)");
    } else {
      await invoke("content_aware_fill", { ...args, iterations: fillIterations });
      setStatus(`Filled selection with content-aware fill (${fillIterations} iterations)`);
    }
    await refreshCanvas(doc.id);
  }

  async function placeImageOnActiveLayer() {
    if (!doc || !activeLayerId) return;
    const path = await open({ multiple: false, filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff"] }] });
    if (!path || typeof path !== "string") return;
    await invoke("place_image", { documentId: doc.id, layerId: activeLayerId, path, x: 0, y: 0 });
    await refreshCanvas(doc.id);
    setStatus(`Placed "${path}" onto the active layer`);
  }

  return (
    <main className="app">
      <div className="doc-tabs">
        {openDocs.map((d) => (
          <div key={d.id} className={`doc-tab ${doc?.id === d.id ? "active" : ""}`} title={d.id}>
            <button className="doc-tab-label" onClick={() => switchDocument(d.id)}>
              {d.name}
              {!savedPaths[d.id] && <span className="unsaved-dot" title="Not saved to disk">•</span>}
            </button>
            <button
              className="doc-tab-close"
              onClick={(e) => {
                e.stopPropagation();
                closeTab(d.id, d.name);
              }}
              title="Close"
            >
              ×
            </button>
          </div>
        ))}
      </div>
      <header className="toolbar">
        <button onClick={() => setShowSettings(true)}>Settings</button>
        <button onClick={newDocument}>New Document</button>
        <button onClick={saveProject}>Save Project</button>
        <button onClick={openProject}>Open Project...</button>
        <button onClick={openImageAsDocument}>Open Image...</button>
        <button onClick={openPsd}>Open PSD...</button>
        <button onClick={exportPsd}>Export PSD...</button>
        <button onClick={placeImageOnActiveLayer}>Place Image...</button>
        <button onClick={flattenDocument} title="Merge all visible layers into one Background layer">Flatten Image</button>
        <button onClick={createTextLayerFn} title="Create a live, editable text layer from the Text tool panel's current text/size/color">New Text Layer</button>
        <div className="tool-group">
          {(["brush", "eraser", "select", "text", "rect", "ellipse", "pen", "gradient", "paintBucket", "cloneStamp", "dodge", "burn"] as Tool[]).map((t) => (
            <button
              key={t}
              className={tool === t ? "active" : ""}
              onClick={() => {
                setPenPoints([]);
                setTool(t);
              }}
            >
              {t === "brush"
                ? "Brush"
                : t === "eraser"
                  ? "Eraser"
                  : t === "select"
                    ? "Select"
                    : t === "text"
                      ? "Text"
                      : t === "rect"
                        ? "Rectangle"
                        : t === "ellipse"
                          ? "Ellipse"
                          : t === "pen"
                            ? "Pen"
                            : t === "gradient"
                              ? "Gradient"
                              : t === "paintBucket"
                                ? "Paint Bucket"
                                : t === "cloneStamp"
                                  ? "Clone Stamp"
                                  : t === "dodge"
                                    ? "Dodge"
                                    : "Burn"}
            </button>
          ))}
        </div>
        {tool === "pen" && (
          <div className="tool-group">
            <span style={{ fontSize: 12, padding: "0 6px" }}>Anchors: {penPoints.length}</span>
            <button onClick={() => finishPen("fill")} disabled={penPoints.length < 2}>
              Fill Path
            </button>
            <button onClick={() => finishPen("stroke")} disabled={penPoints.length < 2}>
              Stroke Path
            </button>
            <button onClick={() => setPenPoints([])} disabled={penPoints.length === 0}>
              Cancel
            </button>
          </div>
        )}
        <button onClick={clearSelection}>Clear Selection</button>
        <div className="tool-group" title={!selectionRect ? "Make a selection first (Select tool, or Rectangle/Ellipse)" : undefined}>
          <select value={fillMethod} onChange={(e) => setFillMethod(e.target.value as "classical" | "ml")}>
            <option value="ml">Generative Fill (ML - MI-GAN)</option>
            <option value="classical">Generative Fill (Classical - PDE)</option>
          </select>
          {fillMethod === "classical" && (
            <label>
              Iterations
              <input type="range" min={20} max={1000} step={20} value={fillIterations} onChange={(e) => setFillIterations(Number(e.target.value))} />
              <span style={{ fontSize: 12 }}>{fillIterations}</span>
            </label>
          )}
          <button onClick={generativeFill} disabled={!selectionRect || !activeLayerId}>
            Fill Selection
          </button>
        </div>
        <button onClick={doUndo}>Undo</button>
        <button onClick={doRedo}>Redo</button>
        <button onClick={doExport}>Export PNG</button>
        {tool === "text" ? (
          <>
            <label>
              Text
              <input type="text" value={textValue} onChange={(e) => setTextValue(e.target.value)} style={{ width: 140 }} />
            </label>
            <label>
              Size
              <input type="range" min={8} max={160} value={textSize} onChange={(e) => setTextSize(Number(e.target.value))} />
            </label>
          </>
        ) : tool === "paintBucket" ? (
          <label>
            Tolerance
            <input type="range" min={0} max={255} value={fillTolerance} onChange={(e) => setFillTolerance(Number(e.target.value))} />
          </label>
        ) : tool === "dodge" || tool === "burn" ? (
          <>
            <label>
              Brush size
              <input type="range" min={1} max={80} value={brushSize} onChange={(e) => setBrushSize(Number(e.target.value))} />
            </label>
            <label>
              Strength
              <input type="range" min={0.02} max={0.6} step={0.02} value={toneStrength} onChange={(e) => setToneStrength(Number(e.target.value))} />
            </label>
          </>
        ) : tool === "gradient" ? (
          <label>
            Type
            <select value={gradientKind} onChange={(e) => setGradientKind(e.target.value as "linear" | "radial")}>
              <option value="linear">Linear</option>
              <option value="radial">Radial</option>
            </select>
          </label>
        ) : (
          <label>
            Brush size
            <input type="range" min={1} max={80} value={brushSize} onChange={(e) => setBrushSize(Number(e.target.value))} />
          </label>
        )}
        <label>
          Color
          <input type="color" value={brushColor} onChange={(e) => setBrushColor(e.target.value)} />
        </label>
        {tool === "gradient" && (
          <label>
            Color 2
            <input type="color" value={gradientColor2} onChange={(e) => setGradientColor2(e.target.value)} />
          </label>
        )}
        {tool === "cloneStamp" && (
          <span style={{ fontSize: 12, padding: "0 6px" }}>{cloneSource ? `Source: (${Math.round(cloneSource.x)}, ${Math.round(cloneSource.y)}) - Shift+click to reset` : "Shift+click to set clone source"}</span>
        )}
      </header>

      <div className="workspace">
        <div className="canvas-wrap">
          <div
            ref={canvasRef}
            className="canvas-surface"
            style={{
              ...fitDisplaySize(doc?.width ?? CANVAS_WIDTH, doc?.height ?? CANVAS_HEIGHT),
              backgroundImage: renderUrl ? `url(${renderUrl})` : undefined,
            }}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerLeave={onPointerUp}
          >
            {selectionRect && doc && (
              <div
                className="selection-overlay"
                style={{
                  left: (selectionRect.x / doc.width) * 100 + "%",
                  top: (selectionRect.y / doc.height) * 100 + "%",
                  width: (selectionRect.w / doc.width) * 100 + "%",
                  height: (selectionRect.h / doc.height) * 100 + "%",
                }}
              />
            )}
            {tool === "pen" && penPoints.length > 0 && doc && (
              <svg className="pen-overlay" viewBox={`0 0 ${doc.width} ${doc.height}`} preserveAspectRatio="none">
                {penPoints.length > 1 && (
                  <polyline points={penPoints.map((p) => `${p.x},${p.y}`).join(" ")} fill="none" stroke="#4c7eff" strokeWidth={Math.max(1, doc.width / 300)} />
                )}
                {penPoints.map((p, i) => (
                  <circle key={i} cx={p.x} cy={p.y} r={Math.max(2, doc.width / 150)} fill="#4c7eff" />
                ))}
              </svg>
            )}
            {tool === "gradient" && gradientStart.current && gradientEnd && doc && (
              <svg className="pen-overlay" viewBox={`0 0 ${doc.width} ${doc.height}`} preserveAspectRatio="none">
                <line x1={gradientStart.current.x} y1={gradientStart.current.y} x2={gradientEnd.x} y2={gradientEnd.y} stroke="#4c7eff" strokeWidth={Math.max(1, doc.width / 300)} />
              </svg>
            )}
          </div>
        </div>

        <aside className="sidebar">
          <section className="panel">
            <h3>Document</h3>
            <label>
              Width
              <input type="number" min={1} max={16384} value={resizeW} onChange={(e) => setResizeW(Number(e.target.value))} />
            </label>
            <label>
              Height
              <input type="number" min={1} max={16384} value={resizeH} onChange={(e) => setResizeH(Number(e.target.value))} />
            </label>
            <button onClick={resizeImage}>Resize Image (scale)</button>
            <button onClick={resizeCanvasFn}>Resize Canvas (crop/pad)</button>
          </section>

          <section className="panel">
            <h3>Layers</h3>
            <button onClick={addLayer}>+ Add Layer</button>
            <button onClick={addGroup}>+ Add Group</button>
            {doc && doc.layers.some((l) => l.is_group) && (
              <label className="clip-toggle" style={{ marginTop: 6 }}>
                Move-into target:
                <select value={groupTarget} onChange={(e) => setGroupTarget(e.target.value)}>
                  <option value="">(none)</option>
                  {doc.layers
                    .filter((l) => l.is_group)
                    .map((g) => (
                      <option key={g.id} value={g.id}>
                        {g.name}
                      </option>
                    ))}
                </select>
              </label>
            )}
            <ul className="layer-list">
              {doc &&
                [...doc.layers].reverse().map((layer) => (
                  <li
                    key={layer.id}
                    className={layer.id === activeLayerId ? "active" : ""}
                    style={layer.parent_group ? { marginLeft: 16, borderLeft: "2px solid #4c7eff" } : undefined}
                    onClick={() => setActiveLayerId(layer.id)}
                  >
                    <div className="layer-row">
                      <input type="checkbox" checked={layer.visible} onChange={() => toggleVisible(layer)} onClick={(e) => e.stopPropagation()} />
                      <span className="layer-name">
                        {layer.is_group ? "📁 " : ""}
                        {layer.name}
                      </span>
                    </div>
                    {!layer.is_group && (
                      <div className="layer-controls" onClick={(e) => e.stopPropagation()}>
                        <select value={layer.blend_mode} onChange={(e) => setBlendMode(layer, e.target.value)}>
                          {BLEND_MODES.map((m) => (
                            <option key={m} value={m}>
                              {m}
                            </option>
                          ))}
                        </select>
                        <input type="range" min={0} max={1} step={0.05} value={layer.opacity} onChange={(e) => setOpacity(layer, Number(e.target.value))} />
                        <label className="clip-toggle">
                          <input type="checkbox" checked={layer.clip_to_below} onChange={() => toggleClipToBelow(layer)} />
                          Clip
                        </label>
                        <label className="clip-toggle">
                          <input type="checkbox" checked={layer.has_drop_shadow} onChange={() => toggleDropShadow(layer)} />
                          Shadow
                        </label>
                        <label className="clip-toggle">
                          <input type="checkbox" checked={layer.has_stroke} onChange={() => toggleStroke(layer)} />
                          Stroke
                        </label>
                        <label className="clip-toggle">
                          <input type="checkbox" checked={layer.has_outer_glow} onChange={() => toggleOuterGlow(layer)} />
                          Glow
                        </label>
                        <label className="clip-toggle" title="Create from the active selection; clear removes it">
                          <input type="checkbox" checked={layer.has_mask} onChange={() => toggleMask(layer)} />
                          Mask
                        </label>
                        {(layer.smart_filter_count > 0 || layer.has_mask || layer.has_text) && (
                          <span className="layer-badges">
                            {layer.has_text && <span title="Text layer">T</span>}
                            {layer.has_mask && <span title="Has a mask">M</span>}
                            {layer.smart_filter_count > 0 && <span title={`${layer.smart_filter_count} smart filter(s)`}>FX</span>}
                          </span>
                        )}
                        <button onClick={() => duplicateLayer(layer.id)}>Dup</button>
                        <button onClick={() => moveLayer(layer.id, 1)}>Up</button>
                        <button onClick={() => moveLayer(layer.id, -1)}>Down</button>
                        <button onClick={() => mergeLayerDown(layer.id)} title="Merge with the layer below">Merge Down</button>
                        <select value={smartFilterType} onChange={(e) => setSmartFilterType(e.target.value)} title="Smart filter type">
                          <option value="gaussianBlur">Gaussian Blur</option>
                          <option value="sharpen">Sharpen</option>
                          <option value="brightnessContrast">Brightness/Contrast</option>
                          <option value="hueSaturation">Hue/Saturation</option>
                          <option value="invert">Invert</option>
                        </select>
                        <button onClick={() => addSmartFilterToLayer(layer)} title="Uses the Blur/Sharpen/Brightness/Contrast/Hue/Saturation panel values below">
                          + Smart Filter
                        </button>
                        <button onClick={() => deleteLayer(layer.id)}>Del</button>
                        {layer.parent_group ? (
                          <button onClick={() => ungroupLayer(layer.id)}>Ungroup</button>
                        ) : (
                          <button onClick={() => moveIntoGroup(layer.id)} disabled={!groupTarget}>
                            → Group
                          </button>
                        )}
                      </div>
                    )}
                    {layer.has_text && (
                      <div className="layer-controls" onClick={(e) => e.stopPropagation()}>
                        <button
                          onClick={() =>
                            invoke("set_text_content", {
                              documentId: doc!.id,
                              layerId: layer.id,
                              text: textValue,
                              x: 20,
                              y: 20,
                              size: textSize,
                              color: hexToRgba(brushColor),
                            }).then(() => refreshDoc(doc!.id))
                          }
                          title="Apply the Text tool's current text/size/color to this layer"
                        >
                          Update Text (from Text tool panel)
                        </button>
                      </div>
                    )}
                    {layer.is_group && (
                      <div className="layer-controls" onClick={(e) => e.stopPropagation()}>
                        <select value={layer.blend_mode} onChange={(e) => setBlendMode(layer, e.target.value)}>
                          {BLEND_MODES.map((m) => (
                            <option key={m} value={m}>
                              {m}
                            </option>
                          ))}
                        </select>
                        <input type="range" min={0} max={1} step={0.05} value={layer.opacity} onChange={(e) => setOpacity(layer, Number(e.target.value))} />
                        <button onClick={() => deleteLayer(layer.id)}>Del</button>
                      </div>
                    )}
                  </li>
                ))}
            </ul>
          </section>

          <section className="panel">
            <h3>Layer Comps</h3>
            <label>
              Name
              <input type="text" value={compName} onChange={(e) => setCompName(e.target.value)} style={{ width: 100 }} />
            </label>
            <button onClick={createComp}>+ Save Comp (current visibility/opacity)</button>
            <ul className="layer-list">
              {layerComps.map((c) => (
                <li key={c.id}>
                  <div className="layer-row">
                    <span className="layer-name">{c.name}</span>
                    <button onClick={() => applyComp(c.id)}>Apply</button>
                    <button onClick={() => deleteComp(c.id)}>Del</button>
                  </div>
                </li>
              ))}
            </ul>
          </section>

          <section className="panel">
            <h3>Adjustments (active layer)</h3>
            <label>
              Brightness
              <input type="range" min={-1} max={1} step={0.05} value={brightness} onChange={(e) => setBrightness(Number(e.target.value))} />
            </label>
            <label>
              Contrast
              <input type="range" min={-1} max={1} step={0.05} value={contrast} onChange={(e) => setContrast(Number(e.target.value))} />
            </label>
            <button onClick={applyBrightnessContrast}>Apply Brightness/Contrast</button>
            <label>
              Hue
              <input type="range" min={-180} max={180} step={1} value={hue} onChange={(e) => setHue(Number(e.target.value))} />
            </label>
            <label>
              Saturation
              <input type="range" min={-1} max={1} step={0.05} value={saturation} onChange={(e) => setSaturation(Number(e.target.value))} />
            </label>
            <button onClick={applyHueSaturation}>Apply Hue/Saturation</button>
          </section>

          <section className="panel">
            <h3>Filters (active layer)</h3>
            <label>
              Blur radius
              <input type="range" min={0} max={30} step={0.5} value={blurRadius} onChange={(e) => setBlurRadius(Number(e.target.value))} />
            </label>
            <button onClick={applyBlur}>Apply Gaussian Blur</button>
            <label>
              Sharpen radius
              <input type="range" min={0} max={10} step={0.1} value={sharpenRadius} onChange={(e) => setSharpenRadius(Number(e.target.value))} />
            </label>
            <button onClick={applySharpenFilter}>Apply Sharpen</button>
          </section>

          <section className="panel">
            <h3>AI (active layer)</h3>
            <button onClick={cutoutSubject}>Cutout Subject</button>
            <button onClick={selectSubject}>Select Subject</button>
          </section>

          <section className="panel">
            <h3>Color Management</h3>
            <label>
              From
              <select value={fromProfile} onChange={(e) => setFromProfile(e.target.value)}>
                {PROFILES.map((p) => (
                  <option key={p} value={p}>
                    {p}
                  </option>
                ))}
              </select>
            </label>
            <label>
              To
              <select value={toProfile} onChange={(e) => setToProfile(e.target.value)}>
                {PROFILES.map((p) => (
                  <option key={p} value={p}>
                    {p}
                  </option>
                ))}
              </select>
            </label>
            <button onClick={convertProfile}>Convert Profile</button>
            <button onClick={applyCmykSoftProof}>CMYK Soft Proof (active layer)</button>
            <button onClick={exportHighBitDepth}>Export 16-bit PNG</button>
          </section>
        </aside>
      </div>

      <footer className="status-bar">{status || "Ready"}</footer>
      {showSettings && <Settings onClose={() => setShowSettings(false)} />}
      {closeTabTarget && (
        <CloseTabDialog
          docName={closeTabTarget.name}
          onCancel={() => setCloseTabTarget(null)}
          onDiscard={() => {
            const target = closeTabTarget;
            setCloseTabTarget(null);
            finishCloseTab(target.id);
          }}
          onSave={async () => {
            const target = closeTabTarget;
            setCloseTabTarget(null);
            const path = await saveProjectAs(target.id, target.name);
            if (path) await finishCloseTab(target.id); // cancelled save dialog -> leave the tab open
          }}
        />
      )}
    </main>
  );
}

export default App;

// Model bar + engine presets: the titlebar Local/Remote badge, the model status
// fan-out (Run gate + badge + bar), backend presets (llamacpp local vs remote
// vllm/sglang/custom), the Load/Unload buttons, app-lifetime load-progress
// listeners, and the quant tier labels / cached markers.

import { requireTauri } from "./tauri.js";

/** Update the titlebar Local/Remote/No-model badge from a model_status payload. */
export function updateModeBadge(status) {
  const badge = document.getElementById("modeBadge");
  if (!badge) return;
  const loaded = !!(status && status.loaded);
  const mode = status && status.mode;
  const dotClass = !loaded ? "is-off" : mode === "remote" ? "is-remote" : "is-loaded";
  const label = !loaded ? "No model" : mode === "remote" ? "Remote" : "Local";
  badge.innerHTML = '<span class="titlebar__mode-dot ' + dotClass + '"></span>' + label;
}

/** Fetch model_status and fan it out: the Run gate (ui), the titlebar badge, and
 *  the model bar (status text + Load/Unload enablement). Called on startup and
 *  after every load/unload. Fail-soft outside the webview. */
export async function refreshModelStatus(ui) {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  let status = { loaded: false, mode: "", label: "" };
  try {
    status = await t.core.invoke("model_status");
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[model] status failed", err);
  }
  if (ui && typeof ui.applyModelStatus === "function") ui.applyModelStatus(status);
  updateModeBadge(status);
  const loadBtn = document.getElementById("loadModelBtn");
  const unloadBtn = document.getElementById("unloadModelBtn");
  const statusText = document.getElementById("modelStatusText");
  if (unloadBtn) unloadBtn.disabled = !status.loaded;
  if (loadBtn) loadBtn.textContent = status.loaded ? "Reload model" : "Load model";
  if (statusText) {
    statusText.textContent = status.loaded ? "Loaded: " + status.label : "No model loaded";
  }
  // Upgrade the unloaded "Load model" label to "Download & load model" when the
  // selected local quant is not yet cached (best-effort; loaded state is left).
  updateLoadLabel();
}

/** When no model is loaded, label the Load button "Download & load model" if the
 *  selected local quant is not cached on disk, else "Load model". Remote backends
 *  and a custom GGUF override never download a quant, so they stay "Load model".
 *  Best-effort: silent outside the webview or if list_local_models fails. */
async function updateLoadLabel() {
  const loadBtn = document.getElementById("loadModelBtn");
  if (!loadBtn || loadBtn.textContent === "Reload model") return; // loaded: leave it
  if (activeEngineMode() !== "local") {
    loadBtn.textContent = "Load model";
    return;
  }
  if (pickedGguf("modelFilePath")) {
    loadBtn.textContent = "Load model";
    return;
  }
  const quant = document.getElementById("optQuant")?.value;
  let cached = [];
  try {
    cached = await requireTauri().core.invoke("list_local_models");
  } catch (err) {
    return;
  }
  loadBtn.textContent = (cached || []).includes(quant)
    ? "Load model"
    : "Download & load model";
}

// Backend presets. llamacpp = managed-local spawn (Quant control drives it, no
// URL); vllm/sglang/custom are remote OpenAI-compatible endpoints. Non-custom
// presets keep the URL/key/model fields hidden; vllm/sglang prefill #remoteUrl
// with the backend's default port (base URL only -- the backend appends
// /v1/chat/completions, so no /v1 suffix here). load_model reads mode + the
// (possibly prefilled) #remoteUrl, so presets need no extra plumbing.
export const ENGINE_PRESETS = {
  llamacpp: { mode: "local", url: null },
  // Full (non-GGUF) DeepSeek-OCR on GPU: same remote path as vllm, but also
  // prefills the model name so the user does not have to know the repo id.
  gpu: { mode: "remote", url: "http://127.0.0.1:8000", model: "deepseek-ai/DeepSeek-OCR" },
  vllm: { mode: "remote", url: "http://127.0.0.1:8000" },
  sglang: { mode: "remote", url: "http://127.0.0.1:30000" },
  custom: { mode: "remote", url: null },
};

/** Apply a backend preset: toggle the remote field visibility (editable only for
 *  Custom), prefill the URL for vllm/sglang, and hide the Quant control for any
 *  remote backend (quant only applies to the managed-local spawn). */
export function applyPreset(name) {
  const p = ENGINE_PRESETS[name] || ENGINE_PRESETS.llamacpp;
  const remoteFields = document.getElementById("remoteFields");
  if (remoteFields) remoteFields.hidden = name !== "custom";
  // Custom-GGUF pickers apply only to the managed-local spawn (they replace the
  // download + quant naming); hide them for any remote backend.
  const localFields = document.getElementById("localFields");
  if (localFields) localFields.hidden = p.mode !== "local";
  if (p.url) {
    const url = document.getElementById("remoteUrl");
    if (url) url.value = p.url;
  }
  // The GPU preset also prefills the served model name (vLLM needs it in the
  // request body); other presets leave #remoteModel as the user set it.
  if (p.model) {
    const modelEl = document.getElementById("remoteModel");
    if (modelEl) modelEl.value = p.model;
  }
  const quantEl = document.getElementById("optQuant");
  const quantField = quantEl && quantEl.closest(".opts__field");
  if (quantField) quantField.hidden = p.mode !== "local";
  // Show the GPU prerequisites hint only for the GPU preset.
  const gpuHint = document.getElementById("gpuHint");
  if (gpuHint) gpuHint.hidden = name !== "gpu";
}

/** Wire the OCR engine backend preset dropdown. Changing it re-applies the preset
 *  (field visibility + URL prefill). The selected preset's mode is read by the
 *  Load button to pick local vs remote. */
export function wireEnginePreset() {
  const sel = document.getElementById("enginePreset");
  if (!sel) return;
  sel.addEventListener("change", () => {
    applyPreset(sel.value);
    // Mode may have flipped local<->remote: refresh the download/load label.
    updateLoadLabel();
  });
  applyPreset(sel.value);
}

/** Open the engine-connection modal from the Modify button. applyPreset() has
 *  already toggled which block (remote vs custom-GGUF) is visible inside it.
 *  Native <dialog>: backdrop, Esc, and the Done/× forms close it for free. */
export function wireEngineDialog() {
  const btn = document.getElementById("engineModifyBtn");
  const dlg = document.getElementById("engineDialog");
  if (!btn || !dlg || typeof dlg.showModal !== "function") return;
  btn.addEventListener("click", () => dlg.showModal());
}

/** Return the active backend's mode ("local" | "remote"). */
export function activeEngineMode() {
  const sel = document.getElementById("enginePreset");
  const name = sel ? sel.value : "llamacpp";
  return (ENGINE_PRESETS[name] || ENGINE_PRESETS.llamacpp).mode;
}

/** Wire the Load/Unload model buttons. Load reads the active engine mode + the
 *  quant (local) or remote URL/key (remote) and calls load_model, then refreshes
 *  status. Loading is long (download + health wait) so the button shows progress
 *  via the app-lifetime ocr:// listeners attached in attachLoadListeners. */
/** Read a custom-GGUF picker's chosen path, or null if none picked / element
 *  missing. Stored on the span's dataset by wireGgufPicker. */
function pickedGguf(spanId) {
  const el = document.getElementById(spanId);
  const p = el && el.dataset ? el.dataset.path : "";
  return p && p.trim() ? p : null;
}

/** Wire one GGUF file picker: clicking the button opens the native dialog
 *  (tauri-plugin-dialog, same as the PDF importer) filtered to .gguf, and the
 *  chosen path is stored on the paired span (dataset.path + visible basename).
 *  Clicking the span clears the selection. No-op outside the Tauri shell. */
function wireGgufPicker(btnId, spanId, label) {
  const btn = document.getElementById(btnId);
  const span = document.getElementById(spanId);
  if (!btn || !span) return;
  const setPath = (p) => {
    if (p) {
      span.dataset.path = p;
      span.textContent = p.split(/[/\\]/).pop();
      span.title = p;
    } else {
      delete span.dataset.path;
      span.textContent = "none";
      span.title = "";
    }
    // A custom model GGUF skips the download, so the Load label drops the
    // "Download &" prefix; clearing it restores the cached/uncached label.
    updateLoadLabel();
  };
  setPath(null);
  btn.addEventListener("click", async () => {
    const dialog = window.__TAURI__ && window.__TAURI__.dialog;
    if (!dialog || !dialog.open) return;
    try {
      const selected = await dialog.open({
        multiple: false,
        directory: false,
        filters: [{ name: "GGUF", extensions: ["gguf"] }],
      });
      if (typeof selected === "string" && selected.trim()) setPath(selected);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[model] " + label + " picker failed:", err.message);
    }
  });
  // Click the path span to clear the override.
  span.addEventListener("click", () => setPath(null));
}

export function wireModelBar(ui) {
  const loadBtn = document.getElementById("loadModelBtn");
  const unloadBtn = document.getElementById("unloadModelBtn");
  const statusText = document.getElementById("modelStatusText");

  // Custom-GGUF pickers (local mode). Each button opens the native dialog and
  // stores the chosen path on the paired span's dataset; the load handler reads
  // it. Clearing is via the span's clear button. No path picked -> null sent.
  wireGgufPicker("pickModelBtn", "modelFilePath", "model GGUF");
  wireGgufPicker("pickMmprojBtn", "mmprojFilePath", "projector GGUF");

  // Quant change can flip cached<->uncached: refresh the download/load label.
  const quantEl = document.getElementById("optQuant");
  if (quantEl) quantEl.addEventListener("change", () => updateLoadLabel());

  if (loadBtn) {
    loadBtn.addEventListener("click", async () => {
      let t;
      try {
        t = requireTauri();
      } catch (err) {
        if (statusText) statusText.textContent = "unavailable outside the app";
        return;
      }
      const mode = activeEngineMode();
      const quantEl = document.getElementById("optQuant");
      const urlEl = document.getElementById("remoteUrl");
      const keyEl = document.getElementById("remoteKey");
      const modelEl = document.getElementById("remoteModel");
      // Startup-only DeepSeek-OCR knobs read at load time (they parameterize the
      // llama-server spawn). Blank/invalid -> null so the backend omits the flag.
      const imtEl = document.getElementById("optImageMaxTokens");
      const ctEl = document.getElementById("optChatTemplate");
      const imtVal = imtEl ? parseInt(imtEl.value || "", 10) : NaN;
      loadBtn.disabled = true;
      if (unloadBtn) unloadBtn.disabled = true;
      if (statusText) statusText.textContent = "loading…";
      try {
        const status = await t.core.invoke("load_model", {
          mode,
          quant: quantEl ? quantEl.value : null,
          baseUrl: urlEl ? urlEl.value : null,
          apiKey: keyEl ? keyEl.value : null,
          model: modelEl ? modelEl.value : null,
          llamaBin: null,
          imageMaxTokens: Number.isFinite(imtVal) && imtVal > 0 ? imtVal : null,
          chatTemplate: ctEl && ctEl.value ? ctEl.value : null,
          modelFile: pickedGguf("modelFilePath"),
          mmprojFile: pickedGguf("mmprojFilePath"),
        });
        if (ui) ui.applyModelStatus(status);
      } catch (err) {
        if (statusText) statusText.textContent = "load failed: " + String(err);
      } finally {
        loadBtn.disabled = false;
        await refreshModelStatus(ui);
      }
    });
  }

  if (unloadBtn) {
    unloadBtn.addEventListener("click", async () => {
      let t;
      try {
        t = requireTauri();
      } catch (err) {
        return;
      }
      try {
        await t.core.invoke("unload_model");
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[model] unload failed", err);
      }
      await refreshModelStatus(ui);
    });
  }
}

/** App-lifetime listeners that surface model LOAD progress in the model bar
 *  (download pct + server-ready). These events now fire during load_model, not
 *  run_ocr, so they belong to the model bar rather than the per-run subscription. */
export function attachLoadListeners() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const statusText = document.getElementById("modelStatusText");
  t.event.listen("ocr://progress", (e) => {
    const { name, pct } = (e && e.payload) || {};
    if (statusText) statusText.textContent = "downloading " + (name || "model") + " " + (pct || 0) + "%";
  });
  t.event.listen("ocr://server-ready", (e) => {
    const { port } = (e && e.payload) || {};
    if (statusText) statusText.textContent = "server ready on :" + port;
  });
}

/** Map a raw quant value to the human-readable tier label used in the select.
 *  Matches the CLI tier semantics: best=BF16, good=Q8_0, less=Q4_K_M.
 *  Unknown quants fall back to the raw value so future quants degrade gracefully. */
export function quantTierLabel(quant) {
  const TIERS = { BF16: "best", Q8_0: "good", Q4_K_M: "less" };
  const tier = TIERS[quant];
  return tier ? tier + " (" + quant + ")" : quant;
}

/** Mark which quant options are already cached on disk (list_local_models) by
 *  appending a check to their label. Applies to both the run-time Quant select and
 *  the Settings default-quant select. Best-effort; never throws. Preserves the tier
 *  label prefix so the cached marker appends to "good (Q8_0)", not just "Q8_0". */
export async function markCachedQuants() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  let cached = [];
  try {
    cached = await t.core.invoke("list_local_models");
  } catch (err) {
    return;
  }
  const set = new Set(cached || []);
  ["optQuant", "setQuant"].forEach((id) => {
    const sel = document.getElementById(id);
    if (!sel) return;
    Array.from(sel.options).forEach((opt) => {
      const base = opt.value;
      const label = quantTierLabel(base);
      opt.textContent = set.has(base) ? label + " ✓ cached" : label;
    });
  });
}

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
}

// Backend presets. llamacpp = managed-local spawn (Quant control drives it, no
// URL); vllm/sglang/custom are remote OpenAI-compatible endpoints. Non-custom
// presets keep the URL/key/model fields hidden; vllm/sglang prefill #remoteUrl
// with the backend's default port (base URL only -- the backend appends
// /v1/chat/completions, so no /v1 suffix here). load_model reads mode + the
// (possibly prefilled) #remoteUrl, so presets need no extra plumbing.
export const ENGINE_PRESETS = {
  llamacpp: { mode: "local", url: null },
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
  if (p.url) {
    const url = document.getElementById("remoteUrl");
    if (url) url.value = p.url;
  }
  const quantEl = document.getElementById("optQuant");
  const quantField = quantEl && quantEl.closest(".opts__field");
  if (quantField) quantField.hidden = p.mode !== "local";
}

/** Wire the OCR engine backend preset dropdown. Changing it re-applies the preset
 *  (field visibility + URL prefill). The selected preset's mode is read by the
 *  Load button to pick local vs remote. */
export function wireEnginePreset() {
  const sel = document.getElementById("enginePreset");
  if (!sel) return;
  sel.addEventListener("change", () => applyPreset(sel.value));
  applyPreset(sel.value);
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
export function wireModelBar(ui) {
  const loadBtn = document.getElementById("loadModelBtn");
  const unloadBtn = document.getElementById("unloadModelBtn");
  const statusText = document.getElementById("modelStatusText");

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

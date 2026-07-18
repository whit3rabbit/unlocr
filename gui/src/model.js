// Model bar + engine presets: the titlebar Local/Remote badge, the model status
// fan-out (Run gate + badge + bar), backend presets (llamacpp local vs remote
// vllm/sglang/custom), the Load/Unload buttons, app-lifetime load-progress
// listeners, and the quant tier labels / cached markers.

import { requireTauri } from "./tauri.js";
import { showToast, removeToast, addNotification } from "./toasts.js";

// EH-0013 bite 2: i18n hook. Named `tr` -- `t` is the Tauri handle in every fn.
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

// Last-known loaded state, stamped by refreshModelStatus. updateLoadLabel reads
// THIS rather than inferring loaded state from the button's textContent, so a
// live locale switch (which changes tr("model.reload") under the button) cannot
// make it relabel a loaded model as "Load model".
let modelLoaded = false;

// True only while a load is in flight (set on Load click, cleared in its finally).
// The ocr://progress / status / server-ready listeners are gated on this so a
// late-arriving server-ready event cannot clobber the resting "Loaded: ..." text
// after refreshModelStatus has already set it.
let loadingModel = false;

// Load-phase "still alive" feedback. The backend emits ocr://status ("loading
// model into memory…") ONCE, then Server::start blocks in await_health for 30-90s
// (CPU load of the multi-GB GGUF) emitting nothing; meanwhile the download toast
// has already been removed, so both the model bar and the toast stack look frozen.
// This ticks a 1s elapsed-seconds counter on both surfaces until server-ready or
// the load resolves, so the user can see it is working, not hung.
let loadHeartbeat = null;
let loadHeartbeatBase = "";
let loadHeartbeatStart = 0;
const LOAD_TOAST_ID = "load:memory";
// Persistent (not heartbeat) toast for a load FAILURE. Separate id so
// stopLoadFeedback (which only removes LOAD_TOAST_ID) leaves the error visible.
const LOAD_ERROR_TOAST_ID = "load:error";

/** Stop the load heartbeat and clear its toast. Idempotent. */
function stopLoadFeedback() {
  if (loadHeartbeat) {
    clearInterval(loadHeartbeat);
    loadHeartbeat = null;
  }
  removeToast(LOAD_TOAST_ID);
}

/** Show `baseMsg` on the model bar + a toast, both with a live "(Ns)" elapsed
 *  counter, so a long event-less load phase is visibly progressing. Self-stops if
 *  the load gate (loadingModel) drops. ponytail: "(Ns)" is an elapsed unit, not
 *  prose, so it is not routed through i18n (the base message is backend-supplied
 *  and already English). */
function startLoadFeedback(baseMsg) {
  stopLoadFeedback();
  loadHeartbeatBase = baseMsg || tr("model.loading");
  loadHeartbeatStart = Date.now();
  const statusText = document.getElementById("modelStatusText");
  const paint = () => {
    const secs = Math.floor((Date.now() - loadHeartbeatStart) / 1000);
    if (statusText) statusText.textContent = loadHeartbeatBase + " (" + secs + "s)";
    showToast(LOAD_TOAST_ID, { kind: "download", title: loadHeartbeatBase, meta: secs + "s" });
  };
  paint();
  loadHeartbeat = setInterval(() => {
    if (!loadingModel) {
      stopLoadFeedback();
      return;
    }
    paint();
  }, 1000);
}

/** Paint the titlebar model-light + label from a model_status payload. This is
 *  the single at-a-glance status (the model-bar text below carries detail):
 *  green dot + the loaded model label (e.g. "Unlimited-OCR Q8_0") for a local
 *  model, violet dot + the label for a remote endpoint, gray dot + "No model"
 *  when nothing is loaded. The dot color carries loaded/idle/remote state, so
 *  no "Local"/"Loaded:" prefix is needed. */
export function updateModeBadge(status) {
  const badge = document.getElementById("modeBadge");
  if (!badge) return;
  const loaded = !!(status && status.loaded);
  const mode = status && status.mode;
  const dotClass = !loaded ? "is-off" : mode === "remote" ? "is-remote" : "is-loaded";
  const fallback = mode === "remote" ? tr("model.remote") : tr("model.local");
  const label = !loaded ? tr("status.noModel") : (status && status.label) || fallback;
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
  modelLoaded = !!status.loaded;
  if (unloadBtn) unloadBtn.disabled = !status.loaded;
  if (loadBtn) loadBtn.textContent = status.loaded ? tr("model.reload") : tr("model.load");
  if (statusText) {
    statusText.textContent = status.loaded ? tr("model.loadedLabel", { label: status.label }) : tr("model.noModelLoaded");
  }
  // Upgrade the unloaded "Load model" label to "Download & load model" when the
  // selected local quant is not yet cached (best-effort; loaded state is left).
  updateLoadLabel();
  // Signal load/unload/startup completion so the file-rail pipeline pane can
  // re-run preflight (a download may have added the GGUF). DOM event is the seam
  // (this fn has no `rail`), same pattern as the GGUF picker's unlocr:gguf-changed.
  document.dispatchEvent(new CustomEvent("unlocr:model-changed"));
}

/** When no model is loaded, label the Load button "Download & load model" if the
 *  selected local quant is not cached on disk, else "Load model". Remote backends
 *  and a custom GGUF override never download a quant, so they stay "Load model".
 *  MLX also stays "Load model": mlxcel owns its own HF cache (~/.cache/mlxcel),
 *  which unlocr can't probe, so we can't tell a first-run download from a warm
 *  cache -- "Load model" is the least-wrong label (the #mlxHint already warns the
 *  first run downloads the model). Best-effort: silent outside the webview or if
 *  list_local_models fails. */
async function updateLoadLabel() {
  const loadBtn = document.getElementById("loadModelBtn");
  if (!loadBtn || modelLoaded) return; // a model is loaded: leave the "Reload" label
  if (activeEngineMode() !== "local") {
    loadBtn.textContent = tr("model.load");
    return;
  }
  if (pickedGguf("modelFilePath")) {
    loadBtn.textContent = tr("model.load");
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
    ? tr("model.load")
    : tr("model.downloadLoad");
}

// Backend presets. llamacpp = managed-local spawn (Quant control drives it, no
// URL); vllm/sglang/custom are remote OpenAI-compatible endpoints. Non-custom
// presets keep the URL/key/model fields hidden; vllm/sglang prefill #remoteUrl
// with the backend's default port (base URL only -- the backend appends
// /v1/chat/completions, so no /v1 suffix here). load_model reads mode + the
// (possibly prefilled) #remoteUrl, so presets need no extra plumbing.
export const ENGINE_PRESETS = {
  llamacpp: { mode: "local", url: null },
  // Full (non-GGUF) Unlimited-OCR on GPU: same remote path as vllm, but also
  // prefills the served model name so the user does not have to know the repo id.
  gpu: { mode: "remote", url: "http://127.0.0.1:8000", model: "baidu/Unlimited-OCR" },
  // model: "" = clear the field (so a stale gpu name is not sent); user types their
  // own served name. custom/llamacpp omit `model` entirely = leave the field as-is
  // (preserves a value restored from saved settings).
  vllm: { mode: "remote", url: "http://127.0.0.1:8000", model: "" },
  sglang: { mode: "remote", url: "http://127.0.0.1:30000", model: "" },
  // Local mlxcel-server (Apple Silicon only, github.com/lablup/mlxcel). Not a
  // remote endpoint: no URL/key needed. The model repo is picked via the same
  // #optQuant control local mode uses (see MLX_QUANTS/applyPreset below), not
  // the remote-model field.
  mlx: { mode: "mlx", url: null },
  custom: { mode: "remote", url: null },
};

// The 4 published MLX quants (sahilchachra/unlimited-ocr-*-mlx on Hugging
// Face). Hand-mirrored here (no backend "list mlx quants" command -- mlxcel
// manages its own HF cache, unlocr never sees the files) the same way
// options.js's TASK_PROMPTS mirrors cli_args::Task::prompt(); keep in sync
// with `recommend_model`'s tiers in src/server/mlx.rs if the lineup changes.
// tier aliases mirror the GGUF best/good/less convention: mxfp8 has the best
// CER among the quantized set, 8bit is the balanced default, 4bit is
// smallest/fastest; mxfp4 has no alias (a close cousin of 4bit, similar
// tradeoffs, shown as a plain extra option like the 10 alias-less GGUF quants).
const MLX_QUANTS = [
  { name: "sahilchachra/unlimited-ocr-mxfp8-mlx", tier: "best", cached: false },
  { name: "sahilchachra/unlimited-ocr-8bit-mlx", tier: "good", cached: false },
  { name: "sahilchachra/unlimited-ocr-mxfp4-mlx", tier: null, cached: false },
  { name: "sahilchachra/unlimited-ocr-4bit-mlx", tier: "less", cached: false },
];
// Mirrors unlocr::server::MLX_DEFAULT_MODEL (src/server/mlx.rs) -- keep in sync.
const MLX_DEFAULT_MODEL = "sahilchachra/unlimited-ocr-8bit-mlx";

// Last GGUF lineup fetched by populateQuantSelects, cached so applyPreset can
// restore it when switching back from mlx to llamacpp without a re-fetch (and
// so a preset switch that happens before the first fetch resolves doesn't
// leave the select empty -- see populateQuantSelects' mlx guard below).
let ggufQuantsCache = [];

/** Rebuild `ids`' `<option>` lists from `quants` ({name, tier, cached}[]),
 *  preserving each select's current selection when it is still present, else
 *  falling back to `fallbackName`. Shared by populateQuantSelects (GGUF,
 *  backend-fetched) and applyPreset's mlx branch (static MLX_QUANTS). */
function renderQuantOptions(ids, quants, fallbackName) {
  const names = new Set(quants.map((q) => q.name));
  ids.forEach((id) => {
    const sel = document.getElementById(id);
    if (!sel) return;
    const prevValue = sel.value;
    sel.innerHTML = "";
    quants.forEach((q) => {
      const opt = document.createElement("option");
      opt.value = q.name;
      const label = quantTierLabel(q.name, q.tier);
      opt.textContent = q.cached ? label + tr("model.cached") : label;
      sel.appendChild(opt);
    });
    sel.value = names.has(prevValue) ? prevValue : names.has(fallbackName) ? fallbackName : sel.value;
  });
}

/** Apply a backend preset: toggle the remote field visibility (editable only for
 *  Custom), prefill the URL for vllm/sglang, and repopulate the Quant control --
 *  GGUF quants for llamacpp, the 4 MLX quants for mlx, hidden for a true remote
 *  endpoint (quant/MLX-repo selection only applies to a managed-local spawn). */
export function applyPreset(name) {
  const p = ENGINE_PRESETS[name] || ENGINE_PRESETS.llamacpp;
  const remoteFields = document.getElementById("remoteFields");
  if (remoteFields) remoteFields.hidden = name !== "custom";
  // Custom-GGUF pickers apply only to the managed-local llama.cpp spawn (they
  // replace the download + quant naming); hide them for any other backend.
  const localFields = document.getElementById("localFields");
  if (localFields) localFields.hidden = p.mode !== "local";
  if (p.url) {
    const url = document.getElementById("remoteUrl");
    if (url) url.value = p.url;
  }
  // gpu prefills the served model name (vLLM needs it in the request body); vllm/
  // sglang set "" to clear a stale gpu name; custom/llamacpp omit `model` so a
  // saved value restored into the field is preserved (undefined = leave as-is).
  if (p.model !== undefined) {
    const modelEl = document.getElementById("remoteModel");
    if (modelEl) modelEl.value = p.model;
  }
  const quantEl = document.getElementById("optQuant");
  const quantField = quantEl && quantEl.closest(".opts__field");
  // Shown for local (GGUF quant) and mlx (MLX quant repo); hidden only for a
  // true remote endpoint, which has no quant/repo concept of its own.
  if (quantField) quantField.hidden = p.mode === "remote";
  if (name === "mlx") {
    renderQuantOptions(["optQuant", "setQuant", "qsQuant"], MLX_QUANTS, MLX_DEFAULT_MODEL);
  } else if (p.mode === "local" && ggufQuantsCache.length) {
    // Only re-render when the GGUF lineup is actually known. If applyPreset fires
    // before populateQuantSelects has resolved (empty cache), rendering [] would
    // wipe the selects to zero options; leave the DOM as-is and let the pending
    // populateQuantSelects fill them (it re-renders for the live llamacpp preset).
    renderQuantOptions(["optQuant", "setQuant", "qsQuant"], ggufQuantsCache, "Q8_0");
  }
  // Show the GPU prerequisites hint only for the GPU preset.
  const gpuHint = document.getElementById("gpuHint");
  if (gpuHint) gpuHint.hidden = name !== "gpu";
  const mlxHint = document.getElementById("mlxHint");
  if (mlxHint) mlxHint.hidden = name !== "mlx";
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

/** Return the active backend's mode ("local" | "mlx" | "remote"). */
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
      span.textContent = tr("model.none");
      span.title = "";
    }
    // A custom model GGUF skips the download, so the Load label drops the
    // "Download &" prefix; clearing it restores the cached/uncached label.
    updateLoadLabel();
    // Let settings.js's auto-save persist this pick/clear without a direct
    // import (settings.js already imports FROM model.js; the reverse would
    // be circular). A plain DOM event is the decoupling seam.
    span.dispatchEvent(new CustomEvent("unlocr:gguf-changed", { bubbles: true }));
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

/**
 * Wire up event listeners and custom file pickers for the model control bar.
 * @param {Object} ui - The UI controller instance.
 */
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
        if (statusText) statusText.textContent = tr("model.unavailableOutside");
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
      if (statusText) statusText.textContent = tr("model.loading");
      // Drop any error toast from a previous failed attempt so a retry doesn't
      // stack a stale reason next to the new one.
      removeToast(LOAD_ERROR_TOAST_ID);
      loadingModel = true;
      try {
        const status = await t.core.invoke("load_model", {
          mode,
          // #optQuant doubles as the MLX quant/model-repo picker when mode is
          // "mlx" (see applyPreset); send it as `model` (the field load_model's
          // "mlx" arm reads), not `quant` (GGUF-only, ignored for mlx/remote).
          quant: mode === "local" && quantEl ? quantEl.value : null,
          baseUrl: urlEl ? urlEl.value : null,
          apiKey: keyEl ? keyEl.value : null,
          model: mode === "mlx" ? (quantEl ? quantEl.value : null) : modelEl ? modelEl.value : null,
          // Explicit llama-server override from Settings (#setLlamaBin). Empty =
          // null so the backend auto-resolves (managed cached -> download -> PATH).
          // A set path is flagged External in the preflight/pipeline provenance.
          llamaBin: ((document.getElementById("setLlamaBin") || {}).value || "").trim() || null,
          imageMaxTokens: Number.isFinite(imtVal) && imtVal > 0 ? imtVal : null,
          chatTemplate: ctEl && ctEl.value ? ctEl.value : null,
          modelFile: pickedGguf("modelFilePath"),
          mmprojFile: pickedGguf("mmprojFilePath"),
        });
        if (ui) ui.applyModelStatus(status);
        // Paint the badge + status text DIRECTLY from the known load result
        // instead of waiting on refreshModelStatus's model_status re-fetch (a
        // late ocr://server-ready event can otherwise land after it and pin the
        // text on "server ready on :PORT"). loadingModel is dropped in finally
        // BEFORE that re-fetch, so those listeners cannot clobber this.
        updateModeBadge(status);
        if (statusText) statusText.textContent = tr("model.loadedLabel", { label: status.label });
      } catch (err) {
        const msg = tr("model.loadFailed", { error: String(err) });
        if (statusText) statusText.textContent = msg;
        // The load heartbeat toast is removed in `finally`; without a separate,
        // PERSISTENT error toast the failure only lands in the small model-bar
        // status line and reads as "the toast just vanished" (esp. on a fast MLX
        // failure). Distinct id => stopLoadFeedback (removes LOAD_TOAST_ID only)
        // does not tear it down. Also persist to the bell so it survives scroll.
        showToast(LOAD_ERROR_TOAST_ID, { kind: "error", title: msg });
        addNotification("error", tr("model.load"), msg);
      } finally {
        // Load resolved (success line above / catch). Kill the heartbeat + its
        // toast now so a pending tick cannot overwrite the final "Loaded: ..."
        // label; loadingModel drops just below as a second backstop.
        stopLoadFeedback();
        loadBtn.disabled = false;
        // Clear the gate BEFORE refreshModelStatus's await: otherwise a late
        // ocr://server-ready landing during that re-fetch passes the gate and
        // overwrites the "Loaded: ..." label above with "server ready on :PORT".
        loadingModel = false;
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
      // Feedback + badge flip immediately: the backend kill/drop is fast, but
      // without this the model bar sits on "server ready on :PORT" until
      // refreshModelStatus lands "No model loaded". loadingModel is false here,
      // so the gated load listeners cannot overwrite this.
      loadingModel = false;
      if (statusText) statusText.textContent = tr("model.stopping");
      updateModeBadge({ loaded: false, mode: "", label: "" });
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
    if (!loadingModel) return;
    const { name, pct } = (e && e.payload) || {};
    if (statusText) statusText.textContent = tr("model.downloadingPct", { name: name || tr("model.model"), pct: pct || 0 });
  });
  t.event.listen("ocr://status", (e) => {
    if (!loadingModel) return;
    const { message } = (e && e.payload) || {};
    // This status marks the long, event-less "loading into memory" phase; drive
    // the elapsed-seconds heartbeat (bar + toast) so it does not look frozen.
    if (message) startLoadFeedback(message);
  });
  t.event.listen("ocr://server-ready", (e) => {
    if (!loadingModel) return;
    stopLoadFeedback();
    const { port } = (e && e.payload) || {};
    if (statusText) statusText.textContent = tr("model.serverReady", { port });
  });
}

/** Format a quant's display label: "good (Q8_0)"-style when the backend
 *  supplies a best/good/less tier alias, else just the raw quant tag (the 10
 *  quants with no CLI Quality alias). */
export function quantTierLabel(name, tier) {
  return tier ? tr("tier.format", { tier: tr("tier." + tier), quant: name }) : name;
}

/** Rebuild the 3 quant `<select>`s (`#optQuant`, `#setQuant`, `#qsQuant`) from
 *  the backend's full published GGUF lineup (list_available_quants: all 13
 *  quants, not just the 3 CLI Quality tiers), marking each already-cached
 *  option. Replaces the previously-static `<option>` lists in index.html.
 *  Preserves each select's current selection when it's still an available
 *  quant, else falls back to Q8_0 (OcrOptions::default().quant). Caches the
 *  fetched lineup in `ggufQuantsCache` so applyPreset can restore it on an
 *  mlx->llamacpp switch without re-fetching. If mlx is the LIVE preset when
 *  this resolves, the DOM is left alone (its MLX options stay visible); the
 *  cache still updates, so switching back to llamacpp picks up the fresh
 *  list immediately. Best-effort; never throws. */
export async function populateQuantSelects() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  let quants = [];
  try {
    quants = await t.core.invoke("list_available_quants");
  } catch (err) {
    return;
  }
  ggufQuantsCache = quants;
  // mlx owns #optQuant with its static MLX_QUANTS; don't overwrite those with
  // the GGUF lineup. Still re-render MLX_QUANTS so a locale switch (this runs
  // via markCachedQuants on onLocaleChange) re-translates the tier labels.
  if (activeEngineMode() === "mlx") {
    renderQuantOptions(["optQuant", "setQuant", "qsQuant"], MLX_QUANTS, MLX_DEFAULT_MODEL);
    return;
  }
  renderQuantOptions(["optQuant", "setQuant", "qsQuant"], quants, "Q8_0");
}

/** Alias kept for existing call sites (main.js, settings.js): a single
 *  re-fetch now does both jobs (relabeling AND cached-marking), since the
 *  `<select>` options are rebuilt from the backend rather than statically
 *  relabeled in place. */
export async function markCachedQuants() {
  await populateQuantSelects();
}

// EH-0013: re-render the quant tier labels and the model Load-button label on a
// locale switch. Both re-derive via tr() (reading the freshly-updated dict), so
// the quant <option> text and the Load/Download&load button flip language
// instantly. (The model status TEXT is refreshed on the next status event.)
if (typeof window !== "undefined" && window.unlocrI18n && window.unlocrI18n.onLocaleChange) {
  window.unlocrI18n.onLocaleChange(markCachedQuants);
  window.unlocrI18n.onLocaleChange(updateLoadLabel);
}

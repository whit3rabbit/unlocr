// Settings panel: load persisted settings into the form, persist on Save (and
// re-apply to the live workspace controls), and the Model cache section (path +
// GGUF size + Clear button). Backed by the get_settings / save_settings /
// get_cache_info / clear_model_cache commands.

import { requireTauri } from "./tauri.js";
import { formatEpoch } from "./paths.js";
import { applyPreset, markCachedQuants, ENGINE_PRESETS } from "./model.js";
import {
  renderEffectiveSummary,
  applyPageSelectionVisibility,
  numOr,
  floatOrNull,
  floatOrNullMin0,
  intMinNeg1,
  setVal,
} from "./options.js";

// EH-0013 bite 2: i18n hook. Named `tr` -- `t` is the Tauri handle in every wire*
// fn. Only the original wireSettings/wireCacheControls/wireDependencies strings are
// translated here; wireSystemRequirements + SYSREQ_INFO belong to the concurrent
// System Requirements feature and are left untouched.
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

/** Restore a custom-GGUF picker span (dataset.path + visible basename + tooltip)
 *  from a saved path. Mirrors model.js's private `setPath`, but does not dispatch
 *  the `unlocr:gguf-changed` event -- this is a restore, not a user pick/clear,
 *  and must not immediately re-trigger the auto-save it is itself seeding. */
function restoreGgufSpan(spanId, path) {
  const span = document.getElementById(spanId);
  if (!span || !path) return;
  span.dataset.path = path;
  span.textContent = path.split(/[/\\]/).pop();
  span.title = path;
}

// Serializes every patchSettings call (across all 3 writers -- Settings pane,
// Quick Settings, Workspace auto-save) onto one chain, so a refetch-then-write
// from one call can never interleave with another's: the refetch-before-write
// pattern above only prevents a lost update between SEQUENTIAL saves, and two
// callers racing (e.g. an in-flight auto-save overlapping a Quick Settings
// Save click) could otherwise both read the same base and one clobber the
// other's delta on write.
let saveChain = Promise.resolve();

/** Fetch the current settings row, merge in `buildOverrides(base)`, and persist
 *  the result. Always refetches `base` fresh (never a cached/stale snapshot) so
 *  every settings-writing surface (Settings pane, Quick Settings, Workspace
 *  auto-save) agrees on the current row and none can silently revert a field
 *  another surface just saved. Returns `{ok: true, settings}` or `{ok: false,
 *  error}`; the caller decides how to surface a failure. `errLabel` tags the
 *  console.error so the three call sites stay distinguishable in devtools. */
export function patchSettings(t, buildOverrides, errLabel) {
  const run = async () => {
    let base = {};
    try {
      base = (await t.core.invoke("get_settings")) || {};
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error(`[settings] ${errLabel} refetch failed`, err);
    }
    const newSettings = { ...base, ...buildOverrides(base) };
    try {
      await t.core.invoke("save_settings", { newSettings });
      return { ok: true, settings: newSettings };
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error(`[settings] ${errLabel} save failed`, err);
      return { ok: false, error: err };
    }
  };
  // Chain onto the shared queue regardless of whether the previous link
  // succeeded, so one failed save doesn't wedge every later save.
  const result = saveChain.then(run, run);
  saveChain = result.then(
    () => {},
    () => {}
  );
  return result;
}

// Workspace/Settings controls that mirror a Settings field 1:1 via a plain
// `.value` assignment (numeric/text fields are parsed back out at save time in
// wireAutoSaveEngineOptions' save(), not here). Single source of truth for both
// the restore-on-load loop below and AUTO_SAVE_CHANGE_IDS, so a new control only
// needs one entry instead of two independently-maintained lists.
const SYNCED_FIELDS = [
  ["optQuant", "defaultQuant"],
  ["optDpi", "defaultDpi"],
  ["optMaxTokens", "defaultMaxTokens"],
  ["optImageMaxTokens", "imageMaxTokens"],
  ["optChatTemplate", "chatTemplate"],
  ["optTemperature", "temperature"],
  ["optRepeatPenalty", "repeatPenalty"],
  ["optDryMultiplier", "dryMultiplier"],
  ["optDryBase", "dryBase"],
  ["optDryAllowedLength", "dryAllowedLength"],
  ["optDryPenaltyLastN", "dryPenaltyLastN"],
  ["optOutputMode", "outputMode"],
  ["optPagesMode", "pagesMode"],
  ["optPageFrom", "pageFrom"],
  ["optPageTo", "pageTo"],
  ["remoteUrl", "remoteBaseUrl"],
  ["remoteModel", "remoteModel"],
];

// The Quick Settings popup's 4 owned run-default fields: settings key, the
// popup's own dialog-local input id, and the two other surfaces that mirror
// the same value (Workspace `opt*` control, Settings-pane `set*` control).
// `numeric` picks numOr() vs a plain `.value` read; `blankFallback` (only set
// for defaultPrompt) overrides the default "fall back to the previous saved
// value" behavior for a field where blank is itself a meaningful value (an
// empty prompt box means "use the selected Task preset", not "keep the old
// override" -- see options.js's readRunOptions/promptOr). One list drives
// quick_settings.js's load, save-payload build, and post-save mirror sync,
// instead of three independently hand-written field blocks that could drift.
export const QUICK_SETTINGS_FIELDS = [
  { key: "defaultQuant", qsId: "qsQuant", optId: "optQuant", setId: "setQuant", numeric: false },
  { key: "defaultDpi", qsId: "qsDpi", optId: "optDpi", setId: "setDpi", numeric: true },
  { key: "defaultMaxTokens", qsId: "qsMaxTokens", optId: "optMaxTokens", setId: "setMaxTokens", numeric: true },
  { key: "defaultPrompt", qsId: "qsPrompt", optId: "optPrompt", setId: "setPrompt", numeric: false, blankFallback: "" },
];

/** Apply persisted settings to the live workspace controls (engine defaults,
 *  provider mode, remote fields) so a user's saved defaults seed each session. */
export function applySettingsToControls(s) {
  if (!s) return;
  // Settings-pane-only fields (not in SYNCED_FIELDS / AUTO_SAVE_CHANGE_IDS).
  setVal("optPrompt", s.defaultPrompt);
  setVal("remoteKey", s.remoteApiKey);
  SYNCED_FIELDS.forEach(([id, key]) => setVal(id, s[key]));
  const keepImagesEl = document.getElementById("optKeepImages");
  if (keepImagesEl && s.keepImages != null) keepImagesEl.checked = !!s.keepImages;
  // Anti-loop toggle: a checkbox, so restored outside the setVal (.value) loop.
  const antiLoopEl = document.getElementById("optAntiLoop");
  if (antiLoopEl && s.antiLoop != null) antiLoopEl.checked = !!s.antiLoop;
  // Pages: mode + bounds, then re-run the visibility toggle directly (not via
  // a dispatched `change` event -- that would also fire wireAutoSaveEngineOptions'
  // listener on #optPagesMode and spuriously re-trigger an auto-save) so the
  // from/to inputs show/hide correctly for a restored non-"all" mode.
  applyPageSelectionVisibility();
  // Custom GGUF paths: <span> dataset stores, not <input>.value.
  restoreGgufSpan("modelFilePath", s.modelFile);
  restoreGgufSpan("mmprojFilePath", s.mmprojFile);
  // Select the exact saved preset (engine_preset), not a lossy mode-based guess:
  // mode only distinguishes local/remote, but there are 5 presets (llamacpp/gpu/
  // vllm/sglang/custom), 3 of which are "remote". A pre-migration row has no
  // enginePreset value (or a stale/corrupt one); fall back to the old
  // mode-based guess rather than assign a value with no matching <option>
  // (which HTML resolves to an unselected dropdown).
  const sel = document.getElementById("enginePreset");
  if (sel) {
    const validPreset = s.enginePreset && Object.prototype.hasOwnProperty.call(ENGINE_PRESETS, s.enginePreset);
    sel.value = validPreset ? s.enginePreset : ((s.mode || "local") === "remote" ? "custom" : "llamacpp");
    applyPreset(sel.value);
  }
}

/** Wire the Settings panel: load persisted settings into the form on startup and
 *  persist them on Save (also re-applying to the workspace controls). */
export async function wireSettings(onSaved) {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const ids = {
    mode: "setMode",
    defaultQuant: "setQuant",
    remoteBaseUrl: "setRemoteUrl",
    remoteApiKey: "setRemoteKey",
    remoteModel: "setRemoteModel",
    llamaBin: "setLlamaBin",
    defaultDpi: "setDpi",
    defaultMaxTokens: "setMaxTokens",
    defaultPrompt: "setPrompt",
    idleUnloadMinutes: "setIdleMinutes",
  };
  const get = (id) => document.getElementById(id);

  let current = null;
  try {
    current = await t.core.invoke("get_settings");
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[settings] load failed", err);
  }
  if (current) {
    Object.keys(ids).forEach((k) => {
      const el = get(ids[k]);
      if (el != null && current[k] != null) el.value = current[k];
    });
    applySettingsToControls(current);
    if (typeof onSaved === "function") onSaved(current);
  }

  const saveBtn = document.getElementById("settingsSave");
  const saved = document.getElementById("settingsSaved");
  if (saveBtn) {
    saveBtn.addEventListener("click", async () => {
      // Like numOr() but allows 0 (idle-unload uses 0 to mean "never").
      const numOrZero = (id, fallback) => {
        const v = parseInt((get(id) && get(id).value) || "", 10);
        return Number.isFinite(v) && v >= 0 ? v : fallback;
      };
      // The Settings pane only edits this fixed field set (`ids` above); the
      // Advanced engine/run-option fields (persisted separately by
      // wireAutoSaveEngineOptions) live in the Workspace, not here. patchSettings
      // spreads a freshly-fetched base underneath this override set, so this
      // Save cannot blow those fields back to their struct defaults.
      const result = await patchSettings(
        t,
        () => ({
          mode: (get(ids.mode) && get(ids.mode).value) || "local",
          defaultQuant: (get(ids.defaultQuant) && get(ids.defaultQuant).value) || "Q8_0",
          remoteBaseUrl: (get(ids.remoteBaseUrl) && get(ids.remoteBaseUrl).value) || "",
          remoteApiKey: (get(ids.remoteApiKey) && get(ids.remoteApiKey).value) || "",
          remoteModel: (get(ids.remoteModel) && get(ids.remoteModel).value) || "",
          llamaBin: (get(ids.llamaBin) && get(ids.llamaBin).value) || "",
          defaultDpi: numOr(get(ids.defaultDpi), 144),
          defaultMaxTokens: numOr(get(ids.defaultMaxTokens), 4096),
          // Optional persistent override; empty = use the per-run Task preset.
          defaultPrompt: (get(ids.defaultPrompt) && get(ids.defaultPrompt).value) || "",
          idleUnloadMinutes: numOrZero(ids.idleUnloadMinutes, 15),
        }),
        "settings save"
      );
      if (result.ok) {
        applySettingsToControls(result.settings);
        renderEffectiveSummary();
        if (typeof onSaved === "function") onSaved(result.settings);
        if (saved) {
          saved.hidden = false;
          setTimeout(() => {
            saved.hidden = true;
          }, 1500);
        }
      } else if (saved) {
        saved.textContent = tr("settings.saveError", { error: String(result.error) });
        saved.hidden = false;
        setTimeout(() => {
          saved.hidden = true;
          saved.textContent = tr("settings.saved");
        }, 3000);
      }
    });
  }
}

// Workspace fields auto-saved on `change` by wireAutoSaveEngineOptions: every
// SYNCED_FIELDS id plus the two special-cased controls (checkbox + validated
// select) that applySettingsToControls restores outside the plain setVal loop.
// Deliberately excludes #remoteKey (see the function doc) -- everything else the
// Workspace's engine bar + Advanced panel exposes.
const AUTO_SAVE_CHANGE_IDS = [
  ...SYNCED_FIELDS.map(([id]) => id),
  "optKeepImages",
  "optAntiLoop",
  "enginePreset",
];

/** Auto-save the Workspace's engine/run-option knobs (quant, DPI, max tokens,
 *  Advanced panel, pages, output mode, engine preset, remote URL/model, custom
 *  GGUF paths) on every `change`, so closing the app without ever opening the
 *  Settings pane still keeps the last values used. The remote API key
 *  (#remoteKey) is deliberately excluded: it already has working, tested save
 *  semantics through the Settings pane's manual Save button (masked-value +
 *  OS-keyring logic in settings.rs); auto-saving a live-typing password field
 *  risks writing an intermediate/masked value into the keyring by mistake.
 *
 *  Each `save()` call re-fetches the current row via `patchSettings` rather than
 *  trusting a cached baseline: an earlier version cached the row once at
 *  wire-time and only ever refreshed it from its OWN saves, so a save from the
 *  Settings pane or Quick Settings went unnoticed and a later Workspace edit
 *  would silently revert it. Fields this function does not own (remoteApiKey,
 *  llamaBin, defaultPrompt, idleUnloadMinutes -- all Settings-pane-only) ride
 *  through unchanged via that fresh base.
 *
 *  Call once, after wireSettings() has finished restoring the controls, so
 *  those initial value assignments don't spuriously fire these listeners as if
 *  the user had just edited every field. Fail-soft outside the webview. */
export function wireAutoSaveEngineOptions() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }

  const get = (id) => document.getElementById(id);
  const pickedGguf = (spanId) => {
    const el = get(spanId);
    const p = el && el.dataset ? el.dataset.path : "";
    return p && p.trim() ? p : "";
  };

  async function save() {
    await patchSettings(
      t,
      (base) => {
        const presetEl = get("enginePreset");
        const preset = (presetEl && presetEl.value) || "llamacpp";
        const mode = (ENGINE_PRESETS[preset] || ENGINE_PRESETS.llamacpp).mode;
        return {
          mode,
          enginePreset: preset,
          defaultQuant: (get("optQuant") && get("optQuant").value) || base.defaultQuant,
          defaultDpi: numOr(get("optDpi"), base.defaultDpi),
          defaultMaxTokens: numOr(get("optMaxTokens"), base.defaultMaxTokens),
          remoteBaseUrl: (get("remoteUrl") && get("remoteUrl").value) || base.remoteBaseUrl,
          remoteModel: (get("remoteModel") && get("remoteModel").value) || base.remoteModel,
          imageMaxTokens: numOr(get("optImageMaxTokens"), null),
          chatTemplate: (get("optChatTemplate") && get("optChatTemplate").value) || "",
          temperature: floatOrNullMin0(get("optTemperature")),
          repeatPenalty: floatOrNull(get("optRepeatPenalty")),
          dryMultiplier: floatOrNullMin0(get("optDryMultiplier")),
          dryBase: floatOrNull(get("optDryBase")),
          // Persist the RAW override fields (null when blank), not the anti-loop
          // fallback: readRunOptions re-derives 2/-1 from the toggle at run time.
          dryAllowedLength: numOr(get("optDryAllowedLength"), null),
          dryPenaltyLastN: intMinNeg1(get("optDryPenaltyLastN"), null),
          antiLoop: !!(get("optAntiLoop") && get("optAntiLoop").checked),
          keepImages: !!(get("optKeepImages") && get("optKeepImages").checked),
          outputMode: (get("optOutputMode") && get("optOutputMode").value) || "single",
          pagesMode: (get("optPagesMode") && get("optPagesMode").value) || "all",
          pageFrom: numOr(get("optPageFrom"), null),
          pageTo: numOr(get("optPageTo"), null),
          modelFile: pickedGguf("modelFilePath"),
          mmprojFile: pickedGguf("mmprojFilePath"),
        };
      },
      "auto-save"
    );
  }

  AUTO_SAVE_CHANGE_IDS.forEach((id) => {
    const el = get(id);
    if (el) el.addEventListener("change", save);
  });
  // Custom-GGUF pickers aren't native <input>s (dialog.open() + span dataset
  // write); model.js's setPath dispatches this DOM CustomEvent on pick/clear
  // instead, so the trigger fires once per user action, same as a real change.
  ["modelFilePath", "mmprojFilePath"].forEach((id) => {
    const el = get(id);
    if (el) el.addEventListener("unlocr:gguf-changed", save);
  });
}

/** Format bytes to a human-readable string (MiB for GB-scale GGUFs). Shared by
 *  wireCacheControls (aggregate size) and wireModelFilesTable (per-file size)
 *  so the two surfaces can't drift on formatting. */
function fmtBytes(n) {
  if (n < 0) {
    // eslint-disable-next-line no-console
    console.error("[settings] fmtBytes: negative size:", n);
  }
  if (n <= 0) return "0 B";
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KiB";
  if (n < 1024 * 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + " MiB";
  return (n / (1024 * 1024 * 1024)).toFixed(2) + " GiB";
}

/** Wire the Settings panel's Model cache section: load the cache path + GGUF
 *  size via get_cache_info, and wire the Clear button to clear_model_cache.
 *  Called once on startup (the Settings view exists in the DOM at load time).
 *  Fail-soft outside the webview. */
export async function wireCacheControls() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const dirEl = document.getElementById("cacheDir");
  const sizeEl = document.getElementById("cacheSizeBytes");
  const clearBtn = document.getElementById("clearCacheBtn");
  const statusEl = document.getElementById("clearCacheStatus");

  /** Refresh the path + size display from the backend. */
  async function refreshCacheInfo() {
    try {
      const info = await t.core.invoke("get_cache_info");
      if (dirEl) dirEl.textContent = (info && info.path) || "-";
      if (sizeEl) sizeEl.textContent = info ? fmtBytes(Number(info.sizeBytes) || 0) : "";
    } catch (err) {
      if (dirEl) dirEl.textContent = tr("settings.unavailable", { error: String(err) });
      if (sizeEl) sizeEl.textContent = "";
    }
  }

  await refreshCacheInfo();

  if (clearBtn) {
    clearBtn.addEventListener("click", async () => {
      clearBtn.disabled = true;
      if (statusEl) { statusEl.hidden = false; statusEl.textContent = tr("settings.clearing"); }
      try {
        await t.core.invoke("clear_model_cache");
        if (statusEl) statusEl.textContent = tr("settings.cacheCleared");
      } catch (err) {
        if (statusEl) statusEl.textContent = tr("settings.cacheError", { error: String(err) });
      } finally {
        await refreshCacheInfo();
        // Re-mark cached quants (all gone after a clear).
        markCachedQuants();
        clearBtn.disabled = false;
        if (statusEl) {
          setTimeout(() => { if (statusEl) statusEl.hidden = true; }, 3000);
        }
      }
    });
  }
}

/** Wire the Settings panel's per-file model cache table: list every cached
 *  GGUF (name/size/sha256/mtime) via list_cached_files, with a per-row delete
 *  button calling remove_cached_file. Mirrors wireCacheControls' shape (fetch
 *  -> render -> wire -> refresh). Also re-lists after the aggregate "Clear
 *  cached models" button fires, since it wipes the same files this table
 *  shows. Called once on startup; fail-soft outside the webview. */
export async function wireModelFilesTable() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const tbody = document.getElementById("modelFilesTbody");
  const emptyEl = document.getElementById("modelFilesEmpty");
  if (!tbody) return;

  async function refresh() {
    let files = [];
    try {
      files = await t.core.invoke("list_cached_files");
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error("[settings] list_cached_files failed", err);
    }
    tbody.innerHTML = "";
    if (emptyEl) emptyEl.hidden = files.length > 0;
    files.forEach((f) => {
      const tr_ = document.createElement("tr");
      const shortSha = f.sha256.length > 12 ? f.sha256.slice(0, 12) + "…" : f.sha256;
      const nameCell = document.createElement("td");
      nameCell.textContent = f.name;
      nameCell.title = f.name;
      const sizeCell = document.createElement("td");
      sizeCell.textContent = fmtBytes(Number(f.sizeBytes) || 0);
      const shaCell = document.createElement("td");
      shaCell.className = "mono";
      shaCell.textContent = shortSha;
      shaCell.title = f.sha256;
      const modifiedCell = document.createElement("td");
      modifiedCell.textContent = f.modified != null ? formatEpoch(f.modified) : "-";
      const actionCell = document.createElement("td");
      const delBtn = document.createElement("button");
      delBtn.className = "model-btn model-btn--ghost";
      delBtn.type = "button";
      delBtn.textContent = tr("settings.deleteFile");
      delBtn.dataset.name = f.name;
      actionCell.appendChild(delBtn);
      tr_.append(nameCell, sizeCell, shaCell, modifiedCell, actionCell);
      tbody.appendChild(tr_);
    });
    tbody.querySelectorAll("button[data-name]").forEach((btn) => {
      btn.addEventListener("click", async () => {
        btn.disabled = true;
        try {
          await t.core.invoke("remove_cached_file", { filename: btn.dataset.name });
        } catch (err) {
          // eslint-disable-next-line no-console
          console.error("[settings] remove_cached_file failed", err);
        } finally {
          await refresh();
          // Re-sync the quant dropdown's cached markers after a per-file delete.
          markCachedQuants();
        }
      });
    });
  }

  await refresh();

  // The aggregate "Clear cached models" button wipes the same files this
  // table shows; re-list after it fires (own listener, independent of
  // wireCacheControls' own click handler on the same button).
  const clearBtn = document.getElementById("clearCacheBtn");
  if (clearBtn) clearBtn.addEventListener("click", () => setTimeout(refresh, 0));
}

// Human-facing labels + recommendation hints for system metrics, keyed by
// the metric key from the backend.
const SYSREQ_INFO = {
  ram_total: { label: "RAM" },
  cpu_cores: { label: "CPU cores" },
  cpu_model: { label: "CPU model" },
  gpu:       { label: "GPU" },
  disk_free: { label: "Free disk space" },
};

/** Wire the Settings "System Requirements" panel: detect hardware specs and
 *  rate each against known thresholds. Called once on startup and on Recheck.
 *  Fail-soft outside the webview. Renders the same dep-row layout as Dependencies. */
export async function wireSystemRequirements() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const list = document.getElementById("sysreqList");
  const empty = document.getElementById("sysreqEmpty");
  const refresh = document.getElementById("sysreqRefresh");
  const verdict = document.getElementById("sysreqVerdict");
  if (!list) return;

  async function render() {
    let report;
    try {
      report = await t.core.invoke("system_requirements");
    } catch (err) {
      if (empty) { empty.hidden = false; empty.textContent = tr("settings.unavailable", { error: String(err) }); }
      return;
    }
    list.querySelectorAll(".dep-row").forEach((n) => n.remove());
    if (empty) empty.hidden = true;

    for (const metric of report.metrics || []) {
      // Label is localized; fall back to the English SYSREQ_INFO label, then the
      // raw key, if a locale is missing the entry.
      const labelKey = "sysreq.label." + metric.key;
      let label = tr(labelKey);
      if (label === labelKey) label = (SYSREQ_INFO[metric.key] || {}).label || metric.key;
      const row = document.createElement("div");
      row.className = "dep-row";

      const name = document.createElement("span");
      name.className = "dep-row__name";
      name.textContent = label;

      const status = document.createElement("span");
      status.className = "dep-row__status";
      if (metric.status === "good") status.classList.add("is-ok");
      else if (metric.status === "marginal") status.classList.add("is-warn");
      else if (metric.status === "insufficient") status.classList.add("is-bad");
      status.textContent = metric.value || "Unknown";

      row.appendChild(name);
      row.appendChild(status);

      if (metric.hint) {
        const hint = document.createElement("span");
        hint.className = "dep-row__hint";
        hint.textContent = metric.hint;
        row.appendChild(hint);
      }

      list.appendChild(row);
    }

    // Set the overall verdict label and color on the section header.
    if (verdict) {
      verdict.className = "panel__label sysreq__verdict";
      if (report.verdict === "good") verdict.classList.add("is-good");
      else if (report.verdict === "marginal") verdict.classList.add("is-marginal");
      else if (report.verdict === "insufficient") verdict.classList.add("is-insufficient");
      else verdict.classList.add("is-unknown");
      // Localized verdict; fall back to the backend's English label.
      const verdictKey = "sysreq.verdict." + report.verdict;
      let verdictText = tr(verdictKey);
      if (verdictText === verdictKey) verdictText = report.verdictLabel || "System Requirements";
      verdict.textContent = verdictText;
    }
  }

  if (refresh) refresh.addEventListener("click", render);
  // Re-render on a locale change (and on the initial locale load) so the metric
  // labels and verdict retranslate; they come from tr(), not data-i18n nodes.
  // The static hardware probes are cached in the backend, so this is cheap.
  if (window.unlocrI18n && window.unlocrI18n.onLocaleChange) {
    window.unlocrI18n.onLocaleChange(() => render());
  }
  await render();
}

// Human-facing labels + per-OS package-manager hints for the external tools, keyed by
// the backend tool name (list_tools / locate). The hint shown is for the detected host
// OS (host_os command); used only when a tool is missing and cannot be auto-downloaded
// (i.e. not Windows; on Windows a Download button is shown instead).
const TOOL_INFO = {
  pdftoppm: {
    label: "pdftoppm (poppler)",
    brew: "poppler",
    hints: { macos: "brew install poppler", linux: "apt install poppler-utils  ·  dnf install poppler-utils" },
  },
  "llama-server": {
    label: "llama-server (unlocr patched build)",
    // No `brew`: Homebrew's llama.cpp is stock and lacks the Unlimited-OCR R-SWA
    // patch (PR #24975), so unlocr downloads its own patched build instead. The
    // hints below only show on exotic arches with no pinned download.
    hints: {
      macos: "unlocr downloads a patched build (stock llama.cpp lacks R-SWA, PR #24975)",
      linux: "unlocr downloads a patched build (stock llama.cpp lacks R-SWA, PR #24975)",
    },
  },
  pandoc: {
    label: "pandoc (export)",
    brew: "pandoc",
    hints: { macos: "brew install pandoc", linux: "apt install pandoc  ·  dnf install pandoc" },
  },
};

// Official Homebrew install command, shown (copyable, not auto-run) to macOS users who
// have no package manager and need tools that aren't directly downloadable.
const HOMEBREW_INSTALL =
  '/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"';

/** Wire the Settings "Dependencies" panel: list each external tool's status and, on
 *  Windows, offer a Download button per missing tool (download_tool fetches a pinned,
 *  sha256-verified build into the cache). Elsewhere, a missing tool shows its package-
 *  manager hint. Called once on startup. Fail-soft outside the webview. */
export async function wireDependencies() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const list = document.getElementById("depsList");
  const empty = document.getElementById("depsEmpty");
  const hint = document.getElementById("depsHint");
  const refresh = document.getElementById("depsRefresh");
  if (!list) return;

  // Detect the host OS + Homebrew presence once. OS makes hints OS-correct (the GUI
  // ships per platform); brew presence decides whether macOS shows an "Install with
  // Homebrew" button vs. manual guidance. Fail-soft.
  let os = "unknown";
  let brew = false;
  try {
    os = await t.core.invoke("host_os");
    brew = await t.core.invoke("brew_available");
  } catch (err) {
    // leave defaults; UI degrades to hints
  }

  // One app-lifetime listener routes tool://download progress to the matching row by
  // tool name (set as data-tool on the row's status span).
  if (t.event && t.event.listen) {
    t.event.listen("tool://download", (e) => {
      const p = e.payload || {};
      const cell = list.querySelector('[data-tool-status="' + p.name + '"]');
      if (cell) cell.textContent = tr("deps.downloadingPct", { pct: (p.pct || 0) });
    });
  }

  async function render() {
    let tools;
    try {
      tools = await t.core.invoke("list_tools");
    } catch (err) {
      if (empty) { empty.hidden = false; empty.textContent = tr("settings.unavailable", { error: String(err) }); }
      return;
    }
    // Clear previous rows (keep the #depsEmpty node).
    list.querySelectorAll(".dep-row").forEach((n) => n.remove());
    if (empty) empty.hidden = true;

    let anyManual = false;
    for (const tool of tools || []) {
      const info = TOOL_INFO[tool.name] || { label: tool.name, hints: {} };
      const row = document.createElement("div");
      row.className = "dep-row";

      const name = document.createElement("span");
      name.className = "dep-row__name";
      name.textContent = info.label;

      const status = document.createElement("span");
      status.className = "dep-row__status" + (tool.found ? " is-ok" : " is-bad");
      status.dataset.toolStatus = tool.name;
      status.textContent = tool.found ? tr("deps.found") : tr("deps.notFound");
      if (tool.found && tool.path) status.title = tool.path;

      // Show the resolved llama-server location as the override field's PLACEHOLDER
      // (never its value): empty keeps auto-resolve (managed cached -> download ->
      // PATH), so a downloaded managed build is still reported Managed. Setting the
      // value would force an External override and wrongly trip the loop warning.
      // This is the "default from download": after Download the row rebuilds found,
      // and the placeholder updates to where the managed build landed.
      if (tool.name === "llama-server" && tool.found && tool.path) {
        const bin = document.getElementById("setLlamaBin");
        if (bin) bin.placeholder = tool.path;
      }

      row.appendChild(name);
      row.appendChild(status);

      if (!tool.found && tool.downloadable) {
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "model-btn model-btn--ghost dep-row__get";
        btn.textContent = tr("deps.download");
        btn.addEventListener("click", async () => {
          btn.disabled = true;
          status.textContent = tr("deps.downloading");
          try {
            await t.core.invoke("download_tool", { name: tool.name });
            await render(); // re-check; the row rebuilds as "found"
          } catch (err) {
            status.textContent = tr("deps.failed");
            status.title = String(err);
            btn.disabled = false;
          }
        });
        row.appendChild(btn);
      } else if (!tool.found && os === "macos" && brew && info.brew) {
        // No direct download on macOS (poppler/llama), but brew is present: offer a
        // one-click `brew install <formula>`.
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "model-btn model-btn--ghost dep-row__get";
        btn.textContent = tr("deps.installBrew");
        btn.addEventListener("click", async () => {
          btn.disabled = true;
          status.textContent = tr("deps.installing");
          try {
            await t.core.invoke("brew_install", { formula: info.brew });
            await render();
          } catch (err) {
            status.textContent = tr("deps.failed");
            status.title = String(err);
            btn.disabled = false;
          }
        });
        row.appendChild(btn);
      } else if (!tool.found) {
        anyManual = true;
        const h = document.createElement("span");
        h.className = "dep-row__hint";
        // macOS without brew: nudge toward installing Homebrew (the only practical way
        // to get poppler). Otherwise the OS-specific package-manager hint.
        h.textContent =
          os === "macos" && !brew
            ? tr("deps.needsBrew", { formula: info.brew || tool.name })
            : (info.hints && info.hints[os]) || tr("deps.installViaPm");
        row.appendChild(h);
      }
      list.appendChild(row);
    }
    if (hint) {
      hint.hidden = !anyManual;
      if (anyManual) {
        // On macOS without brew, show the official (copyable) Homebrew install command;
        // elsewhere a generic recheck nudge.
        hint.textContent =
          os === "macos" && !brew
            ? tr("deps.installHomebrewHint", { cmd: HOMEBREW_INSTALL })
            : tr("deps.installMissingHint");
      } else {
        hint.textContent = "";
      }
    }
  }

  if (refresh) refresh.addEventListener("click", render);
  await render();
}

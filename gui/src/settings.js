// Settings panel: load persisted settings into the form, persist on Save (and
// re-apply to the live workspace controls), and the Model cache section (path +
// GGUF size + Clear button). Backed by the get_settings / save_settings /
// get_cache_info / clear_model_cache commands.

import { requireTauri } from "./tauri.js";
import { applyPreset, markCachedQuants } from "./model.js";
import { renderEffectiveSummary } from "./options.js";

/** Apply persisted settings to the live workspace controls (engine defaults,
 *  provider mode, remote fields) so a user's saved defaults seed each session. */
export function applySettingsToControls(s) {
  if (!s) return;
  const setVal = (id, v) => {
    const el = document.getElementById(id);
    if (el != null && v != null) el.value = v;
  };
  setVal("optQuant", s.defaultQuant);
  setVal("optDpi", s.defaultDpi);
  setVal("optMaxTokens", s.defaultMaxTokens);
  setVal("optPrompt", s.defaultPrompt);
  setVal("remoteUrl", s.remoteBaseUrl);
  setVal("remoteKey", s.remoteApiKey);
  setVal("remoteModel", s.remoteModel);
  // Select the backend preset matching the saved mode. We can't tell which remote
  // backend was saved (settings only stores mode + URL), so remote -> Custom, which
  // shows the fields and preserves the #remoteUrl restored by setVal above.
  const sel = document.getElementById("enginePreset");
  if (sel) {
    sel.value = (s.mode || "local") === "remote" ? "custom" : "llamacpp";
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
      const num = (id, fallback) => {
        const v = parseInt((get(id) && get(id).value) || "", 10);
        return Number.isFinite(v) && v > 0 ? v : fallback;
      };
      // Like num() but allows 0 (idle-unload uses 0 to mean "never").
      const numOrZero = (id, fallback) => {
        const v = parseInt((get(id) && get(id).value) || "", 10);
        return Number.isFinite(v) && v >= 0 ? v : fallback;
      };
      const newSettings = {
        mode: (get(ids.mode) && get(ids.mode).value) || "local",
        defaultQuant: (get(ids.defaultQuant) && get(ids.defaultQuant).value) || "Q8_0",
        remoteBaseUrl: (get(ids.remoteBaseUrl) && get(ids.remoteBaseUrl).value) || "",
        remoteApiKey: (get(ids.remoteApiKey) && get(ids.remoteApiKey).value) || "",
        remoteModel: (get(ids.remoteModel) && get(ids.remoteModel).value) || "",
        llamaBin: (get(ids.llamaBin) && get(ids.llamaBin).value) || "",
        defaultDpi: num(ids.defaultDpi, 144),
        defaultMaxTokens: num(ids.defaultMaxTokens, 4096),
        defaultPrompt:
          (get(ids.defaultPrompt) && get(ids.defaultPrompt).value) ||
          "<|grounding|>Convert the document to markdown.",
        idleUnloadMinutes: numOrZero(ids.idleUnloadMinutes, 15),
      };
      try {
        await t.core.invoke("save_settings", { newSettings });
        applySettingsToControls(newSettings);
        renderEffectiveSummary();
        if (typeof onSaved === "function") onSaved(newSettings);
        if (saved) {
          saved.hidden = false;
          setTimeout(() => {
            saved.hidden = true;
          }, 1500);
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[settings] save failed", err);
        if (saved) {
          saved.textContent = "Error saving settings: " + String(err);
          saved.hidden = false;
          setTimeout(() => {
            saved.hidden = true;
            saved.textContent = "Saved.";
          }, 3000);
        }
      }
    });
  }
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

  /** Format bytes to a human-readable string (MiB for GB-scale GGUFs). */
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

  /** Refresh the path + size display from the backend. */
  async function refreshCacheInfo() {
    try {
      const info = await t.core.invoke("get_cache_info");
      if (dirEl) dirEl.textContent = (info && info.path) || "—";
      if (sizeEl) sizeEl.textContent = info ? fmtBytes(Number(info.sizeBytes) || 0) : "";
    } catch (err) {
      if (dirEl) dirEl.textContent = "unavailable: " + String(err);
      if (sizeEl) sizeEl.textContent = "";
    }
  }

  await refreshCacheInfo();

  if (clearBtn) {
    clearBtn.addEventListener("click", async () => {
      clearBtn.disabled = true;
      if (statusEl) { statusEl.hidden = false; statusEl.textContent = "clearing…"; }
      try {
        await t.core.invoke("clear_model_cache");
        if (statusEl) statusEl.textContent = "Cache cleared.";
      } catch (err) {
        if (statusEl) statusEl.textContent = "Error: " + String(err);
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

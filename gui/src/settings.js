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
        // Optional persistent override; empty = use the per-run Task preset.
        defaultPrompt: (get(ids.defaultPrompt) && get(ids.defaultPrompt).value) || "",
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
      if (dirEl) dirEl.textContent = (info && info.path) || "-";
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
    label: "llama-server (llama.cpp)",
    brew: "llama.cpp",
    hints: { macos: "brew install llama.cpp", linux: "build llama.cpp >= b8530 (no distro package)" },
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
      if (cell) cell.textContent = "downloading… " + (p.pct || 0) + "%";
    });
  }

  async function render() {
    let tools;
    try {
      tools = await t.core.invoke("list_tools");
    } catch (err) {
      if (empty) { empty.hidden = false; empty.textContent = "unavailable: " + String(err); }
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
      status.textContent = tool.found ? "found" : "not found";
      if (tool.found && tool.path) status.title = tool.path;

      row.appendChild(name);
      row.appendChild(status);

      if (!tool.found && tool.downloadable) {
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "model-btn model-btn--ghost dep-row__get";
        btn.textContent = "Download";
        btn.addEventListener("click", async () => {
          btn.disabled = true;
          status.textContent = "downloading…";
          try {
            await t.core.invoke("download_tool", { name: tool.name });
            await render(); // re-check; the row rebuilds as "found"
          } catch (err) {
            status.textContent = "failed";
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
        btn.textContent = "Install with Homebrew";
        btn.addEventListener("click", async () => {
          btn.disabled = true;
          status.textContent = "installing…";
          try {
            await t.core.invoke("brew_install", { formula: info.brew });
            await render();
          } catch (err) {
            status.textContent = "failed";
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
            ? "needs Homebrew; see below, then `brew install " + (info.brew || tool.name) + "`"
            : (info.hints && info.hints[os]) || "install via your package manager";
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
            ? "Install Homebrew (https://brew.sh), then Recheck:\n" + HOMEBREW_INSTALL
            : "Install the missing tools with your package manager, then click Recheck.";
      } else {
        hint.textContent = "";
      }
    }
  }

  if (refresh) refresh.addEventListener("click", render);
  await render();
}

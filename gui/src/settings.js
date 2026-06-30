// Settings panel: load persisted settings into the form, persist on Save (and
// re-apply to the live workspace controls), and the Model cache section (path +
// GGUF size + Clear button). Backed by the get_settings / save_settings /
// get_cache_info / clear_model_cache commands.

import { requireTauri } from "./tauri.js";
import { applyPreset, markCachedQuants } from "./model.js";
import { renderEffectiveSummary } from "./options.js";

// EH-0013 bite 2: i18n hook. Named `tr` -- `t` is the Tauri handle in every wire*
// fn. Only the original wireSettings/wireCacheControls/wireDependencies strings are
// translated here; wireSystemRequirements + SYSREQ_INFO belong to the concurrent
// System Requirements feature and are left untouched.
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

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
          saved.textContent = tr("settings.saveError", { error: String(err) });
          saved.hidden = false;
          setTimeout(() => {
            saved.hidden = true;
            saved.textContent = tr("settings.saved");
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

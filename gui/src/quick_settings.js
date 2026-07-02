// Quick Settings popup: opened from the titlebar gear (#settingsGearBtn). A small
// subset of the full Settings view -- display size (UI zoom), language, and the 4
// run defaults (quant/DPI/max tokens/prompt) -- for a fast tweak without leaving
// the current tab. The full Settings view (nav rail) is untouched and still holds
// every advanced field.

import { requireTauri } from "./tauri.js";
import { patchSettings } from "./settings.js";
import { renderEffectiveSummary, numOr, setVal } from "./options.js";

const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

// Display size is a frontend-only preference (like the locale picker,
// assets/i18n.js) -- NOT added to the Rust Settings struct/DB, since that would
// need a schema migration for a value the backend never reads.
const ZOOM_KEY = "unlocr.uiZoom";
const ZOOM_FACTORS = { small: 0.9, medium: 1, large: 1.15, xlarge: 1.3 };

/** Read the persisted zoom size, defaulting to "medium". Fail-soft: storage can
 *  be unavailable in private mode / a sandbox. */
function readZoomSize() {
  try {
    const v = localStorage.getItem(ZOOM_KEY);
    return v && ZOOM_FACTORS[v] ? v : "medium";
  } catch (err) {
    return "medium";
  }
}

/** Apply a zoom size to the whole document (covers the app shell, toasts, and
 *  dialogs, which sit as siblings of .app, not just .app itself) and persist it. */
function applyZoomSize(size) {
  const factor = ZOOM_FACTORS[size] || 1;
  document.documentElement.style.zoom = String(factor);
  try {
    localStorage.setItem(ZOOM_KEY, size);
  } catch (err) {
    /* ignore: storage unavailable */
  }
}

// Apply the saved zoom immediately on load (this module is imported eagerly from
// main.js), so the preference survives a restart without waiting for the popup.
applyZoomSize(readZoomSize());

/** Wire the gear button + popup: open/populate, live zoom + language switching,
 *  and Save for the 4 run defaults. Fail-soft outside the webview. */
export function wireQuickSettingsPopup() {
  const gearBtn = document.getElementById("settingsGearBtn");
  const dlg = document.getElementById("quickSettingsDialog");
  const zoomSel = document.getElementById("qsZoom");
  const localeSel = document.getElementById("qsLocale");
  const quantSel = document.getElementById("qsQuant");
  const dpiInput = document.getElementById("qsDpi");
  const maxTokensInput = document.getElementById("qsMaxTokens");
  const promptInput = document.getElementById("qsPrompt");
  const saveBtn = document.getElementById("qsSave");
  const savedEl = document.getElementById("qsSaved");
  if (!gearBtn || !dlg) return;

  if (zoomSel) {
    zoomSel.addEventListener("change", () => applyZoomSize(zoomSel.value));
  }

  if (localeSel && window.unlocrI18n) {
    localeSel.addEventListener("change", () => {
      window.unlocrI18n.setLocale(localeSel.value);
    });
    // Keep in sync if the locale changes elsewhere (e.g. the full Settings
    // page's own #localeSelect).
    if (window.unlocrI18n.onLocaleChange) {
      window.unlocrI18n.onLocaleChange(() => {
        localeSel.value = window.unlocrI18n.getLocale();
      });
    }
  }

  gearBtn.addEventListener("click", async () => {
    if (zoomSel) zoomSel.value = readZoomSize();
    if (localeSel && window.unlocrI18n) localeSel.value = window.unlocrI18n.getLocale();
    try {
      const t = requireTauri();
      const s = await t.core.invoke("get_settings");
      if (s) {
        if (quantSel) quantSel.value = s.defaultQuant;
        if (dpiInput) dpiInput.value = s.defaultDpi;
        if (maxTokensInput) maxTokensInput.value = s.defaultMaxTokens;
        if (promptInput) promptInput.value = s.defaultPrompt || "";
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error("[quick-settings] load failed", err);
    }
    dlg.showModal();
  });

  if (saveBtn) {
    saveBtn.addEventListener("click", async () => {
      let t;
      try {
        t = requireTauri();
      } catch (err) {
        return;
      }
      // patchSettings re-fetches the current row as the base (save_settings
      // takes the whole Settings object; omitted fields would otherwise
      // round-trip through serde's #[serde(default)] and reset), same shared
      // helper wireSettings' own Save button uses.
      const result = await patchSettings(
        t,
        (base) => ({
          defaultQuant: (quantSel && quantSel.value) || base.defaultQuant,
          defaultDpi: numOr(dpiInput, base.defaultDpi),
          defaultMaxTokens: numOr(maxTokensInput, base.defaultMaxTokens),
          defaultPrompt: (promptInput && promptInput.value) || "",
        }),
        "quick-settings save"
      );
      if (result.ok) {
        const { settings: newSettings } = result;
        renderEffectiveSummary();
        // Sync only the 4 fields this popup owns, in both the Workspace's own
        // `opt*` mirrors and the full Settings page's `set*` fields (either may
        // be open/stale). Deliberately does NOT call applySettingsToControls:
        // that restores every Workspace field from the saved row, which would
        // clobber any OTHER field mid-edit and unrelated to this save.
        setVal("optQuant", newSettings.defaultQuant);
        setVal("optDpi", newSettings.defaultDpi);
        setVal("optMaxTokens", newSettings.defaultMaxTokens);
        setVal("optPrompt", newSettings.defaultPrompt);
        setVal("setQuant", newSettings.defaultQuant);
        setVal("setDpi", newSettings.defaultDpi);
        setVal("setMaxTokens", newSettings.defaultMaxTokens);
        setVal("setPrompt", newSettings.defaultPrompt);
        if (savedEl) {
          savedEl.hidden = false;
          setTimeout(() => {
            savedEl.hidden = true;
          }, 1500);
        }
      } else if (savedEl) {
        savedEl.textContent = tr("settings.saveError", { error: String(result.error) });
        savedEl.hidden = false;
        setTimeout(() => {
          savedEl.hidden = true;
          savedEl.textContent = tr("settings.saved");
        }, 3000);
      }
    });
  }
}

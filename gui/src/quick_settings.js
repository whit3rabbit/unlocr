// Quick Settings popup: opened from the titlebar gear (#settingsGearBtn). A small
// subset of the full Settings view -- display size (UI zoom), language, and the 4
// run defaults (quant/DPI/max tokens/prompt) -- for a fast tweak without leaving
// the current tab. The full Settings view (nav rail) is untouched and still holds
// every advanced field.

import { requireTauri } from "./tauri.js";
import { patchSettings, QUICK_SETTINGS_FIELDS } from "./settings.js";
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

// Window size is a separate frontend-only preference from zoom: zoom scales the
// content within the OS window (CSS `zoom`), this resizes the OS window itself via
// the Tauri window API (`core:window:allow-set-size`, granted in capabilities/default.json).
const WINDOW_KEY = "unlocr.windowSize";
const WINDOW_SIZES = {
  compact: [1200, 800],
  default: [1440, 900],
  large: [1680, 1050],
  xlarge: [1920, 1200],
};

/** Read the persisted window-size preset, defaulting to "default". */
function readWindowSize() {
  try {
    const v = localStorage.getItem(WINDOW_KEY);
    return v && WINDOW_SIZES[v] ? v : "default";
  } catch (err) {
    return "default";
  }
}

/** Resize the OS window to a preset and persist the choice. Fail-soft: the Tauri
 *  window API is only present inside the webview (withGlobalTauri), never in a
 *  plain browser preview, and the resize itself is best-effort. */
async function applyWindowSize(size) {
  try {
    localStorage.setItem(WINDOW_KEY, size);
  } catch (err) {
    /* ignore: storage unavailable */
  }
  const dims = WINDOW_SIZES[size] || WINDOW_SIZES.default;
  try {
    const tauriWindow = window.__TAURI__ && window.__TAURI__.window;
    if (!tauriWindow) return;
    await tauriWindow.getCurrentWindow().setSize(new tauriWindow.LogicalSize(dims[0], dims[1]));
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[quick-settings] window resize failed", err);
  }
}

// Apply the saved window size immediately on load, same as the zoom preference.
applyWindowSize(readWindowSize());

/** Wire the gear button + popup: open/populate, live zoom + language switching,
 *  and Save for the 4 run defaults. Fail-soft outside the webview. */
export function wireQuickSettingsPopup() {
  const gearBtn = document.getElementById("settingsGearBtn");
  const dlg = document.getElementById("quickSettingsDialog");
  const zoomSel = document.getElementById("qsZoom");
  const windowSizeSel = document.getElementById("qsWindowSize");
  const localeSel = document.getElementById("qsLocale");
  const saveBtn = document.getElementById("qsSave");
  const savedEl = document.getElementById("qsSaved");
  if (!gearBtn || !dlg) return;

  if (zoomSel) {
    zoomSel.addEventListener("change", () => applyZoomSize(zoomSel.value));
  }

  if (windowSizeSel) {
    windowSizeSel.addEventListener("change", () => applyWindowSize(windowSizeSel.value));
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
    if (windowSizeSel) windowSizeSel.value = readWindowSize();
    if (localeSel && window.unlocrI18n) localeSel.value = window.unlocrI18n.getLocale();
    try {
      const t = requireTauri();
      const s = await t.core.invoke("get_settings");
      if (s) {
        QUICK_SETTINGS_FIELDS.forEach((f) => setVal(f.qsId, s[f.key] ?? f.blankFallback));
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
        (base) => {
          const overrides = {};
          QUICK_SETTINGS_FIELDS.forEach((f) => {
            const el = document.getElementById(f.qsId);
            overrides[f.key] = f.numeric
              ? numOr(el, base[f.key])
              : (el && el.value) || (Object.prototype.hasOwnProperty.call(f, "blankFallback") ? f.blankFallback : base[f.key]);
          });
          return overrides;
        },
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
        QUICK_SETTINGS_FIELDS.forEach((f) => {
          setVal(f.optId, newSettings[f.key]);
          setVal(f.setId, newSettings[f.key]);
        });
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

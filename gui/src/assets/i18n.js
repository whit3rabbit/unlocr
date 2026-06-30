/* unlocr i18n runtime (EH-0014). No bundler: a classic <script> loaded in
 * index.html <head> before main.js (same pattern as EasyMDE / DOMPurify), so the
 * globals below exist before any ES module runs.
 *
 * Loads locales/<lang>.json (served as static assets under frontendDist ../src),
 * walks [data-i18n] / [data-i18n-ph] / [data-i18n-aria] to translate the visible
 * UI, and exposes t(key, params) for dynamic strings. setLocale(lang) re-applies
 * every node + fires change callbacks so a live language switch retranslates
 * without a reload.
 *
 * Progressive enhancement: HTML keeps its English default text, so before the
 * locale fetch resolves (or if it fails) the app still reads in English; for the
 * default locale the swap is a no-op, so there is no flash of untranslated text.
 * t() falls back to the key itself when a string is missing, so a missing entry
 * is visible (not blank) during the extraction work in EH-0013.
 *
 * CSP: default-src 'self' covers the self fetch; no 'unsafe-inline' needed. */
(function () {
  "use strict";

  // Locale files that ship under locales/. Add a tag here when its JSON lands
  // (zh/ja/ko arrive in EH-0019/0020/0021); everything else falls back to en.
  var AVAILABLE = ["en", "zh", "ja", "ko"];
  var DEFAULT = "en";

  // Locale is a frontend-only preference, so it is persisted in localStorage
  // (synchronous read on boot avoids a flash of the wrong language; the Rust
  // settings store would need a schema migration for a value the backend never
  // uses). Fail-soft: storage can be unavailable in private mode / a sandbox.
  var STORAGE_KEY = "unlocr.locale";

  var dict = {}; // current locale's flat key -> string map
  var current = DEFAULT;
  var listeners = []; // re-render callbacks fired on locale change
  var ready = null; // Promise resolved when the initial locale is applied

  /** Read a previously chosen locale tag, or null if none / unavailable. */
  function readSavedLocale() {
    try {
      var v = localStorage.getItem(STORAGE_KEY);
      return v && AVAILABLE.indexOf(v) !== -1 ? v : null;
    } catch (e) {
      return null;
    }
  }

  /** Persist the resolved locale tag so the choice survives a restart. */
  function saveLocale(tag) {
    try {
      localStorage.setItem(STORAGE_KEY, tag);
    } catch (e) {
      /* ignore: storage unavailable */
    }
  }

  /** Map a BCP-47 tag (e.g. navigator.language) to a locale file we ship.
   *  Exact tag, then primary subtag (zh-CN -> zh), then en. */
  function resolve(tag) {
    if (!tag) return DEFAULT;
    var lower = String(tag).toLowerCase();
    if (AVAILABLE.indexOf(lower) !== -1) return lower;
    var primary = lower.split("-")[0];
    if (AVAILABLE.indexOf(primary) !== -1) return primary;
    return DEFAULT;
  }

  /** {placeholder} substitution. Unknown placeholders are left intact. */
  function fmt(str, params) {
    if (!str || !params) return str;
    return str.replace(/\{(\w+)\}/g, function (_, k) {
      return Object.prototype.hasOwnProperty.call(params, k)
        ? String(params[k])
        : "{" + k + "}";
    });
  }

  /** Translate a key with optional params. Missing key -> the key itself. */
  function t(key, params) {
    var str = Object.prototype.hasOwnProperty.call(dict, key)
      ? dict[key]
      : key;
    return fmt(str, params);
  }

  /** Translate every data-i18n* node under root (default: document). */
  function applyText(root) {
    var scope = root || document;
    scope.querySelectorAll("[data-i18n]").forEach(function (node) {
      var key = node.getAttribute("data-i18n");
      if (key) node.textContent = t(key);
    });
    scope.querySelectorAll("[data-i18n-ph]").forEach(function (node) {
      var key = node.getAttribute("data-i18n-ph");
      if (key) node.setAttribute("placeholder", t(key));
    });
    scope.querySelectorAll("[data-i18n-aria]").forEach(function (node) {
      var key = node.getAttribute("data-i18n-aria");
      if (key) node.setAttribute("aria-label", t(key));
    });
  }

  /** Fetch a locale JSON, falling back to the default locale on any failure
   *  (missing file, parse error) so the UI always has strings. */
  function loadLocale(tag) {
    return fetch("locales/" + tag + ".json", { cache: "no-cache" })
      .then(function (r) {
        if (!r.ok) throw new Error("locale " + tag + ": " + r.status);
        return r.json();
      })
      .catch(function () {
        if (tag === DEFAULT) return {};
        return loadLocale(DEFAULT);
      });
  }

  /** Load + apply a locale, set <html lang>, and notify dynamic renderers. */
  function useLocale(tag) {
    return loadLocale(tag).then(function (data) {
      dict = data || {};
      current = tag;
      saveLocale(tag);
      document.documentElement.lang = tag;
      applyText(document);
      listeners.forEach(function (fn) {
        try {
          fn(t);
        } catch (e) {
          /* a renderer's error must not break the rest */
        }
      });
      return tag;
    });
  }

  function init(lang) {
    ready = useLocale(resolve(lang));
    return ready;
  }

  /** Switch locale live and retranslate everything (no reload). */
  function setLocale(lang) {
    return useLocale(resolve(lang));
  }

  function getLocale() {
    return current;
  }

  /** Register a callback fired with t() after every locale load/switch, so
   *  dynamic string holders (e.g. toasts, status lines) can re-render. */
  function onLocaleChange(fn) {
    if (typeof fn === "function") listeners.push(fn);
  }

  window.unlocrI18n = {
    t: t,
    apply: applyText,
    init: init,
    setLocale: setLocale,
    getLocale: getLocale,
    onLocaleChange: onLocaleChange,
    resolve: resolve,
    get ready() {
      return ready;
    },
  };
  // Short global so inline/template-literal calls can read t('key'); modules
  // that need it should grab `const t = window.unlocrI18n.t` (EH-0013).
  window.t = t;

  // Self-init on DOMContentLoaded (avoids coupling to main.js): the data-i18n
  // nodes exist by then, and the fetch resolves async after. boot() also wires the
  // Settings locale picker (#localeSelect) once the initial locale resolves, so a
  // user can switch language at runtime -- setLocale re-applies every data-i18n
  // node and fires the onLocaleChange callbacks (e.g. the notify panel re-renders).
  function boot(lang) {
    // A saved choice wins over the browser locale so a user's pick survives a
    // restart; falling back to `lang` (navigator.language) on first run.
    init(readSavedLocale() || lang).then(function (tag) {
      var sel = document.getElementById("localeSelect");
      if (!sel || sel.dataset.wired) return;
      sel.dataset.wired = "1";
      sel.value = tag;
      sel.addEventListener("change", function () {
        setLocale(sel.value);
      });
    });
  }
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", function () {
      boot(navigator.language);
    });
  } else {
    boot(navigator.language);
  }
})();

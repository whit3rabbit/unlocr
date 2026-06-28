// Global Tauri bridge access (withGlobalTauri: true, no bundler, see CLAUDE.md).
// We read `window.__TAURI__` instead of importing @tauri-apps/api. Guard every
// access so a stale non-Tauri context (e.g. opening index.html in a plain
// browser) fails softly instead of throwing on load.

export const Tauri = () => window.__TAURI__;

/** Throw a friendly error if the global Tauri bridge is missing. */
export function requireTauri() {
  const t = Tauri();
  if (!t || !t.core || !t.core.invoke) {
    throw new Error("Tauri bridge unavailable; open this page inside the app, not a browser.");
  }
  return t;
}

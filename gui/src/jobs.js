// Job store views entrypoint re-exporting logically split components.
import { requireTauri } from "./tauri.js";

export { openInReview, renderJobCard } from "./job_card.js";
export { makeLibrary } from "./library.js";
export { makeBoard } from "./board.js";
export { wireRail } from "./rail.js";

/** Fetch all jobs from the store and hand them to `render`. Failures log + render
 *  empty rather than throw so a first launch (no store) never breaks a view.
 *  `tag` only labels the console line ("library"/"board"). Shared by the Library
 *  grid and the Board so both load identically (one place to change the contract).
 *  Used inside the views' load() callbacks (runtime), so the jobs.js<->view import
 *  cycle is harmless. */
export async function loadJobs(tag, render) {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    // Outside the webview: nothing to load. Leave the view's empty/placeholder state.
    // eslint-disable-next-line no-console
    console.warn("[" + tag + "] skipped:", err.message);
    return;
  }
  try {
    render(await t.core.invoke("list_jobs"));
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[" + tag + "] list_jobs failed", err);
    render([]);
  }
}

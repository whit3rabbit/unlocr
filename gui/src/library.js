import { requireTauri } from "./tauri.js";
import { jobBaseName } from "./paths.js";
import { renderJobCard, confirmDestructive } from "./job_card.js";
import { loadJobs } from "./jobs.js";

/** EH-0006 bite 2: controller over the Library grid. Reads the persisted job
 *  store via the `list_jobs` command and renders one card per run. The grid is the
 *  "all jobs" view (the Board view, bite 3, groups the same jobs by status). Cards
 *  are read-only in this bite; drag-drop import (bite 4) adds the enqueue path.
 *
 *  `load()` is called once on app start and again whenever the Library rail button
 *  is activated, so a run completed in the Workspace shows up after switching tabs
 *  without a manual reload. A store read failure is surfaced as an error card
 *  instead of throwing, so a first-launch (no store yet) stays usable.
 *
 *  EH-0015: `mdPane` and `railButtons` are optional. When provided, done job cards
 *  with an outputPath become clickable and re-open the .md in the Review pane.
 *  They are wired in DOMContentLoaded after makeMarkdownPane() and the rail buttons
 *  are resolved, then injected via library.setReviewHooks(). */
export function makeLibrary() {
  const grid = document.getElementById("libraryGrid");
  const count = document.getElementById("libraryCount");
  const empty = document.getElementById("libraryEmpty");
  const refresh = document.getElementById("libraryRefresh");
  const removeAllBtn = document.getElementById("libraryRemoveAll");
  const removeAllDeleteBtn = document.getElementById("libraryRemoveAllDelete");
  // Set by setReviewHooks() once mdPane + rail buttons are available.
  let _mdPane = null;
  let _railButtons = null;

  /** Replace the grid with cards for the given jobs (newest-first by createdAt so
   *  the most recent run is top-left). Empty -> placeholder shown. Cards are built
   *  by the shared module-level renderJobCard, so the Library and Board render the
   *  same card markup. Done cards are clickable when _mdPane is wired. */
  function render(jobs) {
    if (!grid) return;
    grid.querySelectorAll(".job-card").forEach((n) => n.remove());
    const list = (jobs || []).slice();
    list.sort((a, b) => (Number(b.createdAt) || 0) - (Number(a.createdAt) || 0));
    if (count) count.textContent = String(list.length);
    if (list.length === 0) {
      if (empty) empty.hidden = false;
      return;
    }
    if (empty) empty.hidden = true;
    for (const job of list) {
      grid.appendChild(renderJobCard(job, _mdPane, _railButtons, actions));
    }
  }

  /** Per-card and bulk delete actions, wired to the delete_job / clear_jobs
   *  commands. Record-only removals act immediately; file-deleting variants
   *  confirm first (irreversible). Each refreshes the grid via load() after. */
  const actions = {
    async remove(id) {
      if (!id) return;
      try {
        const t = requireTauri();
        await t.core.invoke("delete_job", { id, deleteFile: false });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[library] delete_job failed", err);
      }
      load();
    },
    async removeDelete(id, outputPath) {
      if (!id) return;
      const name = jobBaseName(outputPath) || "this file";
      if (!(await confirmDestructive("Delete " + name + " from disk? This cannot be undone.")))
        return;
      try {
        const t = requireTauri();
        await t.core.invoke("delete_job", { id, deleteFile: true });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[library] delete_job (with file) failed", err);
      }
      load();
    },
    async removeAll() {
      try {
        const t = requireTauri();
        await t.core.invoke("clear_jobs", { deleteFiles: false });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[library] clear_jobs failed", err);
      }
      load();
    },
    async removeAllDelete() {
      if (!(await confirmDestructive("Delete every OCR output file from disk? This cannot be undone.")))
        return;
      try {
        const t = requireTauri();
        await t.core.invoke("clear_jobs", { deleteFiles: true });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[library] clear_jobs (with files) failed", err);
      }
      load();
    },
  };

  /** Fetch jobs from the store and render. Shared loader (jobs.js) logs + renders
   *  empty rather than throw so a first launch (no store) never breaks the view. */
  async function load() {
    await loadJobs("library", render);
  }

  if (refresh) {
    refresh.addEventListener("click", load);
  }
  if (removeAllBtn) {
    removeAllBtn.addEventListener("click", () => actions.removeAll());
  }
  if (removeAllDeleteBtn) {
    removeAllDeleteBtn.addEventListener("click", () => actions.removeAllDelete());
  }

  /** EH-0015: inject the review-pane controller and rail buttons so done job
   *  cards become clickable. Call once in DOMContentLoaded after both are live.
   *  Re-renders the grid immediately so existing cards pick up the affordance. */
  function setReviewHooks(mdPane, railButtons) {
    _mdPane = mdPane;
    _railButtons = railButtons;
    // Re-render: cards already in the grid were built without the hooks.
    load();
  }

  return { load, render, setReviewHooks, actions };
}

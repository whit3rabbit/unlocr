import { requireTauri } from "./tauri.js";
import { jobBaseName } from "./paths.js";
import { renderJobCard, confirmDestructive } from "./job_card.js";
import { loadJobs } from "./jobs.js";

// EH-0013 bite 2: i18n hook. Named `tr` -- `t` is the Tauri handle in the actions.
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

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
 *  are resolved, then injected via library.setReviewHooks().
 *
 *  Multi-select: a Set of selected job ids survives across renders. Each card
 *  renders a checkbox (job_card.js) wired to the `isSelected`/`toggleSelect`
 *  actions; the toolbar's select-all checkbox + count + batch Remove buttons act
 *  on the set. render() reconciles the set against the freshly rendered ids (so a
 *  deleted/reloaded-out job is dropped from the selection) and re-derives the
 *  select-all + toolbar state, so a live `jobs://changed` never leaves stale UI. */
export function makeLibrary() {
  const grid = document.getElementById("libraryGrid");
  const count = document.getElementById("libraryCount");
  const empty = document.getElementById("libraryEmpty");
  const refresh = document.getElementById("libraryRefresh");
  const selectAll = document.getElementById("librarySelectAll");
  const selectedCount = document.getElementById("librarySelectedCount");
  const removeSelectedBtn = document.getElementById("libraryRemoveSelected");
  const removeSelectedDeleteBtn = document.getElementById("libraryRemoveSelectedDelete");
  // Set by setReviewHooks() once mdPane + rail buttons are available.
  let _mdPane = null;
  let _railButtons = null;

  // Multi-select state: the checked job ids. Persists across renders; reconciled
  // in render() so ids that disappear (deleted, filtered) are dropped.
  const selected = new Set();
  // The ids + jobs last rendered, so the select-all toggle and locale-switch
  // re-render can rebuild without a backend round-trip.
  let lastIds = [];
  let lastJobs = [];

  /** Set the select-all checkbox to match the selection vs the visible set:
   *  checked iff every visible id is selected, indeterminate iff some-but-not-all.
   *  No-op when the element is absent (e.g. a view-less test harness). */
  function syncSelectAll() {
    if (!selectAll) return;
    const total = lastIds.length;
    const picked = lastIds.reduce((n, id) => n + (selected.has(id) ? 1 : 0), 0);
    selectAll.checked = total > 0 && picked === total;
    selectAll.indeterminate = picked > 0 && picked < total;
  }

  /** Reflect the current selection in the count + the two batch buttons. Called
   *  after every render and every toggle. Buttons are disabled at 0 selected. */
  function updateToolbar() {
    const n = selected.size;
    if (selectedCount) {
      selectedCount.textContent = n > 0 ? tr("job.selected", { n }) : "";
      selectedCount.hidden = n === 0;
    }
    if (removeSelectedBtn) removeSelectedBtn.disabled = n === 0;
    if (removeSelectedDeleteBtn) removeSelectedDeleteBtn.disabled = n === 0;
  }

  /** Replace the grid with cards for the given jobs (newest-first by createdAt so
   *  the most recent run is top-left). Empty -> placeholder shown. Cards are built
   *  by the shared module-level renderJobCard, so the Library and Board render the
   *  same card markup. Done cards are clickable when _mdPane is wired. */
  function render(jobs) {
    if (!grid) return;
    grid.querySelectorAll(".job-card").forEach((n) => n.remove());
    const list = (jobs || []).slice();
    list.sort((a, b) => (Number(b.createdAt) || 0) - (Number(a.createdAt) || 0));
    lastJobs = list;
    lastIds = list.map((j) => j && j.id).filter(Boolean);
    // Reconcile selection: drop ids no longer present (deleted/reloaded out) so
    // a stale id never drives a batch action against a job that is gone.
    for (const id of [...selected]) {
      if (!lastIds.includes(id)) selected.delete(id);
    }
    if (count) count.textContent = String(list.length);
    if (list.length === 0) {
      if (empty) empty.hidden = false;
      syncSelectAll();
      updateToolbar();
      return;
    }
    if (empty) empty.hidden = true;
    for (const job of list) {
      grid.appendChild(renderJobCard(job, _mdPane, _railButtons, actions));
    }
    syncSelectAll();
    updateToolbar();
  }

  /** Per-card and batch delete actions, wired to the delete_job / delete_jobs /
   *  clear_jobs commands. Record-only removals act immediately; file-deleting
   *  variants confirm first (irreversible). Each refreshes the grid via load()
   *  after, which also reconciles the selection. */
  const actions = {
    isSelected(id) {
      return !!id && selected.has(id);
    },
    toggleSelect(id, checked) {
      if (!id) return;
      if (checked) selected.add(id);
      else selected.delete(id);
      syncSelectAll();
      updateToolbar();
    },
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
      const name = jobBaseName(outputPath) || tr("job.thisFile");
      if (!(await confirmDestructive(tr("job.confirmDeleteOne", { name }))))
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
    async removeSelected() {
      if (selected.size === 0) return;
      const ids = [...selected];
      try {
        const t = requireTauri();
        await t.core.invoke("delete_jobs", { ids, deleteFile: false });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[library] delete_jobs failed", err);
      }
      load();
    },
    async removeSelectedDelete() {
      if (selected.size === 0) return;
      const n = selected.size;
      if (!(await confirmDestructive(tr("job.confirmDeleteSelected", { n }))))
        return;
      const ids = [...selected];
      try {
        const t = requireTauri();
        await t.core.invoke("delete_jobs", { ids, deleteFile: true });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[library] delete_jobs (with files) failed", err);
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
  // Select-all toggles every visible id in/out of the selection, then re-renders
  // so each card's checkbox reflects the bulk change (the per-card checkbox state
  // is derived from the `selected` set at card-build time).
  if (selectAll) {
    selectAll.addEventListener("change", () => {
      if (selectAll.checked) {
        for (const id of lastIds) selected.add(id);
      } else {
        selected.clear();
      }
      render(lastJobs);
    });
  }
  if (removeSelectedBtn) {
    removeSelectedBtn.addEventListener("click", () => actions.removeSelected());
  }
  if (removeSelectedDeleteBtn) {
    removeSelectedDeleteBtn.addEventListener("click", () => actions.removeSelectedDelete());
  }

  // The "{n} selected" count is dynamic (set via textContent, not a data-i18n
  // node), so it does not auto-translate on a live locale switch. Re-derive it
  // (and the button-disabled state) when the locale changes.
  if (window.unlocrI18n && typeof window.unlocrI18n.onLocaleChange === "function") {
    window.unlocrI18n.onLocaleChange(updateToolbar);
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

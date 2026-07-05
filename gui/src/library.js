import { requireTauri } from "./tauri.js";
import { jobBaseName } from "./paths.js";
import { renderJobCard, confirmDestructive } from "./job_card.js";
import { loadJobs, clearJobsByStatus } from "./jobs.js";

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
  const clearFailedBtn = document.getElementById("libraryClearFailed");
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

  /** Build one grid cell for a PDF's run history: the latest run as the face
   *  card, plus a toggle that reveals the older runs (collapsed by default). A PDF
   *  run once is a lone card with no toggle. Every run (face + collapsed) is a
   *  full renderJobCard, so re-open/info/remove work per-run.
   *
   *  `runs` is this PDF's runs, newest first. */
  function renderGroup(runs) {
    // ponytail: the group cell is an <li> holding the face card <li> + a nested
    // <ul> of older run cards. Reuses renderJobCard as-is (returns an <li>).
    const cell = document.createElement("li");
    cell.className = "job-group";
    cell.appendChild(renderJobCard(runs[0], _mdPane, _railButtons, actions));
    if (runs.length > 1) {
      const sub = document.createElement("ul");
      sub.className = "job-group__runs";
      sub.hidden = true;
      for (const r of runs.slice(1)) {
        sub.appendChild(renderJobCard(r, _mdPane, _railButtons, actions));
      }
      const toggle = document.createElement("button");
      toggle.type = "button";
      toggle.className = "job-group__toggle";
      const relabel = () => {
        toggle.textContent = sub.hidden
          ? tr("library.showRuns", { n: runs.length })
          : tr("library.hideRuns");
        toggle.setAttribute("aria-expanded", String(!sub.hidden));
      };
      relabel();
      toggle.addEventListener("click", () => {
        sub.hidden = !sub.hidden;
        relabel();
      });
      cell.appendChild(toggle);
      cell.appendChild(sub);
    }
    return cell;
  }

  /** Replace the grid with one cell per PDF (newest-first by createdAt so the most
   *  recent run is top-left), each grouping that PDF's re-runs. Empty -> placeholder
   *  shown. Cards are built by the shared renderJobCard, so a run looks identical to
   *  the Board. Done cards are clickable when _mdPane is wired. */
  function render(jobs) {
    if (!grid) return;
    grid.querySelectorAll(".job-group, .job-card").forEach((n) => n.remove());
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
    // Group runs by input PDF, preserving newest-first order (the first run seen
    // for a path is its latest, since `list` is sorted desc).
    const groups = new Map();
    for (const job of list) {
      const key = (job && job.inputPath) || "";
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(job);
    }
    for (const runs of groups.values()) {
      grid.appendChild(renderGroup(runs));
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

  /** Remove every failed job from the library/store (record-only -- matches
   *  "Remove selected", never deletes files). Shares its confirm/delete/reload
   *  shape with board.js's clearDone() via jobs.js's clearJobsByStatus. */
  async function clearFailed() {
    await clearJobsByStatus(
      lastJobs,
      (status) => status === "failed",
      (n) => tr("library.confirmClearFailed", { n }),
      "library",
      load
    );
  }

  if (refresh) {
    refresh.addEventListener("click", load);
  }
  if (clearFailedBtn) {
    clearFailedBtn.addEventListener("click", clearFailed);
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

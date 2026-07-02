import { loadJobs } from "./jobs.js";
import { renderJobCard, confirmDestructive } from "./job_card.js";
import { requireTauri } from "./tauri.js";

// i18n hook, same pattern as library.js. Named `tr` -- `t` is the Tauri handle
// used by the clearDone Tauri invoke below.
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

/** EH-0006 bite 3: controller over the Board kanban. Reads the same persisted job
 *  store the Library grid uses (list_jobs) and groups jobs into status columns:
 *  queued, running, done, failed. Each column lists its jobs newest-first (by
 *  createdAt). The card markup is the shared renderJobCard, so a job looks identical
 *  in the grid and on the board.
 *
 *  `load()` is called on app start and on Board tab switch (wireRail), so a run
 *  finished in the Workspace shows up on the board without a manual Reload. A store
 *  read failure clears the board rather than throwing, so a first launch (no store)
 *  stays usable. Unknown statuses are bucketed into "queued" so no job is dropped. */
export function makeBoard() {
  // Column DOM by status key: { list node, count node }. Order is the column order
  // shown on screen. "failed" is included alongside the bite's queued/running/done so
  // a failed run is never silently dropped from the board.
  const columns = {
    queued: {
      list: document.getElementById("boardColQueuedList"),
      count: document.getElementById("boardColQueued"),
    },
    running: {
      list: document.getElementById("boardColRunningList"),
      count: document.getElementById("boardColRunning"),
    },
    done: {
      list: document.getElementById("boardColDoneList"),
      count: document.getElementById("boardColDone"),
    },
    failed: {
      list: document.getElementById("boardColFailedList"),
      count: document.getElementById("boardColFailed"),
    },
  };
  const total = document.getElementById("boardCount");
  const empty = document.getElementById("boardEmpty");
  const refresh = document.getElementById("boardRefresh");
  const clearDoneBtn = document.getElementById("boardClearDoneBtn");

  // Bulk mode: the in-memory pending queue (files imported but not yet run) is
  // rendered into the Queued column so the Workflow board shows the whole batch,
  // not just rows the store created at run start. Bound from main.js via bindQueue.
  // getPending() -> string[] of queued paths; removePending(path) drops one.
  let getPending = null;
  let removePending = null;
  // Last store jobs rendered, kept so renderPending() can repaint without a refetch
  // when the pending queue changes (add/remove from the rail or a board card).
  let lastJobs = [];

  /** Normalize a job status to a known column key. Unknown / missing -> queued so
   *  every record lands in some column. */
  function columnKey(status) {
    const s = (status || "").toLowerCase();
    return columns[s] ? s : "queued";
  }

  /** Clear every column list and reset its count. */
  function clearColumns() {
    for (const key of Object.keys(columns)) {
      const col = columns[key];
      if (col.list) col.list.querySelectorAll(".job-card").forEach((n) => n.remove());
      if (col.count) col.count.textContent = "0";
    }
  }

  /** Group jobs into status buckets (newest-first by createdAt within a bucket). */
  function bucketize(jobs) {
    const buckets = { queued: [], running: [], done: [], failed: [] };
    const sorted = (jobs || []).slice().sort(
      (a, b) => (Number(b.createdAt) || 0) - (Number(a.createdAt) || 0),
    );
    for (const job of sorted) {
      buckets[columnKey(job && job.status)].push(job);
    }
    return buckets;
  }

  /** Live engine-option values for the pending-card meta footer (display only;
   *  the real run reads the same controls at click time). Best-effort: missing
   *  controls just omit that field. */
  function currentFormOpts() {
    const val = (id) => {
      const el = document.getElementById(id);
      return el ? el.value : undefined;
    };
    return {
      quant: val("optQuant"),
      dpi: val("optDpi"),
      maxTokens: val("optMaxTokens"),
      keepImages: !!(document.getElementById("optKeepImages") || {}).checked,
    };
  }

  /** Build the queued-column cards for the in-memory pending queue (files imported
   *  but not yet run). Reuses renderJobCard with a synthetic queued job; the card's
   *  "Remove" action drops the path from the queue (board <-> rail stay in sync). */
  function pendingCards() {
    const paths = typeof getPending === "function" ? getPending() : [];
    const opts = currentFormOpts();
    return paths.map((path) =>
      renderJobCard(
        { id: path, inputPath: path, status: "queued", options: opts },
        null,
        null,
        { remove: () => removePending && removePending(path) },
      ),
    );
  }

  /** Render jobs grouped by status into the columns, prepending the in-memory
   *  pending queue to the Queued column. Empty (no pending + no store jobs) ->
   *  show the centered placeholder; otherwise hide it. */
  function render(jobs) {
    lastJobs = (jobs || []).slice();
    clearColumns();
    const buckets = bucketize(jobs);
    const pending = pendingCards();
    let n = 0;
    for (const key of Object.keys(columns)) {
      const col = columns[key];
      const items = buckets[key];
      // Pending (in-memory) queue leads the Queued column, then store-queued rows.
      const extra = key === "queued" ? pending.length : 0;
      if (col.count) col.count.textContent = String(items.length + extra);
      if (col.list) {
        if (key === "queued") for (const card of pending) col.list.appendChild(card);
        for (const job of items) col.list.appendChild(renderJobCard(job));
      }
      n += items.length + extra;
    }
    if (total) total.textContent = String(n);
    if (empty) empty.hidden = n !== 0;
  }

  /** Repaint from the last loaded store jobs + current pending queue, without a
   *  store refetch. Called when the pending queue changes (queue.onChange). */
  function renderPending() {
    render(lastJobs);
  }

  /** Bind the in-memory pending queue (main.js owns it). getPending returns the
   *  queued paths; removePending drops one. Safe to call once after boot. */
  function bindQueue(getter, remover) {
    getPending = getter;
    removePending = remover;
    renderPending();
  }

  /** Fetch jobs from the store and render. Shared loader (jobs.js) logs + clears
   *  the board rather than throw so a first launch (no store) never breaks it. */
  async function load() {
    await loadJobs("board", render);
  }

  /** Remove every Done-status job from the board/store (record-only -- the .md
   *  output files are left on disk). Confirms first even though it is
   *  record-only: a bulk removal from the board is not something a misclick
   *  should silently do, even if the underlying files survive. */
  async function clearDone() {
    const doneIds = lastJobs
      .filter((j) => columnKey(j && j.status) === "done")
      .map((j) => j && j.id)
      .filter(Boolean);
    if (doneIds.length === 0) return;
    if (!(await confirmDestructive(tr("board.confirmClearDone", { n: doneIds.length }))))
      return;
    try {
      const t = requireTauri();
      await t.core.invoke("delete_jobs", { ids: doneIds, deleteFile: false });
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error("[board] delete_jobs (clear done) failed", err);
    }
    load();
  }

  if (refresh) {
    refresh.addEventListener("click", load);
  }
  if (clearDoneBtn) {
    clearDoneBtn.addEventListener("click", clearDone);
  }

  return { load, render, renderPending, bindQueue };
}

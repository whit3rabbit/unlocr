// Job store views: the shared job card, the Library grid (all jobs), the Board
// kanban (jobs grouped by status), the re-open-in-review affordance, the
// record-outcome writer, and the rail (icon nav) view switching that reloads
// these views on tab change. All read the persisted store via the list_jobs /
// record_job commands.

import { requireTauri } from "./tauri.js";
import { jobBaseName, formatEpoch } from "./paths.js";

/** EH-0015: navigate to the Review view and render the .md for a done job.
 *  Called when the user clicks a job card whose status is "done" and has an
 *  outputPath. Switches the rail to "review", loads the markdown from disk via
 *  read_text_file, and renders it in the review pane. Fail-soft: errors are
 *  logged but never crash the Library view.
 *
 *  `outputPath` is the absolute path to the written {stem}.md.
 *  `mdPane`     the makeMarkdownPane() controller.
 *  `buttons`    the rail button NodeList (to update is-active + titlebar). */
export async function openInReview(outputPath, mdPane, buttons) {
  if (!outputPath) return;
  // Switch to the review view — mirror what wireRail does on a click.
  const screenTitle = document.getElementById("screenTitle");
  if (buttons) {
    buttons.forEach((b) => b.classList.remove("is-active"));
    const reviewBtn = Array.from(buttons).find((b) => b.dataset.route === "review");
    if (reviewBtn) reviewBtn.classList.add("is-active");
  }
  document.querySelectorAll(".view").forEach((v) => {
    v.classList.toggle("is-shown", v.dataset.view === "review");
  });
  if (screenTitle) screenTitle.textContent = "Review";

  // Fetch the .md from disk via the backend command.
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  try {
    // allowed_dir: the parent directory of the .md file so read_text_file's
    // allowlist check passes (matches the outDir the run used).
    const lastSep = Math.max(outputPath.lastIndexOf("/"), outputPath.lastIndexOf("\\"));
    const allowedDir = lastSep > 0 ? outputPath.slice(0, lastSep) : ".";
    const markdown = await t.core.invoke("read_text_file", {
      path: outputPath,
      allowedDir,
    });
    if (mdPane) mdPane.render(markdown, outputPath);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[library] re-open failed:", err);
    if (mdPane) mdPane.render("could not read " + outputPath + ": " + String(err), outputPath);
  }
}

/** EH-0006: build a single read-only job card element from a Job record. Status
 *  drives the stripe + badge color via the .job-card--<status> class. Done shows the
 *  output path; failed shows the error. Options + timestamps are the meta footer.
 *  Shared by the Library grid (all jobs) and the Board columns (jobs grouped by
 *  status) so a job looks identical in both — one source of truth for the markup.
 *
 *  EH-0015: `mdPane` and `railButtons` are optional. When provided, cards for done
 *  jobs with an outputPath are clickable: clicking opens the .md in the review pane
 *  (via openInReview). The pointer cursor and a "Open in review" aria-label signal
 *  the affordance. Board column cards do not receive these so they stay inert. */
export function renderJobCard(job, mdPane, railButtons) {
  const card = document.createElement("div");
  const status = (job && job.status) || "queued";
  card.className = "job-card job-card--" + status;

  const head = document.createElement("div");
  head.className = "job-card__head";
  const name = document.createElement("span");
  name.className = "job-card__name";
  name.textContent = jobBaseName(job && job.inputPath);
  name.title = (job && job.inputPath) || "";
  const badge = document.createElement("span");
  badge.className = "job-card__status";
  badge.textContent = status;
  head.appendChild(name);
  head.appendChild(badge);
  card.appendChild(head);

  // Detail line: output path for done, error for failed, nothing otherwise.
  const detail = document.createElement("p");
  detail.className = "job-card__detail";
  if (status === "failed" && job && job.error) {
    detail.classList.add("job-card__error");
    detail.textContent = String(job.error);
  } else if (job && job.outputPath) {
    detail.textContent = String(job.outputPath);
  } else {
    detail.textContent = "—";
  }
  card.appendChild(detail);

  // Meta footer: options echo + timestamps. Each field is a span so they wrap.
  const meta = document.createElement("div");
  meta.className = "job-card__meta";
  const opts = (job && job.options) || {};
  const push = (label, val) => {
    if (val === undefined || val === null || val === "") return;
    const span = document.createElement("span");
    span.textContent = label + " " + val;
    meta.appendChild(span);
  };
  push("quant:", opts.quant);
  push("dpi:", opts.dpi);
  push("max-tok:", opts.maxTokens);
  push("keep-img:", opts.keepImages ? "on" : "off");
  if (job && job.updatedAt) {
    push("updated:", formatEpoch(job.updatedAt));
  } else if (job && job.createdAt) {
    push("queued:", formatEpoch(job.createdAt));
  }
  card.appendChild(meta);

  // EH-0015: wire the "re-open in review" affordance for done jobs that have an
  // on-disk output. Only the Library grid passes mdPane + railButtons; Board cards
  // are intentionally inert (the Board is a status board, not a content browser).
  const outputPath = job && job.outputPath;
  if (mdPane && railButtons && status === "done" && outputPath) {
    card.classList.add("job-card--openable");
    card.title = "Click to open the markdown in the Review pane";
    card.style.cursor = "pointer";
    card.addEventListener("click", () => {
      openInReview(outputPath, mdPane, railButtons);
    });
  }

  return card;
}

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
      grid.appendChild(renderJobCard(job, _mdPane, _railButtons));
    }
  }

  /** Fetch jobs from the store and render. Failures log + render empty rather than
   *  throw so a first launch (no store) never breaks the Library view. */
  async function load() {
    let t;
    try {
      t = requireTauri();
    } catch (err) {
      // Outside the webview: nothing to load. Leave the placeholder.
      // eslint-disable-next-line no-console
      console.warn("[library] skipped:", err.message);
      return;
    }
    try {
      const jobs = await t.core.invoke("list_jobs");
      render(jobs);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error("[library] list_jobs failed", err);
      render([]);
    }
  }

  if (refresh) {
    refresh.addEventListener("click", load);
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

  return { load, render, setReviewHooks };
}

/** EH-0006: record a run's outcome to the job store after run_ocr completes or
 *  fails. Keeps OCR and persistence decoupled: a store write failure is logged but
 *  never rolls back the OCR result the user already received. Returns the stored
 *  Job (or null) so the caller can optionally refresh the Library with it. */
export async function recordRunOutcome(inputPath, opts, status, outputPath, error, library, board) {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return null;
  }
  try {
    const job = await t.core.invoke("record_job", {
      inputPath,
      // Only forward the fields record_job accepts; the rest default server-side.
      quant: opts && opts.quant,
      maxTokens: opts && opts.maxTokens,
      dpi: opts && opts.dpi,
      prompt: opts && opts.prompt,
      keepImages: opts && opts.keepImages,
      status,
      outputPath: outputPath || "",
      error: error || "",
    });
    // Refresh the Library and the Board so the new card appears without a tab switch.
    if (library && typeof library.load === "function") {
      library.load();
    }
    if (board && typeof board.load === "function") {
      board.load();
    }
    return job;
  } catch (err) {
    // Persistence is best-effort; never fail the run on a store write error.
    // eslint-disable-next-line no-console
    console.error("[store] record_job failed", err);
    return null;
  }
}

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

  /** Render jobs grouped by status into the columns. Empty store -> show the centered
   *  placeholder; otherwise hide it. */
  function render(jobs) {
    clearColumns();
    const buckets = bucketize(jobs);
    let n = 0;
    for (const key of Object.keys(columns)) {
      const col = columns[key];
      const items = buckets[key];
      if (col.count) col.count.textContent = String(items.length);
      if (col.list) {
        for (const job of items) col.list.appendChild(renderJobCard(job));
      }
      n += items.length;
    }
    if (total) total.textContent = String(n);
    if (empty) empty.hidden = n !== 0;
  }

  /** Fetch jobs from the store and render. Failures log + clear the board rather than
   *  throw so a first launch (no store) never breaks the Board view. */
  async function load() {
    let t;
    try {
      t = requireTauri();
    } catch (err) {
      // Outside the webview: nothing to load. Leave the columns empty.
      // eslint-disable-next-line no-console
      console.warn("[board] skipped:", err.message);
      return;
    }
    try {
      const jobs = await t.core.invoke("list_jobs");
      render(jobs);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error("[board] list_jobs failed", err);
      render([]);
    }
  }

  if (refresh) {
    refresh.addEventListener("click", load);
  }

  return { load, render };
}

/** Rail (icon nav) view switching. Toggles .is-shown on the matching .view and
 *  updates the titlebar screen label. EH-0006: switching to the Library or Board route
 *  reloads the store so a run completed in the Workspace appears without a manual
 *  Reload click (both views are otherwise only refreshed on app load + on Run). */
export function wireRail(library, board) {
  const buttons = document.querySelectorAll(".rail__btn");
  const screenTitle = document.getElementById("screenTitle");
  buttons.forEach((btn) => {
    btn.addEventListener("click", () => {
      const route = btn.dataset.route;
      if (!route) return;
      buttons.forEach((b) => b.classList.remove("is-active"));
      btn.classList.add("is-active");
      document.querySelectorAll(".view").forEach((view) => {
        view.classList.toggle("is-shown", view.dataset.view === route);
      });
      if (screenTitle) {
        screenTitle.textContent = route.charAt(0).toUpperCase() + route.slice(1);
      }
      // Refresh the Library and Board from the store whenever they are shown, so a
      // just-finished run lands on tab switch.
      if (route === "library" && library && typeof library.load === "function") {
        library.load();
      }
      if (route === "board" && board && typeof board.load === "function") {
        board.load();
      }
    });
  });
}

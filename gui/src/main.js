// unlocr shell wiring.
//
// Two responsibilities:
//   1. Rail (left icon nav) view switching — toggles .view panes and the titlebar
//      screen label.
//   2. OCR run wiring — the Run button calls the `run_ocr` Tauri command and
//      listens for `ocr://` events to drive one progress bar + status text.
//      No streaming-token UI yet (a later card); this card only proves the bridge
//      end to end: preflight on load, run on click, events to UI, done -> markdown.
//
// Tauri is exposed globally (withGlobalTauri: true, no bundler, see CLAUDE.md), so
// we read `window.__TAURI__` instead of importing @tauri-apps/api. Guard every
// access so a stale non-Tauri context (e.g. opening index.html in a plain browser)
// fails softly instead of throwing on load.

const Tauri = () => window.__TAURI__;

/** Throw a friendly error if the global Tauri bridge is missing. */
function requireTauri() {
  const t = Tauri();
  if (!t || !t.core || !t.core.invoke) {
    throw new Error("Tauri bridge unavailable; open this page inside the app, not a browser.");
  }
  return t;
}

/** Run preflight on load. EH-0004 turns this into a GATE: if a required tool
 *  (llama-server or pdftoppm) is missing (report.ok === false), the Run button
 *  is disabled and the structured error is surfaced inline, so the user cannot
 *  start a run that is guaranteed to fail. On ok, Run is enabled. Still logs the
 *  report for the EH-0003 acceptance check. `ui` is optional so a stale
 *  non-Tauri caller (plain browser) can invoke this without a controller. */
async function preflightOnLoad(ui, rail) {
  const t = requireTauri();
  try {
    const report = await t.core.invoke("preflight");
    // eslint-disable-next-line no-console
    console.log("[preflight]", report);

    if (ui && typeof ui.applyPreflight === "function") {
      ui.applyPreflight(report);
    }
    // EH-0004 bite 2: the pipeline pane is bound to preflight-derived state.
    if (rail && typeof rail.renderPipeline === "function") {
      rail.renderPipeline(report);
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[preflight] failed", err);
    // Invoke itself threw (cache-dir failure surfaced as a thrown string): treat
    // as a hard block so we never let a broken-env run start.
    if (ui && typeof ui.applyPreflight === "function") {
      ui.applyPreflight({ ok: false, error: String(err) });
    }
    if (rail && typeof rail.renderPipeline === "function") {
      rail.renderPipeline({ ok: false, error: String(err) });
    }
  }
}

/** Subscribe to the four ocr:// events. Awaits every listen() so the handlers
 *  are actually attached before returning, then resolves to an array of
 *  UnlistenFn for teardown. Callers MUST await this before invoking run_ocr,
 *  otherwise early events (ocr://progress, server-ready) can fire before the
 *  listeners exist and the progress bar misses its opening states. */
async function subscribeOcrEvents(ui) {
  const t = requireTauri();
  const handlers = [];

  // Model/projector download: 0..=100 pct. Bar stays indeterminate-looking until
  // page work begins (download pct is informational, not the page bar).
  handlers.push([
    "ocr://progress",
    (e) => {
      const { name, pct } = e.payload || {};
      ui.setStatus(`downloading ${name ?? "model"} ${pct ?? 0}%`);
    },
  ]);

  // llama-server healthy, about to OCR pages. Indeterminate until first page.
  handlers.push([
    "ocr://server-ready",
    (e) => {
      const { port } = e.payload || {};
      ui.setStatus(`server ready on :${port}`);
      ui.showProgress(true);
      ui.setIndeterminate(true);
    },
  ]);

  // Per-page progress: determinate bar, page/total of total.
  handlers.push([
    "ocr://page",
    (e) => {
      const { page, total } = e.payload || {};
      ui.showProgress(true);
      ui.setIndeterminate(false);
      if (total > 0) {
        ui.setFill(Math.round((page / total) * 100));
      }
      ui.setStatus(`OCR page ${page}/${total > 0 ? total : "?"}`);
    },
  ]);

  // Terminal event: assembled markdown for the input.
  handlers.push([
    "ocr://done",
    (e) => {
      const { markdown } = e.payload || {};
      ui.renderMarkdown(markdown);
    },
  ]);

  // Emitted only when keep_images was set; payload carries the directory the
  // page PNGs were kept in. Without this listener the images are orphaned in a
  // temp dir with no way to find them. Show the dir in both the status line and
  // the transcript so it survives being scrolled past.
  handlers.push([
    "ocr://images-kept",
    (e) => {
      const { dir } = e.payload || {};
      if (!dir) return;
      ui.setStatus("page images kept in: " + dir);
      // Also append to the transcript so the path is not lost when the status
      // line is overwritten by a subsequent run's states.
      const body = document.getElementById("transcriptBody");
      if (body) {
        const note = document.createElement("p");
        note.className = "placeholder";
        note.style.cssText = "margin:0.5rem 0;font-size:0.8rem;";
        note.textContent = "Page images kept in: " + dir;
        body.appendChild(note);
      }
    },
  ]);

  // event.listen returns Promise<UnlistenFn>; await ALL of them so attachment is
  // real (not fire-and-forget) before any run_ocr event can arrive.
  return Promise.all(handlers.map(([event, handler]) => t.event.listen(event, handler)));
}

/** Controller over the Workspace file-rail panes (EH-0004 bite 2): the file
 *  list and the OCR pipeline stages. Both are bound to live state, not static
 *  placeholders:
 *    - file list   <- the input queued for the next run (path field + Import btn)
 *    - pipeline    <- the preflight report (tools/model/mmproj flags)
 *  Kept separate from the transcript UI controller so each pane owns its DOM. */
/** Split a path into a display name + the full path for a queued-file row.
 *  Handles both `/` and `\` so Windows paths (C:\Users\me\file.pdf) show the
 *  right basename. Returns null for an empty/whitespace-only string so callers
 *  can filter(Boolean). Extracted to module level so the self-check can test it
 *  and `makeLibrary` can reuse it instead of duplicating the logic. */
function splitPath(path) {
  const clean = (path || "").trim();
  if (!clean) return null;
  const sep = Math.max(clean.lastIndexOf("/"), clean.lastIndexOf("\\"));
  const name = sep >= 0 ? clean.slice(sep + 1) : clean;
  return { name: name || clean, path: clean };
}

function makeFileRail() {
  const count = document.getElementById("fileCount");
  const list = document.getElementById("fileList");
  const empty = document.getElementById("fileListEmpty");
  const engineLabel = document.getElementById("pipelineEngine");
  const stages = document.getElementById("pipelineStages");

  /** Render the file list from the queued input(s). Empty hides the row and
   *  shows the placeholder; one entry shows a single queued-file row. */
  function renderFiles(paths) {
    const items = (paths || []).map(splitPath).filter(Boolean);
    if (count) count.textContent = String(items.length);
    // Remove every previously rendered .file-row (keep the #fileListEmpty node).
    if (list) list.querySelectorAll(".file-row").forEach((n) => n.remove());
    if (items.length === 0) {
      if (empty) empty.hidden = false;
      return;
    }
    if (empty) empty.hidden = true;
    for (const item of items) {
      const row = document.createElement("div");
      row.className = "file-row";
      row.innerHTML =
        '<svg class="file-row__icon" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M3.5 2h6L13 5.5V14H3.5z"></path><path d="M9.5 2V5.5H13"></path></svg>' +
        '<div class="file-row__body">' +
        '<span class="file-row__name"></span>' +
        '<span class="file-row__path"></span>' +
        "</div>";
      row.querySelector(".file-row__name").textContent = item.name;
      row.querySelector(".file-row__path").textContent = item.path;
      if (list) list.appendChild(row);
    }
  }

  /** Render the OCR pipeline stages from a preflight report. Each stage lights
   *  up from the report's flags: tools/model/mmproj. ok => green, missing =>
   *  red, partial (build-too-old / only one tool) => amber. Unknown stays dim. */
  function renderPipeline(report) {
    if (engineLabel) {
      const quant = (report && report.quant) || "Q8_0";
      engineLabel.textContent = "Unlimited-OCR · " + quant;
    }
    if (!stages) return;
    const ok = !!(report && report.ok);
    const setStage = (key, state) => {
      const li = stages.querySelector('.stage[data-stage="' + key + '"]');
      if (!li) return;
      li.classList.remove("is-ok", "is-warn", "is-bad");
      if (state) li.classList.add("is-" + state);
    };

    // Tools: both llama-server and pdftoppm resolved on a successful report.
    if (!report) {
      setStage("tools", "");
    } else if (ok && report.llamaServer && report.pdftoppm) {
      // Soft warn if the build is known and below the min, even when ok.
      setStage("tools", "is-ok");
    } else {
      setStage("tools", "is-bad");
    }

    // Model GGUF + projector presence come straight off the report.
    setStage("model", report && report.modelPresent ? "is-ok" : report ? "is-bad" : "");
    setStage("mmproj", report && report.mmprojPresent ? "is-ok" : report ? "is-bad" : "");
  }

  return { renderFiles, renderPipeline };
}

/** Controller over the read-only Markdown review pane (EH-0004 bite 2). The pane
 *  is the dedicated result surface: after a run it fetches the written {stem}.md
 *  (path returned by the ocr command, via the read_text_file command) and renders
 *  its source here. contenteditable=false in the markup keeps it visibly read-only;
 *  this controller only ever writes textContent/pre, never enables editing. */
function makeMarkdownPane() {
  const body = document.getElementById("mdBody");
  const placeholder = document.getElementById("mdPlaceholder");
  const source = document.getElementById("mdSource");

  /** Render fetched markdown source + the on-disk path it came from. */
  function render(markdown, path) {
    if (!body) return;
    if (placeholder) placeholder.hidden = true;
    body.innerHTML = "";
    const pre = document.createElement("pre");
    pre.textContent = markdown || "";
    body.appendChild(pre);
    if (source) {
      source.hidden = !path;
      source.textContent = path ? "source: " + path : "";
    }
  }

  function clear() {
    if (body) body.innerHTML = "";
    if (placeholder && body) body.appendChild(placeholder);
    if (placeholder) placeholder.hidden = false;
    if (source) source.hidden = true;
  }

  return { render, clear };
}

/** Controller over the center PDF preview pane. Calls the `render_pages` command
 *  (cached on disk by the backend) and loads each page PNG through the asset
 *  protocol via convertFileSrc. Single image at a time; clicking it cycles pages
 *  (ponytail: no nav buttons, add prev/next if multi-page navigation needs it).
 *  Fails soft outside the webview so layout work still loads in a plain browser. */
function makePreviewPane() {
  const panel = document.querySelector(".panel.preview");
  if (!panel) return { show() {}, clear() {} };
  const stage = panel.querySelector(".preview__stage");
  const pageChip = panel.querySelector(".chip--soft");
  const pageCount = panel.querySelector(".preview__pagecount");
  let pages = []; // asset:// URLs, one per page
  let idx = 0;

  function paint() {
    if (!stage) return;
    stage.innerHTML = "";
    if (pages.length === 0) {
      const p = document.createElement("p");
      p.className = "placeholder";
      p.textContent = "Import a PDF to see a page preview here.";
      stage.appendChild(p);
      if (pageChip) pageChip.textContent = "Page 0";
      if (pageCount) pageCount.textContent = "page 0 / 0";
      return;
    }
    const img = document.createElement("img");
    img.className = "preview__img";
    img.src = pages[idx];
    img.alt = "PDF page " + (idx + 1);
    if (pages.length > 1) {
      img.title = "click for next page";
      img.style.cursor = "pointer";
      img.addEventListener("click", () => {
        idx = (idx + 1) % pages.length;
        paint();
      });
    }
    stage.appendChild(img);
    if (pageChip) pageChip.textContent = "Page " + (idx + 1);
    if (pageCount) pageCount.textContent = "page " + (idx + 1) + " / " + pages.length;
  }

  function clear() {
    pages = [];
    idx = 0;
    paint();
  }

  /** Render previews for one PDF path. Non-PDF or empty clears the pane. */
  async function show(pdfPath) {
    let t;
    try {
      t = requireTauri();
    } catch (err) {
      return; // plain browser: no backend render
    }
    if (!pdfPath || !pdfPath.toLowerCase().endsWith(".pdf")) {
      clear();
      return;
    }
    try {
      const paths = await t.core.invoke("render_pages", { pdfPath, dpi: null });
      pages = (paths || []).map((p) => t.core.convertFileSrc(p));
      idx = 0;
      paint();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[preview] render failed:", err.message);
      clear();
    }
  }

  return { show, clear };
}

/** EH-0006: pull the basename off a POSIX or Windows path for a job card title.
 *  Delegates to the module-level splitPath. Returns "(untitled run)" for a missing
 *  input path so a card never renders an empty title. Shared by the Library grid and
 *  the Board columns so the title logic is never duplicated. */
function jobBaseName(path) {
  const r = splitPath(path);
  return r ? r.name : "(untitled run)";
}

/** EH-0006: build a single read-only job card element from a Job record. Status
 *  drives the stripe + badge color via the .job-card--<status> class. Done shows the
 *  output path; failed shows the error. Options + timestamps are the meta footer.
 *  Module-level so the Library grid (all jobs) and the Board columns (jobs grouped by
 *  status) render identical cards — one source of truth for the card markup. */
function renderJobCard(job) {
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
 *  instead of throwing, so a first-launch (no store yet) stays usable. */
function makeLibrary() {
  const grid = document.getElementById("libraryGrid");
  const count = document.getElementById("libraryCount");
  const empty = document.getElementById("libraryEmpty");
  const refresh = document.getElementById("libraryRefresh");

  /** Replace the grid with cards for the given jobs (newest-first by createdAt so
   *  the most recent run is top-left). Empty -> placeholder shown. Cards are built
   *  by the shared module-level renderJobCard, so the Library and Board render the
   *  same card markup. */
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
      grid.appendChild(renderJobCard(job));
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

  return { load, render };
}

/** Format a unix epoch-seconds value as a short local timestamp for a card footer.
 *  Falls back to the raw number if the browser cannot parse it so the value is
 *  never lost. */
function formatEpoch(secs) {
  const n = Number(secs);
  if (!Number.isFinite(n) || n <= 0) return String(secs);
  const d = new Date(n * 1000);
  if (Number.isNaN(d.getTime())) return String(secs);
  // YYYY-MM-DD HH:MM in local time, compact and locale-stable.
  const pad = (x) => String(x).padStart(2, "0");
  return (
    d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate()) +
    " " + pad(d.getHours()) + ":" + pad(d.getMinutes())
  );
}

/** EH-0006: record a run's outcome to the job store after run_ocr completes or
 *  fails. Keeps OCR and persistence decoupled: a store write failure is logged but
 *  never rolls back the OCR result the user already received. Returns the stored
 *  Job (or null) so the caller can optionally refresh the Library with it. */
async function recordRunOutcome(inputPath, opts, status, outputPath, error, library, board) {
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
function makeBoard() {
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

/** Tiny controller over the transcript/progress DOM nodes. Keeps main flow flat. */
function makeUi() {
  const statusPill = document.querySelector(".status-pill");
  const statusDot = document.querySelector(".status-dot");
  const progress = document.getElementById("runProgress");
  const fill = document.getElementById("runProgressFill");
  const statusText = document.getElementById("runProgressStatus");
  const body = document.getElementById("transcriptBody");
  const placeholder = document.getElementById("transcriptPlaceholder");
  const runBtn = document.getElementById("runBtn");

  // Run OCR is gated on a LOADED model (litellm-style) and no run in flight. The
  // load itself validates the environment for the chosen mode (local needs
  // llama-server + pdftoppm; remote needs only pdftoppm), so a successful load is
  // proof the env is runnable — we do not also gate on preflight here (that would
  // wrongly block remote on a box without llama-server). `envOk` stays a soft
  // signal the preflight panel renders, not a Run gate.
  let envOk = false;
  let modelLoaded = false;
  let running = false;
  function gate() {
    if (!runBtn) return;
    runBtn.disabled = !modelLoaded || running;
    runBtn.classList.toggle("is-blocked", !running && !modelLoaded);
    if (running) {
      runBtn.textContent = "Running…";
    } else if (!modelLoaded) {
      runBtn.textContent = "Load a model first";
    } else {
      runBtn.textContent = "Run OCR";
    }
  }

  function setPill(state, label) {
    if (statusPill) {
      statusPill.className = "status-pill status-pill--" + state;
    }
    if (statusDot) {
      statusDot.className = "status-dot";
    }
    if (statusPill) {
      statusPill.innerHTML = '<span class="status-dot"></span>' + label;
    }
  }

  return {
    setStatus(text) {
      if (statusText) statusText.textContent = text;
    },
    showProgress(show) {
      if (progress) progress.hidden = !show;
    },
    setIndeterminate(on) {
      if (fill) fill.classList.toggle("is-indeterminate", on);
      if (on) fill.style.width = "";
    },
    setFill(pct) {
      if (fill) {
        fill.classList.remove("is-indeterminate");
        fill.style.width = Math.max(0, Math.min(100, pct)) + "%";
      }
    },
    renderMarkdown(md) {
      if (placeholder) placeholder.hidden = true;
      if (body) {
        const pre = document.createElement("pre");
        pre.textContent = md || "";
        body.appendChild(pre);
      }
    },
    reset() {
      if (placeholder) placeholder.hidden = false;
      if (body) body.innerHTML = "";
      if (body && placeholder) body.appendChild(placeholder);
      this.showProgress(false);
      this.setFill(0);
      this.setStatus("Idle");
    },
    setRunning(on) {
      running = on;
      gate();
      setPill(on ? "running" : "idle", on ? "Running" : "Idle");
    },
    /** Model load gate: enable Run only when a model is loaded. Called by
     *  refreshModelStatus after load/unload and on startup. */
    applyModelStatus(status) {
      modelLoaded = !!(status && status.loaded);
      gate();
    },
    fail(message) {
      this.showProgress(false);
      this.setStatus("error: " + message);
      setPill("idle", "Error");
    },
    /** Preflight is now informational, not the Run gate (the model-load gate is).
     *  A missing tool surfaces as a warning so the user knows what to install
     *  before loading a local model; remote mode does not need llama-server, so we
     *  never hard-block on it here. Tolerates a partial report (e.g. an invoke
     *  throw stringified to { ok:false, error }). */
    applyPreflight(report) {
      const ok = !!(report && report.ok);
      envOk = ok;
      gate();
      if (ok) {
        if (!modelLoaded) this.setStatus("Idle");
        setPill("idle", modelLoaded ? "Idle" : "Idle");
      } else {
        const reason = (report && report.error) || "environment not ready";
        this.setStatus("env warning: " + reason);
        // Surface the warning in the transcript so the user sees WHICH tool is
        // missing, without blocking remote runs.
        if (placeholder) placeholder.hidden = true;
        if (body && !body.querySelector("pre")) {
          body.innerHTML = "";
          const note = document.createElement("p");
          note.className = "placeholder placeholder--error";
          note.textContent =
            "Environment warning: " + reason +
            ". Local model load needs this; remote mode needs only poppler/pdftoppm.";
          body.appendChild(note);
        }
      }
    },
  };
}

/** Derive the parent directory of a path ("" if none). Used to pick the out_dir
 *  for a run so the {stem}.md is written beside the source PDF (mirrors the CLI's
 *  "output alongside input" intent) and the returned path is real. Handles both
 *  `/` and `\` so a Windows path (C:\Users\me\file.pdf) yields C:\Users\me, not
 *  "" (which would silently fall into in-memory mode and write no file). */
function parentDirOf(p) {
  const clean = (p || "").trim();
  if (!clean) return "";
  const sep = Math.max(clean.lastIndexOf("/"), clean.lastIndexOf("\\"));
  if (sep < 0) return ""; // bare filename: no parent
  if (sep === 0) return clean.slice(0, 1); // POSIX root: "/a.pdf" -> "/"
  const dir = clean.slice(0, sep);
  // Windows drive root: "C:\a.pdf" -> "C:" is drive-RELATIVE; join would resolve
  // against drive C's CWD, not the root. Return "C:\" so the path stays absolute.
  if (/^[A-Za-z]:$/.test(dir)) return dir + "\\";
  return dir;
}

// Self-check: POSIX + Windows path splitting. Cheap, runs once on load; throws
// loudly in the console if the separator logic regresses. (no test framework here)
(function selfCheckPaths() {
  // parentDirOf: returns the parent directory, empty string when no parent.
  const dirCases = [
    ["/home/me/a.pdf", "/home/me"],
    ["/a.pdf", "/"],
    ["a.pdf", ""],
    ["C:\\Users\\me\\a.pdf", "C:\\Users\\me"],
    ["C:\\a.pdf", "C:\\"],
  ];
  for (const [input, want] of dirCases) {
    const got = parentDirOf(input);
    if (got !== want) {
      // eslint-disable-next-line no-console
      console.error(`parentDirOf(${input}) = ${got}, want ${want}`);
    }
  }

  // splitPath: returns { name, path } where name is the basename (cross-platform).
  const splitCases = [
    ["/home/me/a.pdf",         "a.pdf"],
    ["/a.pdf",                 "a.pdf"],
    ["a.pdf",                  "a.pdf"],
    ["C:\\Users\\me\\a.pdf",   "a.pdf"],
    ["C:\\a.pdf",              "a.pdf"],
  ];
  for (const [input, wantName] of splitCases) {
    const r = splitPath(input);
    const gotName = r && r.name;
    if (gotName !== wantName) {
      // eslint-disable-next-line no-console
      console.error(`splitPath(${input}).name = ${gotName}, want ${wantName}`);
    }
    if (r && r.path !== input.trim()) {
      // eslint-disable-next-line no-console
      console.error(`splitPath(${input}).path = ${r.path}, want ${input.trim()}`);
    }
  }
  // Empty / whitespace-only input must return null (filtered by callers).
  if (splitPath("") !== null || splitPath("  ") !== null) {
    // eslint-disable-next-line no-console
    console.error("splitPath should return null for empty/whitespace input");
  }
})();

/** Read the engine/options controls (EH-0005 bites 1 + 2) into the run_ocr invoke
 *  payload. Every control defaults to unlocr::OcrOptions::default() in the markup
 *  (quant=Q8_0, dpi=144, max_tokens=4096, keep_images=false, prompt="<|grounding|>
 *  Convert the document to markdown."), so a user who touches nothing sends
 *  CLI-parity values. Invalid/empty numbers fall back to the same defaults so a
 *  half-typed field never crashes a run. An empty prompt also falls back to the
 *  default so a run never sends a blank prompt. Keys are camelCase to match the
 *  Tauri command's parameter names. */
function readRunOptions() {
  const quantEl = document.getElementById("optQuant");
  const dpiEl = document.getElementById("optDpi");
  const maxTokensEl = document.getElementById("optMaxTokens");
  const keepImagesEl = document.getElementById("optKeepImages");
  const promptEl = document.getElementById("optPrompt");

  const DEFAULT_DPI = 144;
  const DEFAULT_MAX_TOKENS = 4096;
  const DEFAULT_PROMPT = "<|grounding|>Convert the document to markdown.";
  const numOr = (el, fallback) => {
    const v = parseInt((el && el.value) || "", 10);
    return Number.isFinite(v) && v > 0 ? v : fallback;
  };
  const promptOr = (el, fallback) => {
    const v = (el && el.value) || "";
    // trim only newlines/whitespace at the ends; an interior-only edit is real.
    const trimmed = v.replace(/^\s+|\s+$/g, "");
    return trimmed.length > 0 ? trimmed : fallback;
  };

  return {
    quant: (quantEl && quantEl.value) || "Q8_0",
    dpi: numOr(dpiEl, DEFAULT_DPI),
    maxTokens: numOr(maxTokensEl, DEFAULT_MAX_TOKENS),
    keepImages: !!(keepImagesEl && keepImagesEl.checked),
    prompt: promptOr(promptEl, DEFAULT_PROMPT),
  };
}

/** Render the "effective values" summary (EH-0005 bite 2) next to Run so the user
 *  sees what the next run will send before clicking. Reads from the same
 *  readRunOptions() the Run button uses, so it can never drift from the payload.
 *  The prompt is summarized (not shown verbatim) to keep the line scannable: the
 *  first sentence after any grounding tag, truncated. Empty/missing nodes fall
 *  back to the defaults so the summary is correct on first paint. */
function renderEffectiveSummary() {
  const vals = document.getElementById("effectiveVals");
  if (!vals) return;
  const opts = readRunOptions();
  // Pull the human-readable tail of the prompt (strip a leading <|tag|>) and cap
  // it so a long custom prompt does not blow out the one-line summary.
  const promptShort = opts.prompt
    .replace(/^<\|[^|]*\|\>/, "")
    .replace(/^\s+|\s+$/g, "");
  const shown = promptShort.length > 0 ? promptShort.slice(0, 48) : opts.prompt.slice(0, 48);
  const ellipsis = promptShort.length > 48 ? "…" : "";
  vals.textContent =
    opts.quant +
    " · " + opts.dpi + " DPI · " + opts.maxTokens + " tok · " +
    "keep images " + (opts.keepImages ? "on" : "off") +
    " · prompt: “" + shown + ellipsis + "”";
}

/** Run OCR on a single PDF path end to end: subscribe events, invoke run_ocr,
 *  resolve the markdown result, and record the outcome to the job store. Shared by
 *  the Run button (typed path) and the drag-drop importer (dropped path) so both
 *  paths drive the same UI surfaces + the same store record. EH-0006 bite 4 calls
 *  this for each dropped PDF; the Run button still calls it for the typed path.
 *
 *  Assumes `path` is already validated (non-empty). Returns true on success so a
 *  caller (drag-drop) can decide whether to keep importing the next file.
 *
 *  `ui` may be null when there is no transcript UI to drive (kept optional so a
 *  future background importer can reuse the path without a progress surface). */
async function runOcrOnPath(path, ui, mdPane, unlistensRef, library, board) {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    if (ui) ui.fail(err.message);
    return false;
  }

  if (ui) {
    ui.reset();
    ui.setRunning(true);
    ui.showProgress(true);
    ui.setIndeterminate(true);
    ui.setStatus("starting…");
  }
  if (mdPane) mdPane.clear();

  // Tear down any listeners from a previous run before subscribing fresh ones,
  // so repeated Runs do not accumulate stale handlers that fire against the old
  // (now-reset) UI state. subscribeOcrEvents returns Promise<UnlistenFn>[]; await
  // them so the teardown is real, not fire-and-forget.
  const prev = unlistensRef.value;
  unlistensRef.value = [];
  if (Array.isArray(prev)) {
    await Promise.all(prev.map((p) => Promise.resolve(p).then((fn) => fn && fn())));
  }
  // Await attachment before invoking run_ocr so no early event is missed.
  if (ui) {
    unlistensRef.value = await subscribeOcrEvents(ui);
  } else {
    unlistensRef.value = [];
  }

  // Capture options ONCE before the run so the failure path records exactly what
  // the run used. The option controls are not disabled during a run, so re-reading
  // the DOM in the catch could record options the user changed mid-run.
  const opts = readRunOptions();

  try {
    // out_dir = the input's parent dir so run_ocr writes {stem}.md next to the
    // source (mirrors the CLI default of output-beside-input) and returns the
    // written path. inputs is a Vec for forward-compat with batch runs.
    // EH-0005 bite 1: the engine/options controls (quant/dpi/maxTokens/keepImages)
    // are forwarded into the run_ocr payload so the GUI drives quality, DPI, and
    // image-keeping instead of always sending CLI defaults.
    // A bare filename has no parent dir; fall back to "." (cwd) so run_ocr always
    // writes a file beside the input rather than silently flipping to in-memory
    // mode (empty out_dir) and writing nothing to disk.
    const outDir = parentDirOf(path) || ".";
    // quant is fixed at load time (the model is already held warm); run_ocr only
    // takes the per-run options below.
    const results = await t.core.invoke("run_ocr", {
      inputs: [path],
      outDir,
      maxTokens: opts.maxTokens,
      dpi: opts.dpi,
      prompt: opts.prompt,
      keepImages: opts.keepImages,
    });
    if (ui) {
      ui.setRunning(false);
      ui.setFill(100);
      ui.setStatus("done");
    }

    // run_ocr's return contract depends on out_dir: a non-empty out_dir yields
    // WRITTEN FILE PATHS, an empty out_dir yields in-memory markdown strings.
    // We must never render a path string as markdown text. Resolve which case we
    // are in from the same out_dir we sent, then drive the two surfaces:
    //   - markdown review pane: always from the on-disk file when out_dir was set
    //     (fetch via read_text_file), or from results[0] directly when in-memory
    //   - transcript pane: only the ocr://done event writes there (bite 4); here
    //     we populate it from the same resolved content as a fallback so a Run
    //     that completes before any event subscriber warms up still shows output.
    const haveFile = outDir.length > 0;
    let resolvedMd = "";
    let mdPath = "";
    if (results && results.length) {
      if (haveFile) {
        mdPath = results[0];
        try {
          // Pass outDir as allowedDir so the backend enforces the allowlist:
          // only .md files inside the run's output directory can be read.
          resolvedMd = await t.core.invoke("read_text_file", { path: mdPath, allowedDir: outDir });
        } catch (readErr) {
          // File read failed (rare: written then removed). Surface in the review
          // pane so the user sees why no markdown is shown, but keep the run green.
          if (mdPane) mdPane.render("could not read " + mdPath + ": " + String(readErr), mdPath);
          resolvedMd = "";
        }
      } else {
        // In-memory mode: results[0] IS the markdown content.
        resolvedMd = results[0];
      }
    }

    if (resolvedMd) {
      // The review pane (mdPane) shows the on-disk/in-memory markdown. The
      // transcript is driven solely by the ocr://done event (its listener is
      // attached before invoke, so it always fires): do NOT also append here, or
      // a race between the invoke resolving and the event firing renders twice.
      if (mdPane) mdPane.render(resolvedMd, mdPath);
    }

    // EH-0006: record the run's outcome so it shows in the Library grid. The
    // status is "done" only when we actually have a result; a run that returned
    // an empty results vec is still recorded as done (the backend decided to
    // emit nothing) with an empty output path. best-effort: a store failure logs
    // but never fails the run (recordRunOutcome swallows it).
    const outPath = haveFile && results && results.length ? results[0] : "";
    await recordRunOutcome(path, opts, "done", outPath, "", library, board);
    return true;
  } catch (err) {
    if (ui) {
      ui.setRunning(false);
      ui.fail(String(err));
    }
    // EH-0006: record the failed run too so the Library and Board show it as a
    // failed card (not silently dropped). The user already saw the error in the UI.
    await recordRunOutcome(path, opts, "failed", "", String(err), library, board);
    return false;
  }
}

/** Wire the Run button: validate path, then delegate to runOcrOnPath.
 *  EH-0004 bite 2: on success the written {stem}.md (path returned by run_ocr) is
 *  fetched via read_text_file and rendered into the read-only Markdown review pane.
 *  EH-0006: on completion (success or failure) the outcome is recorded to the job
 *  store via record_job so it appears in the Library grid and on the Board; the
 *  record call is best-effort and never rolls back a delivered OCR result. */
function wireRunButton(ui, mdPane, unlistensRef, library, board) {
  const runBtn = document.getElementById("runBtn");
  const pathInput = document.getElementById("pdfPath");
  if (!runBtn) return;

  runBtn.addEventListener("click", () => {
    const path = (pathInput && pathInput.value || "").trim();
    if (!path) {
      ui.fail("enter a PDF path first");
      return;
    }
    // Fire-and-forget: the click handler cannot await without holding the event,
    // and runOcrOnPath owns its own UI state transitions + error surfacing.
    runOcrOnPath(path, ui, mdPane, unlistensRef, library, board);
  });
}

/** EH-0006 bite 4: drag-drop PDF import onto the Library grid. Subscribes to the
 *  Tauri drag-drop event channel (tauri://drag-enter / drag-over / drag-leave /
 *  drag-drop) — the same window.__TAURI__.event.listen the OCR progress events use,
 *  so no new API surface and no bundler import is needed. The drop payload carries
 *  the absolute file paths the OS handed the webview.
 *
 *  Only PDFs are enqueued (the pipeline is PDF/page-rasterize -> OCR). Non-PDF drops
 *  are surfaced as a status message and skipped, never crash the importer. The
 *  highlight (.is-drop-target on the grid) only lights up while a drag is over the
 *  Library view so the affordance reads as "drop here to enqueue".
 *
 *  Each accepted PDF is enqueued as a real run_ocr job via the shared runOcrOnPath
 *  (same path the Run button takes) and recorded to the store, so a dropped file
 *  lands in the Library grid and on the Board exactly like a button-driven run.
 *  Runs are sequential: the backend spawns one llama-server per run_ocr, so a
 *  parallel fan-out would race on the model/port. Returns the unlisten so the
 *  caller can tear it down if needed (it lives for the app lifetime today). */
function wireLibraryDrop(ui, mdPane, unlistensRef, library, board) {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    // Outside the webview (plain browser): HTML5 drag-drop would be a separate
    // path with no OS file paths. Skip wiring rather than throw on load.
    // eslint-disable-next-line no-console
    console.warn("[drop] drag-drop wiring skipped:", err.message);
    return null;
  }

  const grid = document.getElementById("libraryGrid");
  const empty = document.getElementById("libraryEmpty");
  // Track an in-flight import so a second drop while a run is live does not race
  // two llama-servers. The next drop is accepted only after the current finishes.
  let importing = false;

  /** True when the Library view is the active screen (only then is a drop an
   *  import intent, and only then do we paint the highlight). */
  function libraryIsActive() {
    const view = document.querySelector('.view[data-view="library"]');
    return !!(view && view.classList.contains("is-shown"));
  }

  /** Paint/clear the drop affordance. A drop hint replaces the empty placeholder
   *  while dragging so the user sees "drop PDFs to import". */
  function setDropTarget(on) {
    if (grid) grid.classList.toggle("is-drop-target", on);
    if (empty) {
      empty.textContent = on
        ? "Drop PDF files to import and run OCR."
        : "No OCR jobs yet. Run OCR to populate the library.";
    }
  }

  /** Enqueue one or more dropped PDFs sequentially. Non-PDF entries are reported
   *  and skipped. Re-arms the importer when the queue drains. */
  async function enqueueDrops(paths) {
    const pdfs = (paths || []).filter((p) => typeof p === "string" && p.trim());
    if (pdfs.length === 0) {
      if (ui) ui.setStatus("drop ignored: no files");
      return;
    }
    const accepted = pdfs.filter((p) => p.toLowerCase().endsWith(".pdf"));
    const rejected = pdfs.filter((p) => !p.toLowerCase().endsWith(".pdf"));
    if (rejected.length) {
      // eslint-disable-next-line no-console
      console.warn("[drop] skipped non-PDF drops:", rejected);
    }
    if (accepted.length === 0) {
      if (ui) ui.setStatus("drop ignored: not a PDF");
      return;
    }
    if (importing) {
      // eslint-disable-next-line no-console
      console.warn("[drop] import already in flight; ignoring new drop");
      return;
    }
    importing = true;
    try {
      for (const pdf of accepted) {
        // eslint-disable-next-line no-console
        console.log("[drop] enqueuing OCR job:", pdf);
        // Each run is awaited so llama-server is torn down before the next starts.
        await runOcrOnPath(pdf, ui, mdPane, unlistensRef, library, board);
      }
    } finally {
      importing = false;
    }
  }

  // Tauri 2 emits drag events over the standard event channel as
  // tauri://drag-enter / drag-over / drag-leave / tauri://drag-drop. The drop
  // payload is { paths: string[] }; the others carry position info we do not need.
  const handlers = [
    [
      "tauri://drag-enter",
      () => {
        if (libraryIsActive()) setDropTarget(true);
      },
    ],
    [
      "tauri://drag-over",
      () => {
        if (libraryIsActive()) setDropTarget(true);
      },
    ],
    [
      "tauri://drag-leave",
      () => setDropTarget(false),
    ],
    [
      "tauri://drag-drop",
      (e) => {
        setDropTarget(false);
        // Only import when dropped over the Library view; a drop elsewhere is left
        // for any future target (e.g. the Workspace) rather than silently running.
        if (!libraryIsActive()) return;
        const paths = (e && e.payload && e.payload.paths) || [];
        enqueueDrops(paths);
      },
    ],
  ];

  // event.listen returns Promise<UnlistenFn>; attach all before returning. The
  // unlistens are not tracked in unlistensRef because they must outlive every run
  // (they are app-lifetime listeners, not per-run like the OCR progress ones).
  Promise.all(
    handlers.map(([event, handler]) => t.event.listen(event, handler)),
  ).catch((err) => {
    // eslint-disable-next-line no-console
    console.error("[drop] failed to attach drag-drop listeners", err);
  });

  return null;
}

/** Rail (icon nav) view switching. Toggles .is-shown on the matching .view and
 *  updates the titlebar screen label. EH-0006: switching to the Library or Board route
 *  reloads the store so a run completed in the Workspace appears without a manual
 *  Reload click (both views are otherwise only refreshed on app load + on Run). */
function wireRail(library, board) {
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

/** Update the titlebar Local/Remote/No-model badge from a model_status payload. */
function updateModeBadge(status) {
  const badge = document.getElementById("modeBadge");
  if (!badge) return;
  const loaded = !!(status && status.loaded);
  const mode = status && status.mode;
  const dotClass = !loaded ? "is-off" : mode === "remote" ? "is-remote" : "is-loaded";
  const label = !loaded ? "No model" : mode === "remote" ? "Remote" : "Local";
  badge.innerHTML = '<span class="titlebar__mode-dot ' + dotClass + '"></span>' + label;
}

/** Fetch model_status and fan it out: the Run gate (ui), the titlebar badge, and
 *  the model bar (status text + Load/Unload enablement). Called on startup and
 *  after every load/unload. Fail-soft outside the webview. */
async function refreshModelStatus(ui) {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  let status = { loaded: false, mode: "", label: "" };
  try {
    status = await t.core.invoke("model_status");
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[model] status failed", err);
  }
  if (ui && typeof ui.applyModelStatus === "function") ui.applyModelStatus(status);
  updateModeBadge(status);
  const loadBtn = document.getElementById("loadModelBtn");
  const unloadBtn = document.getElementById("unloadModelBtn");
  const statusText = document.getElementById("modelStatusText");
  if (unloadBtn) unloadBtn.disabled = !status.loaded;
  if (loadBtn) loadBtn.textContent = status.loaded ? "Reload model" : "Load model";
  if (statusText) {
    statusText.textContent = status.loaded ? "Loaded: " + status.label : "No model loaded";
  }
}

/** Wire the OCR engine tabs (Unlimited-OCR | Remote). Toggling the tab sets the
 *  active engine and shows/hides the remote URL/key fields. The active tab's
 *  data-engine is read by the Load button to pick local vs remote. */
function wireEngineTabs() {
  const tabs = document.getElementById("engineTabs");
  const remoteFields = document.getElementById("remoteFields");
  if (!tabs) return;
  tabs.querySelectorAll(".seg__btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      tabs.querySelectorAll(".seg__btn").forEach((b) => b.classList.remove("is-active"));
      btn.classList.add("is-active");
      if (remoteFields) remoteFields.hidden = btn.dataset.engine !== "remote";
    });
  });
}

/** Return the active engine tab's mode ("local" | "remote"). */
function activeEngineMode() {
  const active = document.querySelector("#engineTabs .seg__btn.is-active");
  return (active && active.dataset.engine) || "local";
}

/** Wire the Load/Unload model buttons. Load reads the active engine mode + the
 *  quant (local) or remote URL/key (remote) and calls load_model, then refreshes
 *  status. Loading is long (download + health wait) so the button shows progress
 *  via the app-lifetime ocr:// listeners attached in attachLoadListeners. */
function wireModelBar(ui) {
  const loadBtn = document.getElementById("loadModelBtn");
  const unloadBtn = document.getElementById("unloadModelBtn");
  const statusText = document.getElementById("modelStatusText");

  if (loadBtn) {
    loadBtn.addEventListener("click", async () => {
      let t;
      try {
        t = requireTauri();
      } catch (err) {
        if (statusText) statusText.textContent = "unavailable outside the app";
        return;
      }
      const mode = activeEngineMode();
      const quantEl = document.getElementById("optQuant");
      const urlEl = document.getElementById("remoteUrl");
      const keyEl = document.getElementById("remoteKey");
      loadBtn.disabled = true;
      if (unloadBtn) unloadBtn.disabled = true;
      if (statusText) statusText.textContent = "loading…";
      try {
        const status = await t.core.invoke("load_model", {
          mode,
          quant: quantEl ? quantEl.value : null,
          baseUrl: urlEl ? urlEl.value : null,
          apiKey: keyEl ? keyEl.value : null,
          llamaBin: null,
        });
        if (ui) ui.applyModelStatus(status);
      } catch (err) {
        if (statusText) statusText.textContent = "load failed: " + String(err);
      } finally {
        loadBtn.disabled = false;
        await refreshModelStatus(ui);
      }
    });
  }

  if (unloadBtn) {
    unloadBtn.addEventListener("click", async () => {
      let t;
      try {
        t = requireTauri();
      } catch (err) {
        return;
      }
      try {
        await t.core.invoke("unload_model");
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[model] unload failed", err);
      }
      await refreshModelStatus(ui);
    });
  }
}

/** App-lifetime listeners that surface model LOAD progress in the model bar
 *  (download pct + server-ready). These events now fire during load_model, not
 *  run_ocr, so they belong to the model bar rather than the per-run subscription. */
function attachLoadListeners() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const statusText = document.getElementById("modelStatusText");
  t.event.listen("ocr://progress", (e) => {
    const { name, pct } = (e && e.payload) || {};
    if (statusText) statusText.textContent = "downloading " + (name || "model") + " " + (pct || 0) + "%";
  });
  t.event.listen("ocr://server-ready", (e) => {
    const { port } = (e && e.payload) || {};
    if (statusText) statusText.textContent = "server ready on :" + port;
  });
}

/** Mark which quant options are already cached on disk (list_local_models) by
 *  appending a check to their label. Applies to both the run-time Quant select and
 *  the Settings default-quant select. Best-effort; never throws. */
async function markCachedQuants() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  let cached = [];
  try {
    cached = await t.core.invoke("list_local_models");
  } catch (err) {
    return;
  }
  const set = new Set(cached || []);
  ["optQuant", "setQuant"].forEach((id) => {
    const sel = document.getElementById(id);
    if (!sel) return;
    Array.from(sel.options).forEach((opt) => {
      const base = opt.value;
      opt.textContent = set.has(base) ? base + " ✓ cached" : base;
    });
  });
}

/** Apply persisted settings to the live workspace controls (engine defaults,
 *  provider mode, remote fields) so a user's saved defaults seed each session. */
function applySettingsToControls(s) {
  if (!s) return;
  const setVal = (id, v) => {
    const el = document.getElementById(id);
    if (el != null && v != null) el.value = v;
  };
  setVal("optQuant", s.defaultQuant);
  setVal("optDpi", s.defaultDpi);
  setVal("optMaxTokens", s.defaultMaxTokens);
  setVal("optPrompt", s.defaultPrompt);
  setVal("remoteUrl", s.remoteBaseUrl);
  setVal("remoteKey", s.remoteApiKey);
  // Select the engine tab matching the saved mode.
  const tabs = document.getElementById("engineTabs");
  const remoteFields = document.getElementById("remoteFields");
  if (tabs) {
    tabs.querySelectorAll(".seg__btn").forEach((b) => {
      b.classList.toggle("is-active", b.dataset.engine === (s.mode || "local"));
    });
    if (remoteFields) remoteFields.hidden = (s.mode || "local") !== "remote";
  }
}

/** Wire the Settings panel: load persisted settings into the form on startup and
 *  persist them on Save (also re-applying to the workspace controls). */
async function wireSettings(onSaved) {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const ids = {
    mode: "setMode",
    defaultQuant: "setQuant",
    remoteBaseUrl: "setRemoteUrl",
    remoteApiKey: "setRemoteKey",
    llamaBin: "setLlamaBin",
    defaultDpi: "setDpi",
    defaultMaxTokens: "setMaxTokens",
    defaultPrompt: "setPrompt",
  };
  const get = (id) => document.getElementById(id);

  let current = null;
  try {
    current = await t.core.invoke("get_settings");
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[settings] load failed", err);
  }
  if (current) {
    Object.keys(ids).forEach((k) => {
      const el = get(ids[k]);
      if (el != null && current[k] != null) el.value = current[k];
    });
    applySettingsToControls(current);
    if (typeof onSaved === "function") onSaved(current);
  }

  const saveBtn = document.getElementById("settingsSave");
  const saved = document.getElementById("settingsSaved");
  if (saveBtn) {
    saveBtn.addEventListener("click", async () => {
      const num = (id, fallback) => {
        const v = parseInt((get(id) && get(id).value) || "", 10);
        return Number.isFinite(v) && v > 0 ? v : fallback;
      };
      const newSettings = {
        mode: (get(ids.mode) && get(ids.mode).value) || "local",
        defaultQuant: (get(ids.defaultQuant) && get(ids.defaultQuant).value) || "Q8_0",
        remoteBaseUrl: (get(ids.remoteBaseUrl) && get(ids.remoteBaseUrl).value) || "",
        remoteApiKey: (get(ids.remoteApiKey) && get(ids.remoteApiKey).value) || "",
        llamaBin: (get(ids.llamaBin) && get(ids.llamaBin).value) || "",
        defaultDpi: num(ids.defaultDpi, 144),
        defaultMaxTokens: num(ids.defaultMaxTokens, 4096),
        defaultPrompt:
          (get(ids.defaultPrompt) && get(ids.defaultPrompt).value) ||
          "<|grounding|>Convert the document to markdown.",
      };
      try {
        await t.core.invoke("save_settings", { newSettings });
        applySettingsToControls(newSettings);
        renderEffectiveSummary();
        if (typeof onSaved === "function") onSaved(newSettings);
        if (saved) {
          saved.hidden = false;
          setTimeout(() => {
            saved.hidden = true;
          }, 1500);
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[settings] save failed", err);
      }
    });
  }
}

window.addEventListener("DOMContentLoaded", () => {
  const library = makeLibrary();
  const board = makeBoard();
  wireRail(library, board);

  const ui = makeUi();
  const rail = makeFileRail();
  const mdPane = makeMarkdownPane();
  const unlistensRef = { value: [] };
  wireRunButton(ui, mdPane, unlistensRef, library, board);
  // EH-0006 bite 4: drag-drop PDF import onto the Library grid. Wired once on app
  // load; the listeners live for the app lifetime and are scoped to the Library
  // view inside the handler. Fail-soft outside the webview (plain browser).
  wireLibraryDrop(ui, mdPane, unlistensRef, library, board);

  // Model load/remote wiring: engine tabs (local/remote), the Load/Unload bar,
  // the app-lifetime load-progress listeners, and the settings panel. Load
  // settings first so saved defaults seed the controls, then mark which quants are
  // cached, then read the live model status to set the Run gate + badge.
  wireEngineTabs();
  wireModelBar(ui);
  attachLoadListeners();
  wireSettings(() => {
    markCachedQuants();
  });
  markCachedQuants();
  refreshModelStatus(ui);

  // EH-0004 bite 2: the file list pane is bound to the path field + Import
  // button, so it reflects the input queued for the next run instead of a
  // static placeholder. The Import button seeds the field; typing does too.
  const pathInput = document.getElementById("pdfPath");
  const importBtn = document.getElementById("importBtn");
  const preview = makePreviewPane();
  if (pathInput) {
    const syncFiles = () => {
      const v = (pathInput.value || "").trim();
      rail.renderFiles(v ? [v] : []);
    };
    // Preview render shells out to pdftoppm, so only refresh on `change`
    // (blur/Enter) + explicit picker selection, never per keystroke.
    const refreshPreview = () => preview.show((pathInput.value || "").trim());
    pathInput.addEventListener("input", syncFiles);
    pathInput.addEventListener("change", syncFiles);
    pathInput.addEventListener("change", refreshPreview);
    // Import opens the native file picker (tauri-plugin-dialog, exposed at
    // window.__TAURI__.dialog via withGlobalTauri). Single-select: the chosen
    // path seeds the field, exactly like typing it. Batch import stays on
    // drag-drop (wireLibraryDrop). Outside the webview the API is absent, so
    // fall back to focusing the field for manual entry.
    // ponytail: single-select picker; flip `multiple: true` + loop runOcrOnPath
    // here if a multi-file picker is needed.
    if (importBtn) {
      importBtn.addEventListener("click", async () => {
        const dialog = window.__TAURI__ && window.__TAURI__.dialog;
        if (!dialog || !dialog.open) {
          pathInput.focus();
          return;
        }
        try {
          const selected = await dialog.open({
            multiple: false,
            directory: false,
            filters: [{ name: "PDF", extensions: ["pdf"] }],
          });
          if (typeof selected === "string") {
            pathInput.value = selected;
            syncFiles();
            refreshPreview();
          }
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("[import] picker failed:", err.message);
          pathInput.focus();
        }
      });
    }
  }
  // Start empty (matches the "No files imported yet" placeholder).
  rail.renderFiles([]);

  // EH-0005 bite 2: the "effective values" summary mirrors whatever the engine
  // options controls hold, so it never drifts from the next Run's payload. Update
  // it on every change of any control (input/change covers select, number,
  // checkbox, and textarea) and once on load for the correct first paint.
  const optsControls = document.querySelectorAll(
    "#runOpts input, #runOpts select, #runOpts textarea, #optKeepImages"
  );
  optsControls.forEach((el) => {
    el.addEventListener("input", renderEffectiveSummary);
    el.addEventListener("change", renderEffectiveSummary);
  });
  renderEffectiveSummary();

  // Preflight only runs inside the Tauri webview; fail soft otherwise so the
  // static page still loads in a plain browser (e.g. for layout work). Passing
  // `ui` turns preflight into the Run-gate (EH-0004 bite 1): a missing tool
  // disables Run and shows the reason inline. Passing `rail` (bite 2) drives the
  // pipeline stages from the same report.
  try {
    preflightOnLoad(ui, rail);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[preflight] skipped:", err.message);
  }

  // EH-0006: load the persisted job store on startup so the Library grid (bite 2)
  // and the Board columns (bite 3) show prior runs immediately (both are reloaded on
  // Run + on tab switch too). Fail soft outside the webview (plain browser) so layout
  // work still loads.
  try {
    library.load();
    board.load();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[store] load skipped:", err.message);
  }
});

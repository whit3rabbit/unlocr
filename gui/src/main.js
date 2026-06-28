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

  // Streaming token chunks: one event per token, arrives during inference.
  // Routed through ui.appendPartial so the per-page <pre> grows in the transcript
  // AND the run-popup log, and the stream cursor state lives in makeUi (this
  // function cannot see the transcript body/placeholder in its own scope).
  handlers.push([
    "ocr://partial-text",
    (e) => {
      const { page, chunk } = e.payload || {};
      ui.appendPartial(page, chunk);
    },
  ]);

  // Terminal event: assembled markdown for the input.
  // Clear the streaming pres for this input (they were provisional; the
  // assembled markdown from ocr://done is the canonical result) and render
  // the final version. Reset streamPre so the next input gets a fresh block.
  handlers.push([
    "ocr://done",
    (e) => {
      const { markdown } = e.payload || {};
      // Drop the provisional streaming pres, then render the assembled markdown
      // so the transcript shows the clean result.
      ui.clearPartial();
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
 *  protocol via convertFileSrc. Single image at a time; prev/next buttons and the
 *  Left/Right arrow keys page through, clamped to [0, n-1] (no wrap). Clicking the
 *  image advances one page. Fails soft outside the webview so layout work still
 *  loads in a plain browser. */
function makePreviewPane() {
  const panel = document.querySelector(".panel.preview");
  if (!panel) return { show() {}, clear() {} };
  const stage = panel.querySelector(".preview__stage");
  const pageChip = panel.querySelector(".chip--soft");
  const pageCount = panel.querySelector(".preview__pagecount");
  const prevBtn = panel.querySelector("#prevPage");
  const nextBtn = panel.querySelector("#nextPage");
  let pages = []; // asset:// URLs, one per page
  let idx = 0;

  // Clamp to [0, n-1] (no wrap) so the bounds buttons can disable at the ends.
  function go(delta) {
    if (pages.length === 0) return;
    const next = Math.min(Math.max(idx + delta, 0), pages.length - 1);
    if (next !== idx) {
      idx = next;
      paint();
    }
  }

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
      if (prevBtn) prevBtn.disabled = true;
      if (nextBtn) nextBtn.disabled = true;
      return;
    }
    const img = document.createElement("img");
    img.className = "preview__img";
    img.src = pages[idx];
    img.alt = "PDF page " + (idx + 1);
    if (pages.length > 1) {
      img.title = "click for next page";
      img.style.cursor = "pointer";
      img.addEventListener("click", () => go(1));
    }
    stage.appendChild(img);
    if (pageChip) pageChip.textContent = "Page " + (idx + 1);
    if (pageCount) pageCount.textContent = "page " + (idx + 1) + " / " + pages.length;
    if (prevBtn) prevBtn.disabled = idx <= 0;
    if (nextBtn) nextBtn.disabled = idx >= pages.length - 1;
  }

  if (prevBtn) prevBtn.addEventListener("click", () => go(-1));
  if (nextBtn) nextBtn.addEventListener("click", () => go(1));
  // Arrow keys page when the preview pane has focus or hover (don't hijack typing
  // in an input/textarea elsewhere).
  document.addEventListener("keydown", (e) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    const t = e.target;
    if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT")) return;
    if (!panel.matches(":hover")) return;
    go(e.key === "ArrowRight" ? 1 : -1);
  });

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

/** EH-0015: navigate to the Review view and render the .md for a done job.
 *  Called when the user clicks a job card whose status is "done" and has an
 *  outputPath. Switches the rail to "review", loads the markdown from disk via
 *  read_text_file, and renders it in the review pane. Fail-soft: errors are
 *  logged but never crash the Library view.
 *
 *  `outputPath` is the absolute path to the written {stem}.md.
 *  `mdPane`     the makeMarkdownPane() controller.
 *  `buttons`    the rail button NodeList (to update is-active + titlebar). */
async function openInReview(outputPath, mdPane, buttons) {
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
 *  Module-level so the Library grid (all jobs) and the Board columns (jobs grouped by
 *  status) render identical cards — one source of truth for the card markup.
 *
 *  EH-0015: `mdPane` and `railButtons` are optional. When provided, cards for done
 *  jobs with an outputPath are clickable: clicking opens the .md in the review pane
 *  (via openInReview). The pointer cursor and a "Open in review" aria-label signal
 *  the affordance. Board column cards do not receive these so they stay inert. */
function renderJobCard(job, mdPane, railButtons) {
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
function makeLibrary() {
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
  // Run popup: a dismissible panel mirroring the progress bar + live token log,
  // with a Stop button. Closing it minimizes to a clickable toast that reopens.
  const popup = document.getElementById("runPopup");
  const popupFill = document.getElementById("runPopupFill");
  const popupStatus = document.getElementById("runPopupStatus");
  const popupLog = document.getElementById("runPopupLog");
  const stopBtn = document.getElementById("stopBtn");
  const popupClose = document.getElementById("runPopupClose");
  // Tracks the current per-page <pre> in the transcript so streamed chunks for a
  // page land in one block; reset across pages/inputs. Lives here (not in
  // subscribeOcrEvents) so the partial-text handler routes through ui.appendPartial
  // and shares state with reset()/clearPartial().
  let streamPre = null;
  // Controls greyed out during a run so a second run can't be launched and run
  // options can't change mid-flight. loadModelBtn + importBtn + every #runOpts input.
  function setControlsDisabled(on) {
    const ids = ["loadModelBtn", "importBtn"];
    ids.forEach((id) => {
      const el = document.getElementById(id);
      if (el) el.disabled = on;
    });
    document
      .querySelectorAll("#runOpts input, #runOpts select, #runOpts textarea")
      .forEach((el) => {
        el.disabled = on;
      });
  }

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

  const api = {
    setStatus(text) {
      if (statusText) statusText.textContent = text;
      if (popupStatus) popupStatus.textContent = text;
    },
    showProgress(show) {
      if (progress) progress.hidden = !show;
    },
    setIndeterminate(on) {
      if (fill) fill.classList.toggle("is-indeterminate", on);
      if (on && fill) fill.style.width = "";
      if (popupFill) {
        popupFill.classList.toggle("is-indeterminate", on);
        if (on) popupFill.style.width = "";
      }
    },
    setFill(pct) {
      const w = Math.max(0, Math.min(100, pct)) + "%";
      if (fill) {
        fill.classList.remove("is-indeterminate");
        fill.style.width = w;
      }
      if (popupFill) {
        popupFill.classList.remove("is-indeterminate");
        popupFill.style.width = w;
      }
    },
    openPopup() {
      if (popup) popup.hidden = false;
    },
    closePopup() {
      if (popup) popup.hidden = true;
    },
    isRunning() {
      return running;
    },
    /** Append one streamed token chunk for `page` to both the transcript (one
     *  <pre> per page) and the popup log. Centralized here so subscribeOcrEvents
     *  does not reach for body/placeholder it cannot see in its scope. */
    appendPartial(page, chunk) {
      if (typeof chunk !== "string") return;
      if (body) {
        if (streamPre === null || streamPre.dataset.page !== String(page)) {
          if (placeholder) placeholder.hidden = true;
          streamPre = document.createElement("pre");
          streamPre.dataset.page = String(page);
          body.appendChild(streamPre);
        }
        streamPre.textContent += chunk;
        body.scrollTop = body.scrollHeight;
      }
      if (popupLog) {
        popupLog.textContent += chunk;
        // ponytail: the popup log accumulates every page of a batch; cap it so a
        // long run can't grow it without bound (keep the most recent tail).
        if (popupLog.textContent.length > 100000) {
          popupLog.textContent = popupLog.textContent.slice(-80000);
        }
        popupLog.scrollTop = popupLog.scrollHeight;
      }
    },
    /** Drop the provisional per-page <pre>s (ocr://done renders the assembled
     *  markdown instead) and reset the stream cursor. Popup log is left intact so
     *  the user can still scroll the streamed output after completion. */
    clearPartial() {
      if (body) body.querySelectorAll("pre[data-page]").forEach((n) => n.remove());
      streamPre = null;
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
      streamPre = null;
      if (popupLog) popupLog.textContent = "";
      this.showProgress(false);
      this.setFill(0);
      this.setStatus("Idle");
    },
    setRunning(on) {
      running = on;
      gate();
      setControlsDisabled(on);
      setPill(on ? "running" : "idle", on ? "Running" : "Idle");
      if (on) {
        if (stopBtn) stopBtn.disabled = false;
        this.openPopup();
      } else {
        // Run ended (done/fail/stopped): disable Stop so it can't kill the warm
        // server when no run is in flight, and drop the "minimized" toast.
        if (stopBtn) stopBtn.disabled = true;
        removeToast("ocr:running");
      }
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

  // Stop: ask the backend to cancel (kills the local server -> in-flight read
  // aborts; run_ocr remaps to "stopped"). One-shot: disable the button + show
  // intent. The run's catch path surfaces the final "stopped" state.
  if (stopBtn) {
    stopBtn.addEventListener("click", async () => {
      stopBtn.disabled = true;
      api.setStatus("stopping…");
      try {
        const t = requireTauri();
        await t.core.invoke("stop_ocr");
      } catch (err) {
        // Best-effort; the run will still error out on its own.
      }
    });
  }

  // Close (×): minimize. If a run is in flight, leave a clickable toast that
  // reopens the popup; otherwise just hide it.
  if (popupClose) {
    popupClose.addEventListener("click", () => {
      api.closePopup();
      if (running) {
        const el = showToast("ocr:running", {
          kind: "info",
          title: "OCR running — click to reopen",
        });
        if (el) {
          el.style.cursor = "pointer";
          el.onclick = () => {
            api.openPopup();
            removeToast("ocr:running");
          };
        }
      }
    });
  }

  return api;
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
  const repeatPenaltyEl = document.getElementById("optRepeatPenalty");

  const DEFAULT_DPI = 144;
  const DEFAULT_MAX_TOKENS = 4096;
  const DEFAULT_PROMPT = "<|grounding|>Convert the document to markdown.";
  const numOr = (el, fallback) => {
    const v = parseInt((el && el.value) || "", 10);
    return Number.isFinite(v) && v > 0 ? v : fallback;
  };
  // Optional float; blank/invalid -> null so the backend omits it (server default).
  const floatOrNull = (el) => {
    const v = parseFloat((el && el.value) || "");
    return Number.isFinite(v) && v > 0 ? v : null;
  };
  const promptOr = (el, fallback) => {
    const v = (el && el.value) || "";
    // trim only newlines/whitespace at the ends; an interior-only edit is real.
    const trimmed = v.replace(/^\s+|\s+$/g, "");
    return trimmed.length > 0 ? trimmed : fallback;
  };

  const pages = readPageSelection();

  return {
    quant: (quantEl && quantEl.value) || "Q8_0",
    dpi: numOr(dpiEl, DEFAULT_DPI),
    maxTokens: numOr(maxTokensEl, DEFAULT_MAX_TOKENS),
    keepImages: !!(keepImagesEl && keepImagesEl.checked),
    prompt: promptOr(promptEl, DEFAULT_PROMPT),
    repeatPenalty: floatOrNull(repeatPenaltyEl),
    // 1-based inclusive page range; both null = all pages. The backend validates
    // first>=1 and last>=first (a direct invoke bypasses the form min= clamp).
    firstPage: pages.firstPage,
    lastPage: pages.lastPage,
  };
}

/** Read the Pages control (All / Range / Single) into a `{firstPage, lastPage}`
 *  pair. All (or a blank/invalid entry) -> both null (= all pages). Single -> the
 *  same page for both bounds. Range -> the two bounds, leaving a blank bound null
 *  for the backend to default (first->1, last->first). Never throws on a half-typed
 *  field: an unparseable number falls back to all so a run is never blocked. */
function readPageSelection() {
  const mode = (document.getElementById("optPagesMode") || {}).value || "all";
  const posInt = (id) => {
    const v = parseInt((document.getElementById(id) || {}).value || "", 10);
    return Number.isFinite(v) && v > 0 ? v : null;
  };
  if (mode === "single") {
    const n = posInt("optPageFrom");
    return n === null ? { firstPage: null, lastPage: null } : { firstPage: n, lastPage: n };
  }
  if (mode === "range") {
    const f = posInt("optPageFrom");
    const t = posInt("optPageTo");
    if (f === null && t === null) return { firstPage: null, lastPage: null };
    return { firstPage: f, lastPage: t };
  }
  return { firstPage: null, lastPage: null };
}

/** Show/hide the page-number inputs based on the Pages mode and keep the second
 *  input (the range "to") visible only for Range. Called once at startup and on
 *  every mode change so the form matches the selected mode. */
function wirePageSelection() {
  const modeEl = document.getElementById("optPagesMode");
  const wrap = document.getElementById("optPagesInputs");
  const toEl = document.getElementById("optPageTo");
  const label = document.getElementById("optPagesInputsLabel");
  const fromEl = document.getElementById("optPageFrom");
  if (!modeEl || !wrap) return;
  const apply = () => {
    const mode = modeEl.value;
    wrap.hidden = mode === "all";
    if (toEl) toEl.hidden = mode !== "range";
    if (label) label.textContent = mode === "range" ? "Pages" : "Page";
    if (fromEl) fromEl.placeholder = mode === "range" ? "from" : "page";
    renderEffectiveSummary();
  };
  // Visibility toggle on mode change; the summary is refreshed by the shared
  // #runOpts input/change listeners wired in DOMContentLoaded.
  modeEl.addEventListener("change", apply);
  apply();
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
  // Page span: blank = all. "5" for single, "5-9" for range, "5-end" when only the
  // first bound is set. Mirrors the firstPage/lastPage the run will send.
  let pagesNote = "";
  if (opts.firstPage != null || opts.lastPage != null) {
    const f = opts.firstPage != null ? opts.firstPage : 1;
    if (opts.lastPage != null && opts.lastPage !== f) pagesNote = " · pages " + f + "-" + opts.lastPage;
    else if (opts.lastPage == null) pagesNote = " · pages " + f + "-end";
    else pagesNote = " · page " + f;
  }
  vals.textContent =
    opts.quant +
    " · " + opts.dpi + " DPI · " + opts.maxTokens + " tok · " +
    "keep images " + (opts.keepImages ? "on" : "off") +
    pagesNote +
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
      repeatPenalty: opts.repeatPenalty,
      firstPage: opts.firstPage,
      lastPage: opts.lastPage,
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
    // Surface completion: a momentary toast + a persisted bell notification.
    const stem = (splitPath(path) || {}).name || path;
    showToast("run:" + path, {
      kind: "done",
      title: stem + " — OCR complete",
      meta: outPath || "",
    });
    removeToast("run:" + path, 5000);
    addNotification("done", stem + " — OCR complete", outPath || "");
    return true;
  } catch (err) {
    // User-initiated stop is not a failure. The backend killed the local server
    // and dropped the model, so refresh the gate (Run -> "Load a model first").
    const wasStopped = String(err).trim() === "stopped";
    if (ui) {
      ui.setRunning(false);
      if (wasStopped) {
        ui.setStatus("stopped");
      } else {
        ui.fail(String(err));
      }
    }
    if (wasStopped) {
      // Drop the provisional half-page <pre> left by the interrupted page so the
      // transcript does not keep dangling partial output (no ocr://done fires on
      // a stopped run).
      if (ui) ui.clearPartial();
      await refreshModelStatus(ui);
      // Record as "failed" (the Board/Library buckets only know done/failed/queued;
      // "stopped" would mis-bucket to queued). The error text carries the reason.
      await recordRunOutcome(path, opts, "failed", "", "stopped by user", library, board);
      const stem = (splitPath(path) || {}).name || path;
      showToast("run:" + path, { kind: "info", title: stem + " — stopped", meta: "reload the model to run again" });
      removeToast("run:" + path, 6000);
      addNotification("info", stem + " — OCR stopped", "Stopped by user; reload the model to run again.");
      // Sentinel so a batch loop can stop dispatching the remaining files (the
      // model was dropped; they would all fail "load a model first").
      return "stopped";
    }
    // EH-0006: record the failed run too so the Library and Board show it as a
    // failed card (not silently dropped). The user already saw the error in the UI.
    await recordRunOutcome(path, opts, "failed", "", String(err), library, board);
    const stem = (splitPath(path) || {}).name || path;
    showToast("run:" + path, {
      kind: "error",
      title: stem + " — OCR failed",
      meta: String(err).slice(0, 140),
    });
    removeToast("run:" + path, 8000);
    addNotification("error", stem + " — OCR failed", String(err));
    return false;
  }
}

/** Wire the Run button: validate queued path list, then run each sequentially.
 *  EH-0004 bite 2: on success the written {stem}.md (path returned by run_ocr) is
 *  fetched via read_text_file and rendered into the read-only Markdown review pane.
 *  EH-0006: on completion (success or failure) the outcome is recorded to the job
 *  store via record_job so it appears in the Library grid and on the Board; the
 *  record call is best-effort and never rolls back a delivered OCR result.
 *  EH-0012: getQueuedPaths() returns the current queued-file list so the button
 *  processes all imported files, not just the typed-path field. */
function wireRunButton(ui, mdPane, unlistensRef, library, board, getQueuedPaths) {
  const runBtn = document.getElementById("runBtn");
  const pathInput = document.getElementById("pdfPath");
  if (!runBtn) return;

  runBtn.addEventListener("click", () => {
    // Prefer the multi-file queue; fall back to the typed-path field for
    // single-file entry without the picker.
    const queued = typeof getQueuedPaths === "function" ? getQueuedPaths() : [];
    const fallback = (pathInput && pathInput.value || "").trim();
    const paths = queued.length > 0 ? queued : fallback ? [fallback] : [];
    if (paths.length === 0) {
      ui.fail("import or type a PDF path first");
      return;
    }
    // Fire-and-forget: the click handler cannot await without holding the event.
    // runOcrOnPath owns UI state transitions + error surfacing per file.
    (async () => {
      // Capture the real setStatus once, before any patching, so a rejection in
      // one file cannot leave a patched function that the next iteration would
      // then capture and double-prefix.
      const originalSetStatus = ui.setStatus.bind(ui);
      for (let i = 0; i < paths.length; i++) {
        const path = paths[i];
        // Per-file status prefix so the user knows which file is running when
        // multiple are queued (single-file batches show the same "1/1: name").
        const prefix = paths.length > 1
          ? "[" + (i + 1) + "/" + paths.length + "] " + jobBaseName(path) + " — "
          : "";
        // Patch ui.setStatus to prepend the per-file prefix while this file runs.
        // try/finally so the original is always restored, even if runOcrOnPath
        // rejects (its pre-try teardown/subscribe can throw) — otherwise the
        // patch leaks past the loop and into later files.
        ui.setStatus = (text) => originalSetStatus(prefix + text);
        let r;
        try {
          r = await runOcrOnPath(path, ui, mdPane, unlistensRef, library, board);
        } finally {
          ui.setStatus = originalSetStatus;
        }
        // A user Stop drops the model; remaining queued files would all fail
        // "load a model first", so halt the batch instead of spamming errors.
        if (r === "stopped") break;
      }
    })();
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
        const r = await runOcrOnPath(pdf, ui, mdPane, unlistensRef, library, board);
        // User Stop dropped the model; halt the rest of the dropped batch.
        if (r === "stopped") break;
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

// Backend presets. llamacpp = managed-local spawn (Quant control drives it, no
// URL); vllm/sglang/custom are remote OpenAI-compatible endpoints. Non-custom
// presets keep the URL/key/model fields hidden; vllm/sglang prefill #remoteUrl
// with the backend's default port (base URL only -- the backend appends
// /v1/chat/completions, so no /v1 suffix here). load_model reads mode + the
// (possibly prefilled) #remoteUrl, so presets need no extra plumbing.
const ENGINE_PRESETS = {
  llamacpp: { mode: "local", url: null },
  vllm: { mode: "remote", url: "http://127.0.0.1:8000" },
  sglang: { mode: "remote", url: "http://127.0.0.1:30000" },
  custom: { mode: "remote", url: null },
};

/** Apply a backend preset: toggle the remote field visibility (editable only for
 *  Custom), prefill the URL for vllm/sglang, and hide the Quant control for any
 *  remote backend (quant only applies to the managed-local spawn). */
function applyPreset(name) {
  const p = ENGINE_PRESETS[name] || ENGINE_PRESETS.llamacpp;
  const remoteFields = document.getElementById("remoteFields");
  if (remoteFields) remoteFields.hidden = name !== "custom";
  if (p.url) {
    const url = document.getElementById("remoteUrl");
    if (url) url.value = p.url;
  }
  const quantEl = document.getElementById("optQuant");
  const quantField = quantEl && quantEl.closest(".opts__field");
  if (quantField) quantField.hidden = p.mode !== "local";
}

/** Wire the OCR engine backend preset dropdown. Changing it re-applies the preset
 *  (field visibility + URL prefill). The selected preset's mode is read by the
 *  Load button to pick local vs remote. */
function wireEnginePreset() {
  const sel = document.getElementById("enginePreset");
  if (!sel) return;
  sel.addEventListener("change", () => applyPreset(sel.value));
  applyPreset(sel.value);
}

/** Return the active backend's mode ("local" | "remote"). */
function activeEngineMode() {
  const sel = document.getElementById("enginePreset");
  const name = sel ? sel.value : "llamacpp";
  return (ENGINE_PRESETS[name] || ENGINE_PRESETS.llamacpp).mode;
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
      const modelEl = document.getElementById("remoteModel");
      // Startup-only DeepSeek-OCR knobs read at load time (they parameterize the
      // llama-server spawn). Blank/invalid -> null so the backend omits the flag.
      const imtEl = document.getElementById("optImageMaxTokens");
      const ctEl = document.getElementById("optChatTemplate");
      const imtVal = imtEl ? parseInt(imtEl.value || "", 10) : NaN;
      loadBtn.disabled = true;
      if (unloadBtn) unloadBtn.disabled = true;
      if (statusText) statusText.textContent = "loading…";
      try {
        const status = await t.core.invoke("load_model", {
          mode,
          quant: quantEl ? quantEl.value : null,
          baseUrl: urlEl ? urlEl.value : null,
          apiKey: keyEl ? keyEl.value : null,
          model: modelEl ? modelEl.value : null,
          llamaBin: null,
          imageMaxTokens: Number.isFinite(imtVal) && imtVal > 0 ? imtVal : null,
          chatTemplate: ctEl && ctEl.value ? ctEl.value : null,
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

/** Map a raw quant value to the human-readable tier label used in the select.
 *  Matches the CLI tier semantics: best=BF16, good=Q8_0, less=Q4_K_M.
 *  Unknown quants fall back to the raw value so future quants degrade gracefully. */
function quantTierLabel(quant) {
  const TIERS = { BF16: "best", Q8_0: "good", Q4_K_M: "less" };
  const tier = TIERS[quant];
  return tier ? tier + " (" + quant + ")" : quant;
}

/** Mark which quant options are already cached on disk (list_local_models) by
 *  appending a check to their label. Applies to both the run-time Quant select and
 *  the Settings default-quant select. Best-effort; never throws. Preserves the tier
 *  label prefix so the cached marker appends to "good (Q8_0)", not just "Q8_0". */
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
      const label = quantTierLabel(base);
      opt.textContent = set.has(base) ? label + " ✓ cached" : label;
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
  setVal("remoteModel", s.remoteModel);
  // Select the backend preset matching the saved mode. We can't tell which remote
  // backend was saved (settings only stores mode + URL), so remote -> Custom, which
  // shows the fields and preserves the #remoteUrl restored by setVal above.
  const sel = document.getElementById("enginePreset");
  if (sel) {
    sel.value = (s.mode || "local") === "remote" ? "custom" : "llamacpp";
    applyPreset(sel.value);
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
    remoteModel: "setRemoteModel",
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
        remoteModel: (get(ids.remoteModel) && get(ids.remoteModel).value) || "",
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

/** Wire the Settings panel's Model cache section: load the cache path + GGUF
 *  size via get_cache_info, and wire the Clear button to clear_model_cache.
 *  Called once on startup (the Settings view exists in the DOM at load time).
 *  Fail-soft outside the webview. */
async function wireCacheControls() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const dirEl = document.getElementById("cacheDir");
  const sizeEl = document.getElementById("cacheSizeBytes");
  const clearBtn = document.getElementById("clearCacheBtn");
  const statusEl = document.getElementById("clearCacheStatus");

  /** Format bytes to a human-readable string (MiB for GB-scale GGUFs). */
  function fmtBytes(n) {
    if (n <= 0) return "0 B";
    if (n < 1024) return n + " B";
    if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KiB";
    if (n < 1024 * 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + " MiB";
    return (n / (1024 * 1024 * 1024)).toFixed(2) + " GiB";
  }

  /** Refresh the path + size display from the backend. */
  async function refreshCacheInfo() {
    try {
      const info = await t.core.invoke("get_cache_info");
      if (dirEl) dirEl.textContent = (info && info.path) || "—";
      if (sizeEl) sizeEl.textContent = info ? fmtBytes(Number(info.sizeBytes) || 0) : "";
    } catch (err) {
      if (dirEl) dirEl.textContent = "unavailable";
      if (sizeEl) sizeEl.textContent = "";
    }
  }

  await refreshCacheInfo();

  if (clearBtn) {
    clearBtn.addEventListener("click", async () => {
      clearBtn.disabled = true;
      if (statusEl) { statusEl.hidden = false; statusEl.textContent = "clearing…"; }
      try {
        await t.core.invoke("clear_model_cache");
        if (statusEl) statusEl.textContent = "Cache cleared.";
        await refreshCacheInfo();
        // Re-mark cached quants (all gone after a clear).
        markCachedQuants();
      } catch (err) {
        if (statusEl) statusEl.textContent = "Error: " + String(err);
      } finally {
        clearBtn.disabled = false;
        if (statusEl) {
          setTimeout(() => { if (statusEl) statusEl.hidden = true; }, 3000);
        }
      }
    });
  }
}

// --- notifications + toasts -------------------------------------------------
//
// Two surfaces: transient TOASTS (bottom-right #toastStack) for live download
// progress and momentary done/failed flashes, and a persisted PANEL (the bell,
// #notifyPanel) backed by notifications.json via the add/list/clear commands.
// Toasts are pure DOM; the panel round-trips through Tauri. All user-supplied
// text (filenames, error messages, output paths) is set via textContent, never
// innerHTML, so a hostile path/error string cannot inject markup.

/** Compact human byte size, e.g. 1503238553 -> "1.4 GB". */
function fmtBytes(n) {
  if (!n || n < 0) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) {
    n /= 1024;
    i += 1;
  }
  return (i === 0 ? n : n.toFixed(1)) + " " + u[i];
}

/** Relative age of a unix-seconds timestamp, e.g. "3m ago". */
function relTime(secs) {
  const now = Math.floor(Date.now() / 1000);
  const d = Math.max(0, now - (secs || 0));
  if (d < 60) return d + "s ago";
  if (d < 3600) return Math.floor(d / 60) + "m ago";
  if (d < 86400) return Math.floor(d / 3600) + "h ago";
  return Math.floor(d / 86400) + "d ago";
}

/** Create or update a toast by id (same id = update in place, used for live
 *  download progress). opts: {title, kind, meta, fill}. fill is 0..100 to show a
 *  progress bar, omitted for a plain notice. */
function showToast(id, opts) {
  const stack = document.getElementById("toastStack");
  if (!stack) return null;
  let el = stack.querySelector('[data-toast="' + id + '"]');
  if (!el) {
    el = document.createElement("div");
    el.dataset.toast = id;
    el.innerHTML =
      '<div class="toast__title"></div>' +
      '<div class="toast__meta"></div>' +
      '<div class="toast__bar" hidden><div class="toast__fill"></div></div>';
    stack.appendChild(el);
  }
  el.className = "toast" + (opts.kind ? " toast--" + opts.kind : "");
  el.querySelector(".toast__title").textContent = opts.title || "";
  const meta = el.querySelector(".toast__meta");
  meta.textContent = opts.meta || "";
  meta.hidden = !opts.meta;
  const bar = el.querySelector(".toast__bar");
  if (typeof opts.fill === "number") {
    bar.hidden = false;
    el.querySelector(".toast__fill").style.width =
      Math.max(0, Math.min(100, opts.fill)) + "%";
  } else {
    bar.hidden = true;
  }
  return el;
}

/** Remove a toast by id, optionally after `delay` ms (lets a completed toast
 *  linger briefly before fading out). */
function removeToast(id, delay) {
  const stack = document.getElementById("toastStack");
  if (!stack) return;
  const el = stack.querySelector('[data-toast="' + id + '"]');
  if (!el) return;
  if (delay) setTimeout(() => el.remove(), delay);
  else el.remove();
}

// Per-file download state for speed (bytes/sec) derived from successive events,
// plus a flag so we only record a "model ready" notification when a download
// actually happened this load (server-ready also fires on every plain run).
const dlSpeed = new Map();
let dlHappened = false;

/** Add a persisted notification (best-effort). Refreshes the bell badge. Never
 *  throws into the caller: outside the webview or on a store error it just no-ops. */
async function addNotification(kind, title, body) {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  try {
    await t.core.invoke("add_notification", { kind, title, body: body || "" });
  } catch (err) {
    return;
  }
  refreshNotifyPanel();
}

/** Reload the notification list into the panel and update the unread badge.
 *  Best-effort; silent outside the webview. */
async function refreshNotifyPanel() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  let list = [];
  try {
    list = await t.core.invoke("list_notifications");
  } catch (err) {
    return;
  }
  const badge = document.getElementById("notifyBadge");
  if (badge) {
    const unread = list.filter((n) => !n.read).length;
    badge.textContent = String(unread);
    badge.hidden = unread === 0;
  }
  const listEl = document.getElementById("notifyList");
  if (!listEl) return;
  if (list.length === 0) {
    listEl.innerHTML = '<p class="notify-panel__empty">No notifications.</p>';
    return;
  }
  listEl.innerHTML = "";
  // Newest first.
  list
    .slice()
    .reverse()
    .forEach((n) => {
      const item = document.createElement("div");
      item.className =
        "notify-item notify-item--" + (n.kind || "info") + (n.read ? "" : " is-unread");

      const title = document.createElement("div");
      title.className = "notify-item__title";
      title.textContent = n.title || "";
      item.appendChild(title);

      if (n.body) {
        const bodyEl = document.createElement("div");
        bodyEl.className = "notify-item__body";
        bodyEl.textContent = n.body;
        item.appendChild(bodyEl);
      }

      const time = document.createElement("div");
      time.className = "notify-item__time";
      time.textContent = relTime(n.createdAt);
      item.appendChild(time);

      const x = document.createElement("button");
      x.className = "notify-item__x";
      x.type = "button";
      x.title = "Dismiss";
      x.textContent = "×";
      x.addEventListener("click", async (ev) => {
        ev.stopPropagation();
        try {
          await t.core.invoke("clear_notification", { id: n.id });
        } catch (err) {
          /* ignore */
        }
        refreshNotifyPanel();
      });
      item.appendChild(x);

      listEl.appendChild(item);
    });
}

/** Live download toasts: one per file, pct + size + MB/s, removed when complete.
 *  Records a single "Model ready" notification once a download finishes. */
function wireDownloadToasts(t) {
  t.event.listen("ocr://progress", (e) => {
    const { name, pct, done, total } = (e && e.payload) || {};
    const key = name || "model";
    const id = "dl:" + key;
    dlHappened = true;

    let speedStr = "";
    if (typeof done === "number") {
      const now = Date.now();
      const prev = dlSpeed.get(key);
      if (prev && now > prev.time) {
        const bps = ((done - prev.done) * 1000) / (now - prev.time);
        if (bps > 0) speedStr = fmtBytes(bps) + "/s";
      }
      dlSpeed.set(key, { done, time: now });
    }
    const sizeStr =
      total > 0 ? fmtBytes(done) + " / " + fmtBytes(total) : fmtBytes(done || 0);
    showToast(id, {
      kind: "download",
      title: "Downloading " + key,
      meta:
        (pct != null ? pct + "%  ·  " : "") +
        sizeStr +
        (speedStr ? "  ·  " + speedStr : ""),
      fill: typeof pct === "number" ? pct : undefined,
    });
    if (pct >= 100) {
      dlSpeed.delete(key);
      removeToast(id, 1500);
    }
  });

  t.event.listen("ocr://server-ready", () => {
    // All files present and the server is up. Clear any lingering download toasts
    // and, if a download actually ran this load, record one completion notice.
    const stack = document.getElementById("toastStack");
    if (stack) {
      stack
        .querySelectorAll('[data-toast^="dl:"]')
        .forEach((el) => el.remove());
    }
    if (dlHappened) {
      dlHappened = false;
      addNotification("download", "Model download complete", "");
    }
  });
}

/** Wire the bell (toggle panel, mark-read on open, click-outside close), the
 *  Clear-all button, and the download toasts. Silent outside the webview. */
function initNotifications() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const bell = document.getElementById("notifyBell");
  const panel = document.getElementById("notifyPanel");
  const clearAll = document.getElementById("notifyClearAll");

  if (bell && panel) {
    bell.addEventListener("click", async (e) => {
      e.stopPropagation();
      const opening = panel.hidden;
      panel.hidden = !opening;
      bell.setAttribute("aria-expanded", String(opening));
      if (opening) {
        await refreshNotifyPanel();
        // Mark read so the badge clears, then re-render to drop unread styling.
        try {
          await t.core.invoke("mark_notifications_read");
        } catch (err) {
          /* ignore */
        }
        refreshNotifyPanel();
      }
    });
    document.addEventListener("click", (e) => {
      if (panel.hidden) return;
      if (e.target === bell || bell.contains(e.target) || panel.contains(e.target)) return;
      panel.hidden = true;
      bell.setAttribute("aria-expanded", "false");
    });
  }
  if (clearAll) {
    clearAll.addEventListener("click", async () => {
      try {
        await t.core.invoke("clear_all_notifications");
      } catch (err) {
        /* ignore */
      }
      refreshNotifyPanel();
    });
  }

  wireDownloadToasts(t);
  refreshNotifyPanel(); // seed the badge from the persisted store on launch
}

window.addEventListener("DOMContentLoaded", () => {
  const library = makeLibrary();
  const board = makeBoard();
  wireRail(library, board);

  const ui = makeUi();
  const rail = makeFileRail();
  const mdPane = makeMarkdownPane();
  const unlistensRef = { value: [] };

  // EH-0015: wire the review-pane re-open affordance. Done job cards in the
  // Library become clickable; clicking switches to the Review view and loads
  // the card's .md. Rail buttons are needed so openInReview can update is-active.
  const railButtons = document.querySelectorAll(".rail__btn");
  library.setReviewHooks(mdPane, railButtons);
  // EH-0012: canonical queue of PDF paths to process on Run. The Import picker
  // populates this; the path-input field seeds it too (single-file typed entry).
  // wireRunButton reads this via getQueuedPaths() so all imported files run
  // sequentially on one click instead of only the last typed path.
  let queuedPaths = [];
  const getQueuedPaths = () => queuedPaths.slice();
  wireRunButton(ui, mdPane, unlistensRef, library, board, getQueuedPaths);
  // EH-0006 bite 4: drag-drop PDF import onto the Library grid. Wired once on app
  // load; the listeners live for the app lifetime and are scoped to the Library
  // view inside the handler. Fail-soft outside the webview (plain browser).
  wireLibraryDrop(ui, mdPane, unlistensRef, library, board);

  // Model load/remote wiring: engine tabs (local/remote), the Load/Unload bar,
  // the app-lifetime load-progress listeners, and the settings panel. Load
  // settings first so saved defaults seed the controls, then mark which quants are
  // cached, then read the live model status to set the Run gate + badge.
  wireEnginePreset();
  wireModelBar(ui);
  attachLoadListeners();
  wireSettings(() => {
    markCachedQuants();
  });
  markCachedQuants();
  wireCacheControls();
  refreshModelStatus(ui);
  // Notification bell + panel + download toasts. Self-contained; silent in a
  // plain browser (no Tauri). Seeds the unread badge from the persisted store.
  initNotifications();

  // EH-0004 bite 2 / EH-0012: the file list pane is bound to the queued-path
  // list. The Import button opens a MULTI-select picker; each chosen PDF is
  // added to queuedPaths and rendered in the file-rail. The path-input field
  // provides single-file typed/pasted entry (adds one path on change). The Run
  // button processes queuedPaths in order, with per-file status.
  const pathInput = document.getElementById("pdfPath");
  const importBtn = document.getElementById("importBtn");
  const preview = makePreviewPane();

  // Apply queuedPaths to the file-rail display and clear the text field
  // (the canonical list is in queuedPaths, not the field, for multi-file batches).
  function applyQueue(paths) {
    queuedPaths = paths.slice();
    rail.renderFiles(queuedPaths);
    // Show the first file in the path field for context; for multi-file batches
    // this is the first item only (the rail shows the full list).
    if (pathInput) pathInput.value = queuedPaths.length === 1 ? queuedPaths[0] : "";
  }

  if (pathInput) {
    const syncFromField = () => {
      const v = (pathInput.value || "").trim();
      // Typed path replaces the entire queue (single-file typed entry).
      queuedPaths = v ? [v] : [];
      rail.renderFiles(queuedPaths);
    };
    // Preview render shells out to pdftoppm; only refresh on blur/Enter/change,
    // not per keystroke.
    const refreshPreview = () => preview.show((pathInput.value || "").trim());
    pathInput.addEventListener("input", syncFromField);
    pathInput.addEventListener("change", syncFromField);
    pathInput.addEventListener("change", refreshPreview);

    // Import opens the native multi-select file picker (tauri-plugin-dialog,
    // exposed at window.__TAURI__.dialog via withGlobalTauri). The picker result
    // is a string (single) or string[] (multiple) for multi:true.
    // EH-0012: `multiple: true` so the user can pick several PDFs at once; all
    // are added to queuedPaths and shown in the file-rail.
    if (importBtn) {
      importBtn.addEventListener("click", async () => {
        const dialog = window.__TAURI__ && window.__TAURI__.dialog;
        if (!dialog || !dialog.open) {
          pathInput.focus();
          return;
        }
        try {
          const selected = await dialog.open({
            multiple: true,
            directory: false,
            filters: [{ name: "PDF", extensions: ["pdf"] }],
          });
          // selected is null (cancelled), string (single), or string[] (multiple).
          if (!selected) return;
          const picked = Array.isArray(selected) ? selected : [selected];
          const pdfs = picked.filter((p) => typeof p === "string" && p.trim());
          if (pdfs.length === 0) return;
          applyQueue(pdfs);
          // Preview the first file; multi-file batches show page 1 of the first PDF.
          preview.show(pdfs[0]);
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

  // Task preset -> fill the Prompt box. Keep these strings in sync with the CLI's
  // Task::prompt() (src/main.rs). "custom" leaves whatever the user typed.
  const TASK_PROMPTS = {
    markdown: "<|grounding|>Convert the document to markdown.",
    free: "Free OCR.",
    figure: "Parse the figure.",
  };
  const taskEl = document.getElementById("optTask");
  const promptEl = document.getElementById("optPrompt");
  if (taskEl && promptEl) {
    taskEl.addEventListener("change", () => {
      const preset = TASK_PROMPTS[taskEl.value];
      if (preset) {
        promptEl.value = preset;
        renderEffectiveSummary();
      }
    });
    // A manual prompt edit means the box no longer matches a preset: flip to Custom
    // so the dropdown does not falsely claim a preset is active.
    promptEl.addEventListener("input", () => {
      const match = Object.keys(TASK_PROMPTS).find((k) => TASK_PROMPTS[k] === promptEl.value);
      taskEl.value = match || "custom";
    });
  }

  // Surface the Q4_K_M loop caveat only when that quant is selected.
  const quantEl = document.getElementById("optQuant");
  const quantHint = document.getElementById("quantHint");
  if (quantEl && quantHint) {
    const syncHint = () => {
      quantHint.hidden = quantEl.value !== "Q4_K_M";
    };
    quantEl.addEventListener("change", syncHint);
    syncHint();
  }

  // Page-selection mode -> show/hide the from/to inputs.
  wirePageSelection();

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

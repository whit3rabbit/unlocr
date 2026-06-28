// Workspace pane controllers: the file rail (queued files + pipeline stages), the
// read-only Markdown review pane, and the center PDF preview. Each owns its own
// DOM subtree; none touch the transcript/progress UI (that lives in ui.js).

import { requireTauri } from "./tauri.js";
import { splitPath } from "./paths.js";

/** Controller over the Workspace file-rail panes (EH-0004 bite 2): the file
 *  list and the OCR pipeline stages. Both are bound to live state, not static
 *  placeholders:
 *    - file list   <- the input queued for the next run (path field + Import btn)
 *    - pipeline    <- the preflight report (tools/model/mmproj flags) */
export function makeFileRail() {
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
export function makeMarkdownPane() {
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
export function makePreviewPane() {
  const panel = document.querySelector(".panel.preview");
  if (!panel) return { show() {}, clear() {} };
  const stage = panel.querySelector(".preview__stage");
  const pageChip = panel.querySelector(".chip--soft");
  const pageCount = panel.querySelector(".preview__pagecount");
  const prevBtn = panel.querySelector("#prevPage");
  const nextBtn = panel.querySelector("#nextPage");
  // Lazy per-page model: render only the page being viewed (on demand, as the user
  // navigates), not every page up front. Importing a 300-page book no longer
  // rasterizes 300 PNGs when the user may never leave page 1. (No look-ahead
  // prefetch: each page is rendered when first shown, then cached.)
  let pdfPath = null; // current PDF; null when cleared
  let cache = {}; // 1-based page number -> asset URL (string), or null if out of range
  let idx = 1; // current page (1-based)
  let lastPage = null; // known last page once an out-of-range render is hit; null = unknown
  let token = 0; // bumps on each show()/clear() so stale async renders are dropped

  // Render+cache one page (1-based). Returns its asset URL, or null when the page is
  // out of range (the backend render_page Errs past the last page). Never throws.
  async function fetchPage(t, n) {
    if (n in cache) return cache[n];
    try {
      const p = await t.core.invoke("render_page", { pdfPath, page: n, dpi: null });
      cache[n] = t.core.convertFileSrc(p);
    } catch (err) {
      // Only an out-of-range page marks the end of the document: cache the null and
      // bound navigation. A REAL render failure (pdftoppm error, transient IPC) is
      // NOT cached, so a later navigation retries instead of permanently truncating
      // the preview at a page that would render fine on a second try.
      if (String(err).includes("out of range")) {
        cache[n] = null;
        if (lastPage === null || n - 1 < lastPage) lastPage = Math.max(0, n - 1);
      } else {
        return undefined; // transient: leave uncached so the next attempt retries
      }
    }
    return cache[n];
  }

  function paint(errorMsg) {
    if (!stage) return;
    stage.innerHTML = "";
    const url = pdfPath ? cache[idx] : null;
    if (url == null) {
      const p = document.createElement("p");
      p.className = "placeholder";
      p.textContent = errorMsg || "Import a PDF to see a page preview here.";
      stage.appendChild(p);
      if (pageChip) pageChip.textContent = "Page 0";
      if (pageCount) pageCount.textContent = "page 0 / 0";
      if (prevBtn) prevBtn.disabled = true;
      if (nextBtn) nextBtn.disabled = true;
      return;
    }
    const img = document.createElement("img");
    img.className = "preview__img";
    img.src = url;
    img.alt = "PDF page " + idx;
    // Click advances when a next page may exist (unknown end, or before the last).
    if (lastPage === null || idx < lastPage) {
      img.title = "click for next page";
      img.style.cursor = "pointer";
      img.addEventListener("click", () => go(1));
    }
    stage.appendChild(img);
    if (pageChip) pageChip.textContent = "Page " + idx;
    // Total is unknown until the user reaches the end (no separate page-count probe);
    // show "page N" until then, "page N / total" once discovered.
    if (pageCount) {
      pageCount.textContent = lastPage !== null ? "page " + idx + " / " + lastPage : "page " + idx;
    }
    if (prevBtn) prevBtn.disabled = idx <= 1;
    if (nextBtn) nextBtn.disabled = lastPage !== null && idx >= lastPage;
  }

  // Move by delta (1-based, no wrap), rendering the target page on demand.
  async function go(delta) {
    if (!pdfPath) return;
    const target = idx + delta;
    if (target < 1) return;
    if (lastPage !== null && target > lastPage) return;
    let t;
    try {
      t = requireTauri();
    } catch (err) {
      return;
    }
    const my = token;
    const url = await fetchPage(t, target);
    if (my !== token) return; // a newer show()/clear() superseded this render
    if (url == null) {
      paint(); // hit the end; nextBtn disables via lastPage
      return;
    }
    idx = target;
    paint();
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
    token++;
    pdfPath = null;
    cache = {};
    idx = 1;
    lastPage = null;
    paint();
  }

  /** Render the first page of one PDF; later pages load lazily on navigation. Non-PDF
   *  or empty clears the pane. Fails soft outside the webview. */
  async function show(path) {
    let t;
    try {
      t = requireTauri();
    } catch (err) {
      return; // plain browser: no backend render
    }
    token++;
    const my = token;
    if (!path || !path.toLowerCase().endsWith(".pdf")) {
      clear();
      return;
    }
    pdfPath = path;
    cache = {};
    idx = 1;
    lastPage = null;
    const url = await fetchPage(t, 1);
    if (my !== token) return; // superseded by a newer show()/clear()
    paint(url == null ? "Preview render failed." : undefined);
  }

  return { show, clear };
}

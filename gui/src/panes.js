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

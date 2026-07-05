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
  // Last report rendered, so a locale switch can re-render the imperatively-set
  // (t()-composed) engine label + tooltip below, which no data-i18n walk covers.
  let lastReport = null;

  /** Render the file list from the queued input(s). Empty hides the row and
   *  shows the placeholder; one entry shows a single queued-file row.
   *  `onRemove(path)` (optional) adds a per-row remove button that hands back the
   *  ORIGINAL queue path (not the splitPath name/dir) so the caller can drop the
   *  exact queue entry. */
  function renderFiles(paths, onRemove) {
    const orig = (paths || []).filter((p) => typeof p === "string" && p.trim());
    if (count) count.textContent = String(orig.length);
    // Remove every previously rendered .file-row (keep the #fileListEmpty node).
    if (list) list.querySelectorAll(".file-row").forEach((n) => n.remove());
    if (orig.length === 0) {
      if (empty) empty.hidden = false;
      return;
    }
    if (empty) empty.hidden = true;
    for (const full of orig) {
      const item = splitPath(full);
      if (!item) continue;
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
      if (typeof onRemove === "function") {
        const rm = document.createElement("button");
        rm.type = "button";
        rm.className = "file-row__remove";
        rm.textContent = "×";
        rm.title = "Remove from queue";
        rm.setAttribute("aria-label", "Remove " + item.name + " from queue");
        rm.addEventListener("click", () => onRemove(full));
        row.appendChild(rm);
      }
      if (list) list.appendChild(row);
    }
  }

  /** Render the OCR pipeline stages from a preflight report. Each stage lights
   *  up from the report's flags: tools/model/mmproj. ok => green, missing =>
   *  red, partial (build-too-old / only one tool) => amber. Unknown stays dim. */
  function renderPipeline(report) {
    lastReport = report;
    // An external (PATH/Homebrew/override) llama-server cannot be verified for the
    // R-SWA vision patch (PR #24975) and is the usual cause of the ocr-ocr
    // repetition loops; flag it. "managed" = unlocr's patched build (trusted).
    const external = !!(report && report.provenance === "external");
    const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);
    if (engineLabel) {
      const quant = (report && report.quant) || "Q8_0";
      let label = "Unlimited-OCR · " + quant;
      if (external) label += " · " + tr("pipeline.externalLlama");
      engineLabel.textContent = label;
      engineLabel.title = external ? tr("pipeline.externalLlamaHint") : "";
    }
    if (!stages) return;
    const ok = !!(report && report.ok);
    const setStage = (key, state) => {
      const li = stages.querySelector('.stage[data-stage="' + key + '"]');
      if (!li) return;
      li.classList.remove("is-ok", "is-warn", "is-bad");
      if (state) li.classList.add("is-" + state);
    };

    // Tools: both llama-server and pdftoppm resolved on a successful report. An
    // external llama-server still resolves, but is amber (unverified R-SWA patch),
    // not green -- the managed build is the only trusted one.
    if (!report) {
      setStage("tools", "");
    } else if (ok && report.llamaServer && report.pdftoppm) {
      setStage("tools", external ? "warn" : "ok");
    } else {
      setStage("tools", "bad");
    }

    // Model GGUF + projector presence come straight off the report.
    setStage("model", report && report.modelPresent ? "ok" : report ? "bad" : "");
    setStage("mmproj", report && report.mmprojPresent ? "ok" : report ? "bad" : "");
  }

  // The engine label + tooltip are set imperatively via t() (not a data-i18n
  // node), so applyText() cannot retranslate them on a locale switch. Re-render
  // the last report so the "external" badge/tooltip flips language immediately.
  if (typeof window !== "undefined" && window.unlocrI18n && window.unlocrI18n.onLocaleChange) {
    window.unlocrI18n.onLocaleChange(() => {
      if (lastReport !== null) renderPipeline(lastReport);
    });
  }

  return { renderFiles, renderPipeline };
}

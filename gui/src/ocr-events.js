// OCR event subscription + the on-load preflight gate. subscribeOcrEvents attaches
// the ocr:// listeners (awaited so attachment is real before run_ocr fires) and
// routes them through the ui controller; preflightOnLoad runs preflight and feeds
// the result to the ui (Run gate) and the file-rail pipeline stages.

import { requireTauri } from "./tauri.js";

/** Run preflight on load. EH-0004 turns this into a GATE: if a required tool
 *  (llama-server or pdftoppm) is missing (report.ok === false), the Run button
 *  is disabled and the structured error is surfaced inline, so the user cannot
 *  start a run that is guaranteed to fail. On ok, Run is enabled. Still logs the
 *  report for the EH-0003 acceptance check. `ui` is optional so a stale
 *  non-Tauri caller (plain browser) can invoke this without a controller. */
export async function preflightOnLoad(ui, rail) {
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
export async function subscribeOcrEvents(ui) {
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

  // Free-form status for a long, event-less phase (rasterizing every page before
  // page 1) so the popup does not look frozen on "starting…". Stays indeterminate.
  handlers.push([
    "ocr://status",
    (e) => {
      const { message } = e.payload || {};
      if (message) ui.setStatus(message);
    },
  ]);

  // llama-server healthy, about to OCR pages.
  handlers.push([
    "ocr://server-ready",
    (e) => {
      const { port } = e.payload || {};
      ui.setStatus(`server ready on :${port}`);
      ui.showProgress(true);
    },
  ]);

  // Rasterizing (PDF->PNG) progress, fired while pdftoppm is still running and
  // before any OCR starts. `total` is null when the page count wasn't known
  // upfront (whole-doc run, no resolvable pdfinfo); show a running count with
  // no denominator in that case.
  handlers.push([
    "ocr://rasterizing",
    (e) => {
      const { page, total } = e.payload || {};
      ui.showProgress(true);
      ui.setStatus(total ? `rasterizing page ${page}/${total}` : `rasterizing page ${page}`);
    },
  ]);

  // Per-page progress: page/total status line.
  handlers.push([
    "ocr://page",
    (e) => {
      const { page, total } = e.payload || {};
      ui.showProgress(true);
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

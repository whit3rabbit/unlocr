// Run flow bindings: drive one PDF through run_ocr end to end (subscribe events, invoke,
// resolve markdown, record outcome), the Run button (sequential batch over the
// queued paths), and the Library drag-drop importer.

import { requireTauri } from "./tauri.js";
import { runOcrOnPath } from "./run_ocr.js";
import { jobBaseName } from "./paths.js";

// Shared across BOTH run entry points (the Run button and the drag-drop importer)
// so a second batch cannot start while one is live. runOcrOnPath shares a single
// unlistensRef; two concurrent batches would race its teardown/subscribe and leak
// duplicate ocr:// listeners (transcript renders twice), plus race the one warm
// llama-server. One in-flight batch at a time.
let runInFlight = false;

/** Wire the Run button: validate queued path list, then run each sequentially.
 *  EH-0004 bite 2: on success the written {stem}.md (path returned by run_ocr) is
 *  fetched via read_text_file and rendered into the read-only Markdown review pane.
 *  EH-0006: on completion (success or failure) the outcome is recorded to the job
 *  store via record_job so it appears in the Library grid and on the Board; the
 *  record call is best-effort and never rolls back a delivered OCR result.
 *  EH-0012: getQueuedPaths() returns the current queued-file list so the button
 *  processes all imported files, not just the typed-path field. */
export function wireRunButton(ui, mdPane, unlistensRef, getQueuedPaths) {
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
    // One batch at a time: a double-click (or the Board's "Run all" proxy) while a
    // run is live would start a second concurrent batch sharing unlistensRef.
    if (runInFlight) {
      ui.setStatus("a run is already in progress");
      return;
    }
    // Fire-and-forget: the click handler cannot await without holding the event.
    // runOcrOnPath owns UI state transitions + error surfacing per file.
    // Output location: folder applies to every file; a custom filename is honored
    // only for a single-file run (the backend rejects out_file with >1 input, and
    // the field is disabled in the UI for batches).
    const outDir = (document.getElementById("outFolder") || {}).value || "";
    const outFile = paths.length === 1 ? (document.getElementById("outFile") || {}).value || "" : "";
    runInFlight = true;
    (async () => {
      // Capture the real setStatus once, before any patching, so a rejection in
      // one file cannot leave a patched function that the next iteration would
      // then capture and double-prefix.
      const originalSetStatus = ui.setStatus.bind(ui);
      try {
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
          r = await runOcrOnPath(path, ui, mdPane, unlistensRef, { dir: outDir, file: outFile });
        } finally {
          ui.setStatus = originalSetStatus;
        }
        // A user Stop drops the model; remaining queued files would all fail
        // "load a model first", so halt the batch instead of spamming errors.
        if (r === "stopped") break;
      }
      } finally {
        runInFlight = false;
      }
    })();
  });
}

/** EH-0006 bite 4: drag-drop PDF import onto the Library grid. Subscribes to the
 *  Tauri drag-drop event channel (tauri://drag-enter / drag-over / drag-leave /
 *  tauri://drag-drop) — the same window.__TAURI__.event.listen the OCR progress events use,
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
export function wireLibraryDrop(ui, mdPane, unlistensRef) {
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
  // In-flight tracking is the module-level `runInFlight` shared with the Run
  // button, so a drop while a button-run is live (or vice versa) is rejected, not
  // raced. The next drop is accepted only after the current batch finishes.

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
    if (runInFlight) {
      // eslint-disable-next-line no-console
      console.warn("[drop] a run is already in progress; ignoring new drop");
      return;
    }
    runInFlight = true;
    try {
      for (const pdf of accepted) {
        // eslint-disable-next-line no-console
        console.log("[drop] enqueuing OCR job:", pdf);
        // Each run is awaited so llama-server is torn down before the next starts.
        // Honor the chosen output folder; dropped imports are batch-shaped, so no
        // custom filename (each writes {stem}.md into the folder / beside input).
        const outDir = (document.getElementById("outFolder") || {}).value || "";
        const r = await runOcrOnPath(pdf, ui, mdPane, unlistensRef, { dir: outDir, file: null });
        // User Stop dropped the model; halt the rest of the dropped batch.
        if (r === "stopped") break;
      }
    } finally {
      runInFlight = false;
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

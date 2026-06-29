// Run flow: drive one PDF through run_ocr end to end (subscribe events, invoke,
// resolve markdown, record outcome), the Run button (sequential batch over the
// queued paths), and the Library drag-drop importer. Shared runOcrOnPath keeps the
// button path and the drop path on one code path + one store record.

import { requireTauri } from "./tauri.js";
import { subscribeOcrEvents } from "./ocr-events.js";
import { readRunOptions } from "./options.js";
import { refreshModelStatus } from "./model.js";
import { parentDirOf, splitPath, jobBaseName } from "./paths.js";
import { showToast, removeToast, addNotification } from "./toasts.js";

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
 *  future background importer can reuse the path without a progress surface).
 *
 *  The job store record (running -> done/failed) is owned by the backend `run_ocr`
 *  loop now; it emits `jobs://changed` and the Library/Board reload live (the
 *  listener is wired once in main.js). This path only drives the transcript UI,
 *  the review pane, and the toast/bell notifications. */
export async function runOcrOnPath(path, ui, mdPane, unlistensRef, outOverride) {
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
    // out_dir = the chosen output folder, else the input's parent dir so run_ocr
    // writes {stem}.md next to the source (mirrors the CLI default of
    // output-beside-input) and returns the written path. inputs is a Vec for
    // forward-compat with batch runs.
    // EH-0005 bite 1: the engine/options controls (quant/dpi/maxTokens/keepImages)
    // are forwarded into the run_ocr payload so the GUI drives quality, DPI, and
    // image-keeping instead of always sending CLI defaults.
    // A bare filename has no parent dir; fall back to "." (cwd) so run_ocr always
    // writes a file beside the input rather than silently flipping to in-memory
    // mode (empty out_dir) and writing nothing to disk.
    const ov = outOverride || {};
    const outDir = (ov.dir || "").trim() || parentDirOf(path) || ".";

    // Show/confirm the resolved output directory
    showToast("run-dir-info:" + path, {
      kind: "info",
      title: "Saving output to:",
      meta: outDir === "." ? "Current working directory (.)" : outDir
    });
    removeToast("run-dir-info:" + path, 4000);

    // out_file: single-file custom name (null for batch; the caller gates this).
    const outFile = (ov.file || "").trim() || null;
    // quant is fixed at load time (the model is already held warm); run_ocr only
    // takes the per-run options below.
    const results = await t.core.invoke("run_ocr", {
      inputs: [path],
      outDir,
      outFile,
      maxTokens: opts.maxTokens,
      dpi: opts.dpi,
      prompt: opts.prompt,
      keepImages: opts.keepImages,
      repeatPenalty: opts.repeatPenalty,
      firstPage: opts.firstPage,
      lastPage: opts.lastPage,
      // Informational: recorded on the backend-written job row so the
      // Library/Board show the quant the run used (the model is already warm).
      quant: opts.quant,
    });

    // run_ocr always returns WRITTEN FILE PATHS here: outDir is never empty (it
    // falls back to the input's parent dir, else "."), so the backend always writes
    // {stem}.md and returns its path. results[0] is therefore a path to read via
    // read_text_file, never inline markdown. The transcript pane is driven solely by
    // the ocr://done event (its listener is attached before invoke); the review pane
    // is populated from the on-disk file below.
    let resolvedMd = "";
    let mdPath = "";
    let readError = null;
    if (results && results.length) {
      mdPath = results[0];
      try {
        // The backend authorizes reads from its own record of files run_ocr just
        // wrote (AppState.read_allow); no client-supplied allowlist is needed.
        resolvedMd = await t.core.invoke("read_text_file", { path: mdPath });
      } catch (readErr) {
        // File read failed (rare: written then removed). Surface in the review
        // pane so the user sees why no markdown is shown, but keep the run green.
        if (mdPane) mdPane.render("could not read " + mdPath + ": " + String(readErr), mdPath);
        resolvedMd = "";
        readError = String(readErr);
      }
    }

    // Only declare success once the result is actually in hand: gate the "done" UI
    // on the read outcome so a read failure does not flash 100%/"done" then "failed".
    if (ui && !readError) {
      ui.setRunning(false);
      ui.setFill(100);
      ui.setStatus("done");
    }

    if (resolvedMd) {
      // The review pane (mdPane) shows the on-disk/in-memory markdown. The
      // transcript is driven solely by the ocr://done event (its listener is
      // attached before invoke, so it always fires): do NOT also append here, or
      // a race between the invoke resolving and the event firing renders twice.
      if (mdPane) mdPane.render(resolvedMd, mdPath);
    }

    if (readError) {
      if (ui) {
        ui.setRunning(false);
        ui.fail("read failed: " + readError);
      }
      const stem = (splitPath(path) || {}).name || path;
      showToast("run:" + path, {
        kind: "error",
        title: stem + " — OCR failed",
        meta: "read failed: " + readError.slice(0, 140),
      });
      removeToast("run:" + path, 8000);
      addNotification("error", stem + " — OCR failed", "read failed: " + readError);
      return false;
    }

    // The job row is recorded by the backend (run_ocr) and the Library/Board
    // reload via jobs://changed. Here just surface completion: a momentary toast +
    // a persisted bell notification. mdPath is the written file path ("" for an
    // in-memory run).
    const stem = (splitPath(path) || {}).name || path;
    showToast("run:" + path, {
      kind: "done",
      title: stem + " — OCR complete",
      meta: mdPath || "",
    });
    removeToast("run:" + path, 5000);
    addNotification("done", stem + " — OCR complete", mdPath || "");
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
      // The backend already finished this file's job row as "failed: stopped by
      // user" before returning the "stopped" error, so no frontend record here.
      const stem = (splitPath(path) || {}).name || path;
      showToast("run:" + path, { kind: "info", title: stem + " — stopped", meta: "reload the model to run again" });
      removeToast("run:" + path, 6000);
      addNotification("info", stem + " — OCR stopped", "Stopped by user; reload the model to run again.");
      // Sentinel so a batch loop can stop dispatching the remaining files (the
      // model was dropped; they would all fail "load a model first").
      return "stopped";
    }
    // A per-file failure is recorded by the backend (it finishes the job row as
    // failed before returning); an error thrown before any file started (e.g. "load
    // a model first") writes no row, which is correct (no run began). The user sees
    // the error here regardless.
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
    // Fire-and-forget: the click handler cannot await without holding the event.
    // runOcrOnPath owns UI state transitions + error surfacing per file.
    // Output location: folder applies to every file; a custom filename is honored
    // only for a single-file run (the backend rejects out_file with >1 input, and
    // the field is disabled in the UI for batches).
    const outDir = (document.getElementById("outFolder") || {}).value || "";
    const outFile = paths.length === 1 ? (document.getElementById("outFile") || {}).value || "" : "";
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
          r = await runOcrOnPath(path, ui, mdPane, unlistensRef, { dir: outDir, file: outFile });
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
        // Honor the chosen output folder; dropped imports are batch-shaped, so no
        // custom filename (each writes {stem}.md into the folder / beside input).
        const outDir = (document.getElementById("outFolder") || {}).value || "";
        const r = await runOcrOnPath(pdf, ui, mdPane, unlistensRef, { dir: outDir, file: null });
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

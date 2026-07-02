import { requireTauri } from "./tauri.js";
import { subscribeOcrEvents } from "./ocr-events.js";
import { readRunOptions } from "./options.js";
import { refreshModelStatus } from "./model.js";
import { parentDirOf, splitPath } from "./paths.js";
import { showToast, removeToast, addNotification } from "./toasts.js";

// EH-0013 bite 2: i18n hook. Named `tr` -- `t` is the Tauri handle in runOcrOnPath.
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

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
    ui.setStatus(tr("run.starting"));
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
      title: tr("run.savingOutputTo"),
      meta: outDir === "." ? tr("run.cwd") : outDir
    });
    removeToast("run-dir-info:" + path, 4000);

    // out_file: single-file custom name (null for batch; the caller gates this).
    const outFile = (ov.file || "").trim() || null;
    // quant is fixed at load time (the model is already held warm); run_ocr only
    // takes the per-run options below.
    const res = await t.core.invoke("run_ocr", {
      inputs: [path],
      outDir,
      outFile,
      maxTokens: opts.maxTokens,
      dpi: opts.dpi,
      prompt: opts.prompt,
      keepImages: opts.keepImages,
      repeatPenalty: opts.repeatPenalty,
      dryMultiplier: opts.dryMultiplier,
      dryBase: opts.dryBase,
      firstPage: opts.firstPage,
      lastPage: opts.lastPage,
      // single/pages/both; resolved by the backend's parse_output_mode.
      outputMode: opts.outputMode,
      // Informational: recorded on the backend-written job row so the
      // Library/Board show the quant the run used (the model is already warm).
      quant: opts.quant,
    });

    // run_ocr returns { paths, combined }: paths = written file paths (combined
    // file first in single/both; first page file in pages); combined = the full
    // in-memory markdown. The transcript pane is driven solely by the ocr://done
    // event (its listener is attached before invoke); the review pane is filled
    // here. In pages mode there is no single combined file on disk, so preview the
    // in-memory combined text (Save/Export stay disabled — see markdown_pane).
    const isPages = opts.outputMode === "pages";
    let resolvedMd = "";
    let mdPath = "";
    let readError = null;
    if (isPages) {
      resolvedMd = (res && res.combined) || "";
      mdPath = res && res.paths && res.paths.length ? parentDirOf(res.paths[0]) || "" : "";
    } else if (res && res.paths && res.paths.length) {
      mdPath = res.paths[0];
      try {
        // The backend authorizes reads from its own record of files run_ocr just
        // wrote (AppState.read_allow); no client-supplied allowlist is needed.
        resolvedMd = await t.core.invoke("read_text_file", { path: mdPath });
      } catch (readErr) {
        // File read failed (rare: written then removed). Surface in the review
        // pane so the user sees why no markdown is shown, but keep the run green.
        if (mdPane) mdPane.render(tr("run.couldNotRead", { path: mdPath, error: String(readErr) }), mdPath);
        resolvedMd = "";
        readError = String(readErr);
      }
    }

    // Only declare success once the result is actually in hand: gate the "done" UI
    // on the read outcome so a read failure does not flash "done" then "failed".
    if (ui && !readError) {
      ui.setRunning(false);
      ui.setStatus(tr("run.doneStatus"));
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
        ui.fail(tr("run.readFailed", { error: readError }));
      }
      const stem = (splitPath(path) || {}).name || path;
      showToast("run:" + path, {
        kind: "error",
        title: tr("run.ocrFailed", { stem }),
        meta: tr("run.readFailed", { error: readError.slice(0, 140) }),
      });
      removeToast("run:" + path, 8000);
      addNotification("error", tr("run.ocrFailed", { stem }), tr("run.readFailed", { error: readError }));
      return false;
    }

    // The job row is recorded by the backend (run_ocr) and the Library/Board
    // reload via jobs://changed. Here just surface completion: a momentary toast +
    // a persisted bell notification. mdPath is the written file path ("" for an
    // in-memory run).
    const stem = (splitPath(path) || {}).name || path;
    showToast("run:" + path, {
      kind: "done",
      title: tr("run.ocrComplete", { stem }),
      meta: mdPath || "",
    });
    removeToast("run:" + path, 5000);
    addNotification("done", tr("run.ocrComplete", { stem }), mdPath || "");
    return true;
  } catch (err) {
    // User-initiated stop is not a failure. The backend killed the local server
    // and dropped the model, so refresh the gate (Run -> "Load a model first").
    const wasStopped = String(err).trim() === "stopped";
    if (ui) {
      ui.setRunning(false);
      if (wasStopped) {
        ui.setStatus(tr("run.stoppedStatus"));
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
      showToast("run:" + path, { kind: "info", title: tr("run.ocrStopped", { stem }), meta: tr("run.reloadToRunAgain") });
      removeToast("run:" + path, 6000);
      addNotification("info", tr("run.ocrStoppedInfo", { stem }), tr("run.stoppedByUser"));
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
      title: tr("run.ocrFailed", { stem }),
      meta: String(err).slice(0, 140),
    });
    removeToast("run:" + path, 8000);
    addNotification("error", tr("run.ocrFailed", { stem }), String(err));
    return false;
  }
}

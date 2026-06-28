// Transcript / progress UI controller. Owns the live transcript pane, the run
// progress bar + status text, the run popup (with Stop), and the Run-gate state
// (model loaded? run in flight?). subscribeOcrEvents routes every ocr:// event
// through this api so the event handlers never reach for DOM they cannot see.

import { showToast, removeToast } from "./toasts.js";
import { requireTauri } from "./tauri.js";

/** Tiny controller over the transcript/progress DOM nodes. Keeps main flow flat. */
export function makeUi() {
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
  const backdrop = document.getElementById("runPopupBackdrop");
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
  // Live-transcript flow control. A repetition-looping model streams tokens faster
  // than the DOM can absorb one write+reflow per token, which starves the JS event
  // loop (the Stop click never runs) and grows memory without bound. So buffer the
  // chunks and flush once per animation frame, and cap the rendered text length.
  let pendingChunk = "";
  let pendingPage = null;
  let rafHandle = 0;
  const STREAM_CAP = 100000;
  const STREAM_KEEP = 80000;
  function flushPartial() {
    rafHandle = 0;
    const chunk = pendingChunk;
    const page = pendingPage;
    pendingChunk = "";
    if (!chunk) return;
    if (body) {
      if (streamPre === null || streamPre.dataset.page !== String(page)) {
        if (placeholder) placeholder.hidden = true;
        streamPre = document.createElement("pre");
        streamPre.dataset.page = String(page);
        body.appendChild(streamPre);
      }
      streamPre.textContent += chunk;
      if (streamPre.textContent.length > STREAM_CAP) {
        streamPre.textContent = streamPre.textContent.slice(-STREAM_KEEP);
      }
      body.scrollTop = body.scrollHeight;
    }
    if (popupLog) {
      popupLog.textContent += chunk;
      if (popupLog.textContent.length > STREAM_CAP) {
        popupLog.textContent = popupLog.textContent.slice(-STREAM_KEEP);
      }
      popupLog.scrollTop = popupLog.scrollHeight;
    }
  }
  function cancelPendingFlush() {
    if (rafHandle) {
      cancelAnimationFrame(rafHandle);
      rafHandle = 0;
    }
    pendingChunk = "";
    pendingPage = null;
  }
  // Controls greyed out during a run so a second run can't be launched and run
  // options can't change mid-flight. loadModelBtn + importBtn + every #runOpts input.
  function setControlsDisabled(on) {
    const ids = ["loadModelBtn", "importBtn", "engineModifyBtn"];
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
      if (backdrop) backdrop.hidden = false;
    },
    closePopup() {
      if (popup) popup.hidden = true;
      if (backdrop) backdrop.hidden = true;
    },
    isRunning() {
      return running;
    },
    /** Append one streamed token chunk for `page` to both the transcript (one
     *  <pre> per page) and the popup log. Centralized here so subscribeOcrEvents
     *  does not reach for body/placeholder it cannot see in its scope. */
    appendPartial(page, chunk) {
      if (typeof chunk !== "string") return;
      // A new page's chunk flushes the previous page's buffer first, so a page
      // boundary never gets merged into the wrong <pre>.
      if (pendingPage !== null && pendingPage !== page && pendingChunk) {
        flushPartial();
      }
      pendingPage = page;
      pendingChunk += chunk;
      if (!rafHandle) rafHandle = requestAnimationFrame(flushPartial);
    },
    /** Drop the provisional per-page <pre>s (ocr://done renders the assembled
     *  markdown instead) and reset the stream cursor. Popup log is left intact so
     *  the user can still scroll the streamed output after completion. */
    clearPartial() {
      cancelPendingFlush();
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
      cancelPendingFlush();
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

  // Clicking the dim backdrop minimizes, same as the × button.
  if (backdrop && popupClose) {
    backdrop.addEventListener("click", () => popupClose.click());
  }

  return api;
}

import { requireTauri } from "./tauri.js";
import { jobBaseName, formatEpoch } from "./paths.js";

// EH-0013 bite 2: i18n hook. Named `tr` -- `t` is the Tauri handle in openInReview.
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

/** EH-0015: navigate to the Review view and render the .md for a done job.
 *  Called when the user clicks a job card whose status is "done" and has an
 *  outputPath. Switches the rail to "review", loads the markdown from disk via
 *  read_text_file, and renders it in the review pane. Fail-soft: errors are
 *  logged but never crash the Library view.
 *
 *  `outputPath` is the absolute path to the written {stem}.md.
 *  `mdPane`     the makeMarkdownPane() controller.
 *  `buttons`    the rail button NodeList (to update is-active + titlebar). */
export async function openInReview(outputPath, mdPane, buttons) {
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
  if (screenTitle) screenTitle.textContent = tr("view.review");

  // Fetch the .md from disk via the backend command.
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  try {
    // No client allowlist: read_text_file serves only paths recorded in the job
    // store (this outputPath came from that store) or written this session.
    const markdown = await t.core.invoke("read_text_file", {
      path: outputPath,
    });
    if (mdPane) mdPane.render(markdown, outputPath);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[library] re-open failed:", err);
    if (mdPane) mdPane.render(tr("run.couldNotRead", { path: outputPath, error: String(err) }), outputPath);
  }
}

/** Format a run duration (milliseconds) as a short human string. Sub-second ->
 *  "0.4s"; minutes -> "1m 05s". Returns "" for null/undefined so addRow skips it. */
function fmtDuration(ms) {
  if (ms === null || ms === undefined) return "";
  const secs = ms / 1000;
  if (secs < 60) return secs.toFixed(1) + "s";
  const m = Math.floor(secs / 60);
  const s = Math.round(secs % 60);
  return m + "m " + String(s).padStart(2, "0") + "s";
}

/** Populate the shared #runInfoDialog with a job's metadata (a key/value list,
 *  reusing the pdf-info row styling) and open it. Every value comes from the job
 *  object already in memory -- no backend round-trip. Fail-soft: a missing dialog
 *  element or showModal support is a no-op. */
export function openRunInfo(job) {
  const dlg = document.getElementById("runInfoDialog");
  const body = document.getElementById("runInfoBody");
  if (!dlg || !body || typeof dlg.showModal !== "function") return;
  body.innerHTML = "";
  const addRow = (label, value) => {
    if (value === null || value === undefined || value === "") return;
    const row = document.createElement("div");
    row.className = "pdf-info__row";
    const l = document.createElement("span");
    l.className = "pdf-info__label";
    l.textContent = label;
    const v = document.createElement("span");
    v.className = "pdf-info__value";
    v.textContent = String(value);
    row.append(l, v);
    body.appendChild(row);
  };
  const opts = (job && job.options) || {};
  addRow(tr("runinfo.input"), job && job.inputPath);
  addRow(tr("runinfo.status"), tr("status." + ((job && job.status) || "queued")));
  addRow(tr("runinfo.output"), job && job.outputPath);
  if (job && job.status === "failed") addRow(tr("runinfo.error"), job.error);
  addRow(tr("runinfo.pages"), job && job.pageCount);
  addRow(tr("runinfo.duration"), fmtDuration(job && job.durationMs));
  addRow(tr("runinfo.backend"), job && job.backend);
  addRow(tr("runinfo.outputMode"), job && job.outputMode);
  addRow(tr("runinfo.quant"), opts.quant);
  addRow(tr("runinfo.dpi"), opts.dpi);
  addRow(tr("runinfo.maxTokens"), opts.maxTokens);
  addRow(tr("runinfo.keepImages"), opts.keepImages ? tr("job.on") : tr("job.off"));
  if (job && job.createdAt) addRow(tr("runinfo.created"), formatEpoch(job.createdAt));
  if (job && job.updatedAt) addRow(tr("runinfo.updated"), formatEpoch(job.updatedAt));
  dlg.showModal();
}

/** EH-0006: build a single read-only job card element from a Job record. Status
 *  drives the stripe + badge color via the .job-card--<status> class. Done shows the
 *  output path; failed shows the error. Options + timestamps are the meta footer.
 *  Shared by the Library grid (all jobs) and the Board columns (jobs grouped by
 *  status) so a job looks identical in both — one source of truth for the markup.
 *
 *  EH-0015: `mdPane` and `railButtons` are optional. When provided, cards for done
 *  jobs with an outputPath are clickable: clicking opens the .md in the review pane
 *  (via openInReview). The pointer cursor and a "Open in review" aria-label signal
 *  the affordance. Board column cards do not receive these so they stay inert.
 *
 *  `actions` is optional (Library only). When provided, the card grows a footer
 *  row with "Remove" (record-only) and, for done jobs with an output, "Remove +
 *  delete" (record + the .md on disk). Both stopPropagation so a click on a button
 *  does not also trigger the card's open-in-review. Board cards omit `actions`. */
export function renderJobCard(job, mdPane, railButtons, actions) {
  const card = document.createElement("li");
  const status = (job && job.status) || "queued";
  card.className = "job-card job-card--" + status;

  const head = document.createElement("div");
  head.className = "job-card__head";
  // Multi-select checkbox: Library-only. Rendered iff `actions` carries BOTH
  // select hooks (isSelected + toggleSelect). The Board passes a PARTIAL actions
  // ({ remove } for its pending-job remove affordance), so guarding on the hooks
  // (not just truthy `actions`) keeps the Board checkbox-free and throw-free.
  // First child of the head so it sits left of the name; clicks stopPropagation so
  // toggling never triggers the card's open-in-review affordance on done cards.
  const selectable =
    actions &&
    typeof actions.isSelected === "function" &&
    typeof actions.toggleSelect === "function";
  if (selectable) {
    const id = job && job.id;
    const check = document.createElement("input");
    check.type = "checkbox";
    check.className = "job-card__check";
    check.checked = actions.isSelected(id);
    check.setAttribute(
      "aria-label",
      tr("job.selectOne", { name: jobBaseName(job && job.inputPath) })
    );
    check.addEventListener("click", (ev) => ev.stopPropagation());
    check.addEventListener("change", () => {
      actions.toggleSelect(id, check.checked);
    });
    head.appendChild(check);
  }
  const name = document.createElement("span");
  name.className = "job-card__name";
  name.textContent = jobBaseName(job && job.inputPath);
  name.title = (job && job.inputPath) || "";
  const badge = document.createElement("span");
  badge.className = "job-card__status";
  badge.textContent = tr("status." + status);
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
  push(tr("job.quant"), opts.quant);
  push(tr("job.dpi"), opts.dpi);
  push(tr("job.maxTok"), opts.maxTokens);
  push(tr("job.keepImg"), opts.keepImages ? tr("job.on") : tr("job.off"));
  if (job && job.updatedAt) {
    push(tr("job.updated"), formatEpoch(job.updatedAt));
  } else if (job && job.createdAt) {
    push(tr("job.queuedLabel"), formatEpoch(job.createdAt));
  }
  card.appendChild(meta);

  // EH-0015: wire the "re-open in review" affordance for done jobs that have an
  // on-disk output. Only the Library grid passes mdPane + railButtons; Board cards
  // are intentionally inert (the Board is a status board, not a content browser).
  const outputPath = job && job.outputPath;
  if (mdPane && railButtons && status === "done" && outputPath) {
    card.classList.add("job-card--openable");
    card.title = tr("job.clickToOpen");
    card.style.cursor = "pointer";
    card.setAttribute("role", "button");
    card.tabIndex = 0;
    card.addEventListener("click", () => {
      openInReview(outputPath, mdPane, railButtons);
    });
    card.addEventListener("keydown", (ev) => {
      // Ignore Enter/Space that originate on the multi-select checkbox so
      // keyboard-toggling it does not also open the review pane.
      if (ev.target && ev.target.closest && ev.target.closest(".job-card__check"))
        return;
      if (ev.key === "Enter") {
        ev.preventDefault();
        openInReview(outputPath, mdPane, railButtons);
      } else if (ev.key === " ") {
        ev.preventDefault(); // prevent page scroll
      }
    });
    card.addEventListener("keyup", (ev) => {
      if (ev.target && ev.target.closest && ev.target.closest(".job-card__check"))
        return;
      if (ev.key === " ") {
        openInReview(outputPath, mdPane, railButtons);
      }
    });
  }

  // Library-only action footer: remove (record) / remove + delete (record + .md).
  if (actions) {
    const row = document.createElement("div");
    row.className = "job-card__actions";

    // Info: opens the run-detail dialog. Read-only, so it is offered for every
    // status (a failed run still has a useful error/backend/duration snapshot).
    const info = document.createElement("button");
    info.type = "button";
    info.className = "job-card__action";
    info.textContent = tr("job.info");
    info.title = tr("job.infoTitle");
    info.addEventListener("click", (ev) => {
      ev.stopPropagation();
      openRunInfo(job);
    });
    row.appendChild(info);

    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "job-card__action";
    remove.textContent = tr("job.remove");
    remove.title = tr("job.removeTitle");
    remove.addEventListener("click", (ev) => {
      ev.stopPropagation();
      actions.remove(job && job.id);
    });
    row.appendChild(remove);

    if (status === "done" && outputPath) {
      const del = document.createElement("button");
      del.type = "button";
      del.className = "job-card__action job-card__action--danger";
      del.textContent = tr("job.removeDelete");
      del.title = tr("job.removeDeleteTitle");
      del.addEventListener("click", (ev) => {
        ev.stopPropagation();
        actions.removeDelete(job && job.id, outputPath);
      });
      row.appendChild(del);
    }

    card.appendChild(row);
  }

  return card;
}

/** Native confirm via tauri-plugin-dialog (exposed at window.__TAURI__.dialog by
 *  withGlobalTauri). Returns true only on an explicit confirm; fail-soft to false
 *  (treat as cancel) when the dialog API is unavailable, so a destructive action
 *  never proceeds without a confirmation. */
export async function confirmDestructive(message) {
  try {
    const dialog = window.__TAURI__ && window.__TAURI__.dialog;
    if (!dialog || typeof dialog.ask !== "function") return false;
    return await dialog.ask(message, { title: "unlocr", kind: "warning" });
  } catch (err) {
    // eslint-disable-next-line no-console
    console.error("[library] confirm dialog failed", err);
    return false;
  }
}

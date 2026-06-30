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
      if (ev.key === "Enter") {
        ev.preventDefault();
        openInReview(outputPath, mdPane, railButtons);
      } else if (ev.key === " ") {
        ev.preventDefault(); // prevent page scroll
      }
    });
    card.addEventListener("keyup", (ev) => {
      if (ev.key === " ") {
        openInReview(outputPath, mdPane, railButtons);
      }
    });
  }

  // Library-only action footer: remove (record) / remove + delete (record + .md).
  if (actions) {
    const row = document.createElement("div");
    row.className = "job-card__actions";

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

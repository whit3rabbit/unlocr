// Notifications + toasts.
//
// Two surfaces: transient TOASTS (bottom-right #toastStack) for live download
// progress and momentary done/failed flashes, and a persisted PANEL (the bell,
// #notifyPanel) backed by notifications.json via the add/list/clear commands.
// Toasts are pure DOM; the panel round-trips through Tauri. All user-supplied
// text (filenames, error messages, output paths) is set via textContent, never
// innerHTML, so a hostile path/error string cannot inject markup.

import { requireTauri } from "./tauri.js";

/** Compact human byte size, e.g. 1503238553 -> "1.4 GB". */
function fmtBytes(n) {
  if (!n || n < 0) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) {
    n /= 1024;
    i += 1;
  }
  return (i === 0 ? n : n.toFixed(1)) + " " + u[i];
}

/** Relative age of a unix-seconds timestamp, e.g. "3m ago". */
function relTime(secs) {
  const now = Math.floor(Date.now() / 1000);
  const d = Math.max(0, now - (secs || 0));
  if (d < 60) return d + "s ago";
  if (d < 3600) return Math.floor(d / 60) + "m ago";
  if (d < 86400) return Math.floor(d / 3600) + "h ago";
  return Math.floor(d / 86400) + "d ago";
}

/** Create or update a toast by id (same id = update in place, used for live
 *  download progress). opts: {title, kind, meta, fill}. fill is 0..100 to show a
 *  progress bar, omitted for a plain notice. */
export function showToast(id, opts) {
  const stack = document.getElementById("toastStack");
  if (!stack) return null;
  let el = stack.querySelector('[data-toast="' + id + '"]');
  if (!el) {
    el = document.createElement("div");
    el.dataset.toast = id;
    el.innerHTML =
      '<div class="toast__title"></div>' +
      '<div class="toast__meta"></div>' +
      '<div class="toast__bar" hidden><div class="toast__fill"></div></div>';
    stack.appendChild(el);
  }
  el.className = "toast" + (opts.kind ? " toast--" + opts.kind : "");
  el.querySelector(".toast__title").textContent = opts.title || "";
  const meta = el.querySelector(".toast__meta");
  meta.textContent = opts.meta || "";
  meta.hidden = !opts.meta;
  const bar = el.querySelector(".toast__bar");
  if (typeof opts.fill === "number") {
    bar.hidden = false;
    el.querySelector(".toast__fill").style.width =
      Math.max(0, Math.min(100, opts.fill)) + "%";
  } else {
    bar.hidden = true;
  }
  return el;
}

/** Remove a toast by id, optionally after `delay` ms (lets a completed toast
 *  linger briefly before fading out). */
export function removeToast(id, delay) {
  const stack = document.getElementById("toastStack");
  if (!stack) return;
  const el = stack.querySelector('[data-toast="' + id + '"]');
  if (!el) return;
  if (delay) setTimeout(() => el.remove(), delay);
  else el.remove();
}

// Per-file download state for speed (bytes/sec) derived from successive events,
// plus a flag so we only record a "model ready" notification when a download
// actually happened this load (server-ready also fires on every plain run).
const dlSpeed = new Map();
let dlHappened = false;

/** Add a persisted notification (best-effort). Refreshes the bell badge. Never
 *  throws into the caller: outside the webview or on a store error it just no-ops. */
export async function addNotification(kind, title, body) {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  try {
    await t.core.invoke("add_notification", { kind, title, body: body || "" });
  } catch (err) {
    return;
  }
  refreshNotifyPanel();
}

/** Reload the notification list into the panel and update the unread badge.
 *  Best-effort; silent outside the webview. */
export async function refreshNotifyPanel() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  let list = [];
  try {
    list = await t.core.invoke("list_notifications");
  } catch (err) {
    return;
  }
  const badge = document.getElementById("notifyBadge");
  if (badge) {
    const unread = list.filter((n) => !n.read).length;
    badge.textContent = String(unread);
    badge.hidden = unread === 0;
  }
  const listEl = document.getElementById("notifyList");
  if (!listEl) return;
  if (list.length === 0) {
    listEl.innerHTML = '<p class="notify-panel__empty">No notifications.</p>';
    return;
  }
  listEl.innerHTML = "";
  // Newest first.
  list
    .slice()
    .reverse()
    .forEach((n) => {
      const item = document.createElement("div");
      item.className =
        "notify-item notify-item--" + (n.kind || "info") + (n.read ? "" : " is-unread");

      const title = document.createElement("div");
      title.className = "notify-item__title";
      title.textContent = n.title || "";
      item.appendChild(title);

      if (n.body) {
        const bodyEl = document.createElement("div");
        bodyEl.className = "notify-item__body";
        bodyEl.textContent = n.body;
        item.appendChild(bodyEl);
      }

      const time = document.createElement("div");
      time.className = "notify-item__time";
      time.textContent = relTime(n.createdAt);
      item.appendChild(time);

      const x = document.createElement("button");
      x.className = "notify-item__x";
      x.type = "button";
      x.title = "Dismiss";
      x.textContent = "×";
      x.addEventListener("click", async (ev) => {
        ev.stopPropagation();
        try {
          await t.core.invoke("clear_notification", { id: n.id });
        } catch (err) {
          /* ignore */
        }
        refreshNotifyPanel();
      });
      item.appendChild(x);

      listEl.appendChild(item);
    });
}

/** Live download toasts: one per file, pct + size + MB/s, removed when complete.
 *  Records a single "Model ready" notification once a download finishes. */
function wireDownloadToasts(t) {
  t.event.listen("ocr://progress", (e) => {
    const { name, pct, done, total } = (e && e.payload) || {};
    const key = name || "model";
    const id = "dl:" + key;
    dlHappened = true;

    let speedStr = "";
    if (typeof done === "number") {
      const now = Date.now();
      const prev = dlSpeed.get(key);
      if (prev && now > prev.time) {
        const bps = ((done - prev.done) * 1000) / (now - prev.time);
        if (bps > 0) speedStr = fmtBytes(bps) + "/s";
      }
      dlSpeed.set(key, { done, time: now });
    }
    const sizeStr =
      total > 0 ? fmtBytes(done) + " / " + fmtBytes(total) : fmtBytes(done || 0);
    showToast(id, {
      kind: "download",
      title: "Downloading " + key,
      meta:
        (pct != null ? pct + "%  ·  " : "") +
        sizeStr +
        (speedStr ? "  ·  " + speedStr : ""),
      fill: typeof pct === "number" ? pct : undefined,
    });
    if (pct >= 100) {
      dlSpeed.delete(key);
      removeToast(id, 1500);
    }
  });

  t.event.listen("ocr://server-ready", () => {
    // All files present and the server is up. Clear any lingering download toasts
    // and, if a download actually ran this load, record one completion notice.
    const stack = document.getElementById("toastStack");
    if (stack) {
      stack
        .querySelectorAll('[data-toast^="dl:"]')
        .forEach((el) => el.remove());
    }
    if (dlHappened) {
      dlHappened = false;
      addNotification("download", "Model download complete", "");
    }
  });
}

/** Wire the bell (toggle panel, mark-read on open, click-outside close), the
 *  Clear-all button, and the download toasts. Silent outside the webview. */
export function initNotifications() {
  let t;
  try {
    t = requireTauri();
  } catch (err) {
    return;
  }
  const bell = document.getElementById("notifyBell");
  const panel = document.getElementById("notifyPanel");
  const clearAll = document.getElementById("notifyClearAll");

  if (bell && panel) {
    bell.addEventListener("click", async (e) => {
      e.stopPropagation();
      const opening = panel.hidden;
      panel.hidden = !opening;
      bell.setAttribute("aria-expanded", String(opening));
      if (opening) {
        await refreshNotifyPanel();
        // Mark read so the badge clears, then re-render to drop unread styling.
        try {
          await t.core.invoke("mark_notifications_read");
        } catch (err) {
          /* ignore */
        }
        refreshNotifyPanel();
      }
    });
    document.addEventListener("click", (e) => {
      if (panel.hidden) return;
      if (e.target === bell || bell.contains(e.target) || panel.contains(e.target)) return;
      panel.hidden = true;
      bell.setAttribute("aria-expanded", "false");
    });
  }
  if (clearAll) {
    clearAll.addEventListener("click", async () => {
      try {
        await t.core.invoke("clear_all_notifications");
      } catch (err) {
        /* ignore */
      }
      refreshNotifyPanel();
    });
  }

  wireDownloadToasts(t);
  refreshNotifyPanel(); // seed the badge from the persisted store on launch
}

// Tauri backend for the unlocr desktop GUI.
//
// Thin bridge over the repo-root `unlocr` crate (linked via the path dep in
// Cargo.toml). All OCR behavior lives in `unlocr`; this crate only:
//   - exposes typed commands the frontend can invoke,
//   - maps frontend params into `unlocr::OcrOptions`,
//   - forwards `unlocr::Progress` to the webview as Tauri events.
// No pipeline logic is forked in here (see gui/CLAUDE.md "thin shim").
//
// Layout (commands split out of this file so each stays navigable):
//   - state.rs      AppState + the held backend (Backend/LoadedModel).
//   - cmd_model/    preflight, load/unload/stop, status (mod.rs); cache info/clear (cache.rs).
//   - cmd_run/      run_ocr (mod.rs); render_pages/render_page (render.rs);
//                   read/write/export file commands (fs.rs); list_tools/download_tool
//                   Windows dep downloader (tools.rs).
//   - cmd_store.rs  job store, settings, and notification command wrappers.
// This file keeps only the module wiring and the `run()` builder.

use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{Emitter, Manager, RunEvent};

// SQLite backing for the jobs/settings/notifications stores (single unlocr.db
// under the app-data dir). Opened once in .setup(); accessed via db::with_db.
mod db;
// Persisted job store (SQLite `jobs` table). Library/Board views + the file-read
// allowlist (`allowed_output_paths`) read it; `record_job` writes one row per run.
mod store;
// Persisted GUI settings (provider mode + engine defaults), single-row `settings`
// table in the same DB.
mod settings;
// Persisted notifications (terminal events surfaced in the bell panel), same DB.
mod notifications;

// Managed state + command handlers (see module-doc comments above).
mod cmd_model;
mod cmd_run;
mod cmd_store;
mod state;

// Re-export the wire types so the frontend contract (camelCase Job/JobOptions) is
// documented in one place.
pub use store::{Job, JobOptions};

use cmd_model::{
    clear_model_cache, get_cache_info, list_available_quants, list_cached_files, list_local_models,
    load_model, model_status, preflight, remove_cached_file, stop_ocr, system_requirements,
    unload_model,
};
use cmd_run::{
    brew_available, brew_install, check_pdf_password, download_tool, export_markdown, host_os,
    list_tools, pdf_info, pdf_needs_password, pick_password_file, read_text_file, render_page,
    render_pages, run_ocr, scan_input_paths, write_text_file,
};
use cmd_store::{
    add_notification, clear_all_notifications, clear_notification, delete_job, delete_jobs,
    get_settings, jobs_store_path, list_jobs, list_notifications, mark_notifications_read,
    save_settings,
};
use state::AppState;

/// Application entry point that configures and starts the Tauri desktop GUI.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Open the SQLite store before anything else: a GUI that cannot open
            // its DB is broken, not merely empty, so fail startup with a clear
            // message instead of the old silent-degrade. The single connection
            // lives in db.rs's global slot; store modules reach it via with_db.
            if let Err(e) = crate::db::init() {
                eprintln!("[setup] db init failed: {e}");
                return Err(e.into());
            }

            // No OCR run survives a process restart, so any row still marked
            // `running` is a crash artifact: flip it to `failed` so the Board is
            // accurate on launch instead of showing a phantom in-flight job.
            // Best-effort: a reconcile failure logs but never aborts startup.
            match crate::store::reconcile_interrupted() {
                Ok(n) if n > 0 => eprintln!("[setup] reconciled {n} interrupted job(s)"),
                Ok(_) => {}
                Err(e) => eprintln!("[setup] job reconcile failed: {e}"),
            }

            // Allow the asset protocol to read cached preview PNGs. The cache dir
            // is resolved per-OS at runtime, so the scope is extended here rather
            // than hardcoded in tauri.conf.json. Best-effort: a failure just means
            // previews fail to load (logged), not an app crash.
            if let Ok(cache) = unlocr::model::cache_dir(None) {
                let previews = cache.join("previews");
                let _ = std::fs::create_dir_all(&previews);
                // SECURITY: scope is the `previews` SUBDIR only, never the cache root.
                // Widening this to `cache` would expose every cached file (the GGUF
                // weights, preview PNGs) to the renderer via `asset:` (img-src allows
                // it in tauri.conf.json). The sensitive stores (jobs, settings with
                // the remote API key, notifications) no longer live here at all: they
                // moved to `<app-data>/unlocr/unlocr.db`, but keep the scope pinned
                // to previews; do not loosen this casually.
                debug_assert!(
                    previews.ends_with("previews"),
                    "asset scope must stay the previews subdir, not the cache root"
                );
                if let Err(e) = app.asset_protocol_scope().allow_directory(&previews, true) {
                    eprintln!("[setup] asset scope allow_directory failed: {e}");
                }
            }

            // Idle-unload watcher: the warm model (a held llama-server) is the app's
            // dominant footprint (~6-8 GB GGUF). A walk-away user keeps it resident
            // forever otherwise, so drop it after `idle_unload_minutes` of no load/run
            // to reclaim that RAM (reload is required before the next run). A plain
            // thread (60s tick) avoids assuming a tokio runtime; the model lock makes
            // it safe (try_lock fails while a run holds it, so it never unloads mid-run).
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
                let minutes = settings::load_settings().idle_unload_minutes;
                if minutes == 0 {
                    continue; // disabled: model stays warm until explicit unload / exit
                }
                let state = handle.state::<AppState>();
                // None = never loaded -> nothing to unload. Otherwise idle = time
                // since the last load or run-end (stamped in cmd_model / cmd_run).
                let idle = state
                    .last_used
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .map(|t| t.elapsed());
                let Some(idle) = idle else { continue };
                if idle < std::time::Duration::from_secs(minutes as u64 * 60) {
                    continue;
                }
                // try_lock, NOT lock: a run holds the model lock for its whole batch,
                // so a failed try_lock means a run is in flight; skip this tick rather
                // than block (and never unload mid-run). Tight inner scope so the
                // try_lock guard drops before `state` is reused below.
                let unloaded = {
                    match state.model.try_lock() {
                        Ok(mut g) if g.is_some() => {
                            *g = None; // drops Server -> kills llama-server, frees RAM
                            true
                        }
                        _ => false,
                    }
                };
                if unloaded {
                    *state.server_pid.lock().unwrap_or_else(|p| p.into_inner()) = None;
                    *state.last_used.lock().unwrap_or_else(|p| p.into_inner()) = None;
                    // Tell the UI so the titlebar badge + Run gate reflect the unload.
                    let _ = handle.emit("model://unloaded", ());
                }
            });

            // Native File menu. The action items just emit one event; the
            // frontend reuses the existing toolbar buttons (no logic forked
            // here). Exit uses the predefined quit item, which fires
            // RunEvent::Exit below and kills llama-server.
            let load_pdf = MenuItemBuilder::new("Load PDF...")
                .id("menu_load_pdf")
                .accelerator("CmdOrCtrl+O")
                .build(app)?;
            let load_model = MenuItemBuilder::new("Load Model")
                .id("menu_load_model")
                .accelerator("CmdOrCtrl+M")
                .build(app)?;
            let unload_model = MenuItemBuilder::new("Unload Model")
                .id("menu_unload_model")
                .accelerator("CmdOrCtrl+Shift+U")
                .build(app)?;

            let file = SubmenuBuilder::new(app, "File")
                .item(&load_pdf)
                .item(&load_model)
                .item(&unload_model)
                .separator()
                .quit()
                .build()?;
            // Edit menu kept so cut/copy/paste keep working in webview text
            // fields on macOS (a custom menu replaces Tauri's default).
            let edit = SubmenuBuilder::new(app, "Edit")
                .undo()
                .redo()
                .separator()
                .cut()
                .copy()
                .paste()
                .select_all()
                .build()?;
            let menu = MenuBuilder::new(app).items(&[&file, &edit]).build()?;
            app.set_menu(menu)?;

            app.on_menu_event(|app, event| {
                if let id @ ("menu_load_pdf" | "menu_load_model" | "menu_unload_model") =
                    event.id().0.as_str()
                {
                    let _ = app.emit("menu://action", id);
                }
            });

            Ok(())
        })
        // Managed state holding the warm model + resolved pdftoppm across commands.
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            preflight,
            run_ocr,
            render_pages,
            render_page,
            pdf_info,
            pdf_needs_password,
            check_pdf_password,
            pick_password_file,
            scan_input_paths,
            read_text_file,
            write_text_file,
            export_markdown,
            host_os,
            list_tools,
            download_tool,
            brew_available,
            brew_install,
            list_jobs,
            jobs_store_path,
            delete_job,
            delete_jobs,
            load_model,
            unload_model,
            model_status,
            list_local_models,
            get_settings,
            save_settings,
            get_cache_info,
            clear_model_cache,
            list_available_quants,
            list_cached_files,
            remove_cached_file,
            list_notifications,
            add_notification,
            clear_notification,
            mark_notifications_read,
            clear_all_notifications,
            stop_ocr,
            system_requirements
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // On exit, drop the loaded model so its held llama-server child is
            // killed (Server::Drop, server.rs). Tauri may process::exit on window
            // close, skipping Drop on managed state, which would orphan
            // llama-server; clear it explicitly here instead.
            if let RunEvent::Exit = event {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    let mut g = state.model.lock().unwrap_or_else(|p| p.into_inner());
                    *g = None; // drops Server -> kills llama-server
                }
            }
        });
}

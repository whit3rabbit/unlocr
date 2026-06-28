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
//   - cmd_model.rs  preflight, load/unload/stop, status, cache info/clear.
//   - cmd_run.rs    render_pages, read_text_file, run_ocr.
//   - cmd_store.rs  job store, settings, and notification command wrappers.
// This file keeps only the module wiring and the `run()` builder.

use tauri::menu::{MenuBuilder, SubmenuBuilder};
use tauri::{Emitter, Manager, RunEvent};

// Persisted job store (EH-0006 bite 1). Purely additive: a new module exposing
// `list_jobs` / `record_job` commands. No existing command changes.
mod store;
// Persisted GUI settings (provider mode + engine defaults), same JSON-under-cache
// pattern as store.rs.
mod settings;
// Persisted notifications (terminal events surfaced in the bell panel), same
// JSON-under-cache pattern as store.rs.
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
    clear_model_cache, get_cache_info, list_local_models, load_model, model_status, preflight,
    stop_ocr, unload_model,
};
use cmd_run::{read_text_file, render_pages, run_ocr};
use cmd_store::{
    add_notification, clear_all_notifications, clear_notification, get_settings, jobs_store_path,
    list_jobs, list_notifications, mark_notifications_read, record_job, save_settings,
};
use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Allow the asset protocol to read cached preview PNGs. The cache dir
            // is resolved per-OS at runtime, so the scope is extended here rather
            // than hardcoded in tauri.conf.json. Best-effort: a failure just means
            // previews fail to load (logged), not an app crash.
            if let Ok(cache) = unlocr::model::cache_dir(None) {
                let previews = cache.join("previews");
                let _ = std::fs::create_dir_all(&previews);
                if let Err(e) = app.asset_protocol_scope().allow_directory(&previews, true) {
                    eprintln!("[setup] asset scope allow_directory failed: {e}");
                }
            }

            // Native File menu. The action items just emit one event; the
            // frontend reuses the existing toolbar buttons (no logic forked
            // here). Exit uses the predefined quit item, which fires
            // RunEvent::Exit below and kills llama-server.
            let file = SubmenuBuilder::new(app, "File")
                .text("menu_load_pdf", "Load PDF...")
                .text("menu_load_model", "Load Model")
                .text("menu_unload_model", "Unload Model")
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
            read_text_file,
            list_jobs,
            jobs_store_path,
            record_job,
            load_model,
            unload_model,
            model_status,
            list_local_models,
            get_settings,
            save_settings,
            get_cache_info,
            clear_model_cache,
            list_notifications,
            add_notification,
            clear_notification,
            mark_notifications_read,
            clear_all_notifications,
            stop_ocr
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
                    if let Ok(mut g) = state.model.lock() {
                        *g = None; // drops Server -> kills llama-server
                    }
                }
            }
        });
}

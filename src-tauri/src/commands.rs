// Tauri commands callable from the frontend via invoke().

use tauri::{AppHandle, Emitter, Manager, State};

use crate::db::Db;
use crate::events::{CanonicalActionItem, ScrapeDetail, ScrapeSummary, SidecarStatus};
use crate::runtime::RuntimeHandle;

#[tauri::command]
pub fn list_scrapes(db: State<'_, Db>) -> Result<Vec<ScrapeSummary>, String> {
    db.list_scrapes().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_scrape(id: String, db: State<'_, Db>) -> Result<Option<ScrapeDetail>, String> {
    db.get_scrape(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_source(id: String, app: AppHandle, db: State<'_, Db>) -> Result<(), String> {
    db.delete_source(&id).map_err(|e| e.to_string())?;
    if let Ok(items) = db.list_open_action_items() {
        let _ = app.emit("actions:updated", &items);
    }
    Ok(())
}

#[tauri::command]
pub fn list_action_items(
    status_filter: String,
    db: State<'_, Db>,
) -> Result<Vec<CanonicalActionItem>, String> {
    db.list_action_items(&status_filter)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_action_item_status(
    id: String,
    status: String,
    app: AppHandle,
    db: State<'_, Db>,
) -> Result<CanonicalActionItem, String> {
    let updated = db
        .set_action_status(&id, &status)
        .map_err(|e| e.to_string())?;
    if let Ok(items) = db.list_open_action_items() {
        let _ = app.emit("actions:updated", &items);
    }
    Ok(updated)
}

#[tauri::command]
pub fn set_action_item_assignee(
    id: String,
    assignee_key: Option<String>,
    assignee: Option<String>,
    app: AppHandle,
    db: State<'_, Db>,
) -> Result<CanonicalActionItem, String> {
    let updated = db
        .set_action_assignee(&id, assignee_key.as_deref(), assignee.as_deref())
        .map_err(|e| e.to_string())?;
    if let Ok(items) = db.list_open_action_items() {
        let _ = app.emit("actions:updated", &items);
    }
    Ok(updated)
}

#[tauri::command]
pub fn get_sidecar_status(handle: State<'_, RuntimeHandle>) -> SidecarStatus {
    handle.status()
}

#[tauri::command]
pub fn hide_popover(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("popover") {
        win.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

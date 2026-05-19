// Tauri commands callable from the frontend via invoke().

use tauri::{AppHandle, Manager, State};

use crate::db::Db;
use crate::events::{ScrapeDetail, ScrapeSummary, SidecarStatus};
use crate::sidecar::SidecarHandle;

#[tauri::command]
pub fn list_scrapes(db: State<'_, Db>) -> Result<Vec<ScrapeSummary>, String> {
    db.list_scrapes().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_scrape(id: String, db: State<'_, Db>) -> Result<Option<ScrapeDetail>, String> {
    db.get_scrape(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_sidecar_status(handle: State<'_, SidecarHandle>) -> SidecarStatus {
    handle.status()
}

#[tauri::command]
pub fn hide_popover(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("popover") {
        win.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

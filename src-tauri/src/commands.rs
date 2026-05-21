// Tauri commands callable from the frontend via invoke().

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::db::Db;
use crate::discord::{DiscordBot, DiscordScraper};
use crate::events::{CanonicalActionItem, ScrapeDetail, ScrapeSummary, SidecarStatus};
use crate::runtime::RuntimeManager;
use crate::settings::{AppSettings, SettingsTestResult};

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
pub fn get_sidecar_status(runtime: State<'_, RuntimeManager>) -> SidecarStatus {
    runtime.status()
}

#[tauri::command]
pub fn hide_popover(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("popover") {
        win.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn get_app_settings(app: AppHandle) -> Result<AppSettings, String> {
    crate::settings::load_or_import(&app).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn save_app_settings(
    settings: AppSettings,
    app: AppHandle,
    runtime: State<'_, RuntimeManager>,
) -> Result<AppSettings, String> {
    let runtime = runtime.inner().clone();
    let saved = crate::settings::save(&app, &settings).map_err(|e| e.to_string())?;
    runtime.restart().await.map_err(|e| e.to_string())?;
    Ok(saved)
}

#[tauri::command]
pub async fn test_discord_settings(settings: AppSettings) -> SettingsTestResult {
    let settings = settings.normalized();
    let shape = crate::settings::test_settings_shape(&settings);
    if !shape.ok {
        return shape;
    }

    let bot = DiscordBot::new(
        settings.discord_app_id.clone(),
        settings.discord_bot_token.clone(),
    );
    let bot_user = match bot.test_credentials().await {
        Ok(user) => user.unwrap_or_else(|| "bot".into()),
        Err(e) => return SettingsTestResult::error(format!("Bot check failed: {e}")),
    };

    if let Some(token) = settings.discord_user_token() {
        match DiscordScraper::connect(token).await {
            Ok(scraper) => SettingsTestResult::ok(format!(
                "Bot token works as {bot_user}; scraper token works as {}.",
                scraper.user().unwrap_or_else(|| "user".into())
            )),
            Err(e) => SettingsTestResult::error(format!(
                "Bot token works as {bot_user}, but user token failed: {e}"
            )),
        }
    } else {
        SettingsTestResult::ok(format!(
            "Bot token works as {bot_user}. User token is empty, so scraping will be unavailable."
        ))
    }
}

#[tauri::command]
pub fn test_ai_settings(settings: AppSettings) -> SettingsTestResult {
    crate::ai::test_settings(&settings)
}

#[tauri::command]
pub fn open_settings_window(app: AppHandle) -> Result<(), String> {
    crate::show_settings_window(&app).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_launch_at_login(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_launch_at_login(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())?;
    } else {
        manager.disable().map_err(|e| e.to_string())?;
    }
    manager.is_enabled().map_err(|e| e.to_string())
}

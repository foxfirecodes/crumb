// Tauri commands callable from the frontend via invoke().

use std::process::Command;

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::db::{Db, DiscordSource};
use crate::discord::{DiscordBot, DiscordScraper};
use crate::events::{CanonicalActionItem, ScrapeDetail, ScrapeSummary, SidecarStatus};
use crate::runtime::RuntimeManager;
use crate::settings::{AppSettings, SettingsTestResult};
use crate::PopoverHideGuard;

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
        crate::observe_tray_action_items(&app, &items);
        let _ = app.emit("actions:updated", &items);
    }
    Ok(())
}

#[tauri::command]
pub async fn open_action_source_in_discord(
    id: String,
    app: AppHandle,
    db: State<'_, Db>,
    hide_guard: State<'_, PopoverHideGuard>,
) -> Result<(), String> {
    let db = db.inner().clone();
    let mut source = db
        .discord_source_for_action(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Action item source was not found.".to_string())?;

    if source.guild_id.is_none() && !is_likely_dm_source(&source) {
        source = repair_discord_source_metadata(&app, &db, source).await?;
    }

    let message_id = db
        .latest_discord_message_id_for_action(&id, &source.channel_id)
        .map_err(|e| e.to_string())?;

    let keep_open = crate::settings::load_or_import(&app)
        .map(|s| s.keep_popover_open_on_view)
        .unwrap_or(false);
    if keep_open {
        hide_guard.suppress_next();
    }

    let result = open_with_discord(&discord_source_uri(&source, message_id.as_deref()));
    if result.is_err() && keep_open {
        // The Discord launch never stole focus, so consume the armed flag here
        // to avoid swallowing an unrelated focus-loss event later.
        let _ = hide_guard.take();
    }
    result
}

#[tauri::command]
pub fn list_action_items(
    status_filter: String,
    sort: Option<String>,
    db: State<'_, Db>,
) -> Result<Vec<CanonicalActionItem>, String> {
    db.list_action_items_sorted(&status_filter, sort.as_deref().unwrap_or("newest"))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_manual_action_item(
    title: String,
    app: AppHandle,
    db: State<'_, Db>,
) -> Result<CanonicalActionItem, String> {
    let created = db.create_manual_action(&title).map_err(|e| e.to_string())?;
    if let Ok(items) = db.list_open_action_items() {
        crate::observe_tray_action_items(&app, &items);
        let _ = app.emit("actions:updated", &items);
    }
    Ok(created)
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
        crate::observe_tray_action_items(&app, &items);
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
        crate::observe_tray_action_items(&app, &items);
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
pub async fn test_ai_settings(settings: AppSettings) -> SettingsTestResult {
    crate::ai::test_settings(&settings).await
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

async fn repair_discord_source_metadata(
    app: &AppHandle,
    db: &Db,
    source: DiscordSource,
) -> Result<DiscordSource, String> {
    let settings = crate::settings::load_or_import(app).map_err(|e| e.to_string())?;
    let token = settings.discord_user_token().ok_or_else(|| {
        "Discord user token is required to repair missing channel metadata.".to_string()
    })?;
    let scraper = DiscordScraper::connect(token)
        .await
        .map_err(|e| format!("Discord user token failed: {e}"))?;
    let metadata = scraper
        .fetch_channel_metadata(&source.channel_id)
        .await
        .map_err(|e| format!("Discord channel metadata repair failed: {e}"))?;

    db.update_discord_source_metadata(
        &source.channel_id,
        metadata.channel_name.as_deref(),
        metadata.guild_id.as_deref(),
        source.guild_name.as_deref(),
    )
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "Discord source disappeared during metadata repair.".to_string())
}

fn discord_source_uri(source: &DiscordSource, message_id: Option<&str>) -> String {
    let guild_or_me = source.guild_id.as_deref().unwrap_or("@me");
    let channel_uri = format!("discord:/channels/{guild_or_me}/{}", source.channel_id);
    match message_id {
        Some(message_id) => format!("{channel_uri}/{message_id}"),
        None => channel_uri,
    }
}

fn is_likely_dm_source(source: &DiscordSource) -> bool {
    source
        .channel_name
        .as_deref()
        .is_some_and(|name| name.ends_with("(DM)"))
}

fn open_with_discord(uri: &str) -> Result<(), String> {
    let status = Command::new("open")
        .args(["-a", "Discord", uri])
        .status()
        .map_err(|e| format!("Failed to launch Discord: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Discord open command failed with status {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_source_uri_uses_guild_when_present() {
        let source = DiscordSource {
            channel_id: "channel-1".into(),
            channel_name: Some("dev".into()),
            guild_id: Some("guild-1".into()),
            guild_name: Some("Crumb".into()),
        };

        assert_eq!(
            discord_source_uri(&source, None),
            "discord:/channels/guild-1/channel-1"
        );
    }

    #[test]
    fn discord_source_uri_uses_me_for_dms() {
        let source = DiscordSource {
            channel_id: "dm-1".into(),
            channel_name: Some("Nelly (DM)".into()),
            guild_id: None,
            guild_name: None,
        };

        assert_eq!(
            discord_source_uri(&source, None),
            "discord:/channels/@me/dm-1"
        );
    }

    #[test]
    fn discord_source_uri_can_target_message() {
        let source = DiscordSource {
            channel_id: "channel-1".into(),
            channel_name: Some("dev".into()),
            guild_id: Some("guild-1".into()),
            guild_name: Some("Crumb".into()),
        };

        assert_eq!(
            discord_source_uri(&source, Some("message-1")),
            "discord:/channels/guild-1/channel-1/message-1"
        );
    }
}

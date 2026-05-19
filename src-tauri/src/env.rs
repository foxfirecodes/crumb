// Reads Discord credentials from `.env` (next to the app binary in dev,
// next to the bundle's resource dir in prod). Returns `Ok((bot, app_id, user))`
// where each may be None if the .env entry is empty; the sidecar will reject
// requests that require missing creds.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

pub fn load_discord_env(
    app: &AppHandle,
) -> Result<(Option<String>, Option<String>, Option<String>)> {
    let path = locate_env_file(app)?;
    if !path.exists() {
        return Ok((None, None, None));
    }

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;

    let mut bot = None;
    let mut app_id = None;
    let mut user = None;

    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let value = v.trim().trim_matches('"').trim_matches('\'').to_string();
        let value = if value.is_empty() { None } else { Some(value) };
        match k.trim() {
            "DISCORD_BOT_TOKEN" => bot = value,
            "DISCORD_APP_ID" => app_id = value,
            "DISCORD_USER_TOKEN" => user = value,
            _ => {}
        }
    }

    Ok((bot, app_id, user))
}

/// In dev: repo root (cwd's parent if cwd is src-tauri, else cwd).
/// In prod: app data dir / .env.
fn locate_env_file(app: &AppHandle) -> Result<PathBuf> {
    if cfg!(debug_assertions) {
        let cwd = std::env::current_dir()?;
        // `tauri dev` runs from src-tauri; the project's .env lives one up.
        if cwd.ends_with("src-tauri") {
            return Ok(cwd.parent().unwrap().join(".env"));
        }
        return Ok(cwd.join(".env"));
    }

    let dir = app.path().app_data_dir().context("resolving app data dir")?;
    Ok(dir.join(".env"))
}

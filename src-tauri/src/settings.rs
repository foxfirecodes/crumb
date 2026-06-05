use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default)]
    pub discord_app_id: String,
    #[serde(default)]
    pub discord_bot_token: String,
    #[serde(default)]
    pub discord_user_token: String,
    #[serde(default = "default_ai_model")]
    pub ai_model: String,
    #[serde(default = "default_ai_effort")]
    pub ai_effort: String,
    #[serde(default)]
    pub claude_config_dir: String,
    #[serde(default)]
    pub acp_agent_command: String,
    #[serde(default)]
    pub keep_popover_open_on_view: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsTestResult {
    pub ok: bool,
    pub message: String,
}

impl SettingsTestResult {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            discord_app_id: String::new(),
            discord_bot_token: String::new(),
            discord_user_token: String::new(),
            ai_model: default_ai_model(),
            ai_effort: default_ai_effort(),
            claude_config_dir: String::new(),
            acp_agent_command: String::new(),
            keep_popover_open_on_view: false,
        }
    }
}

impl AppSettings {
    pub fn normalized(mut self) -> Self {
        self.discord_app_id = self.discord_app_id.trim().to_string();
        self.discord_bot_token = self.discord_bot_token.trim().to_string();
        self.discord_user_token = self.discord_user_token.trim().to_string();
        self.ai_model = normalize_ai_model(&self.ai_model);
        self.ai_effort = normalize_ai_effort(&self.ai_effort);
        self.claude_config_dir = self.claude_config_dir.trim().to_string();
        self.acp_agent_command = self.acp_agent_command.trim().to_string();
        self
    }

    pub fn missing_runtime_fields(&self) -> Vec<String> {
        let mut missing = Vec::new();
        if self.discord_app_id.trim().is_empty() {
            missing.push("Application ID".into());
        }
        if self.discord_bot_token.trim().is_empty() {
            missing.push("Bot token".into());
        }
        missing
    }

    pub fn discord_user_token(&self) -> Option<String> {
        non_empty(&self.discord_user_token)
    }

    pub fn claude_config_dir(&self) -> Option<String> {
        non_empty(&self.claude_config_dir)
    }

    pub fn acp_agent_command(&self) -> Option<String> {
        non_empty(&self.acp_agent_command)
    }
}

pub fn load_or_import(app: &AppHandle) -> Result<AppSettings> {
    let path = settings_path(app)?;
    if path.exists() {
        return read_settings(&path);
    }

    if let Some(settings) = load_legacy_env(app)? {
        save(app, &settings)?;
        return Ok(settings);
    }

    Ok(AppSettings::default())
}

pub fn save(app: &AppHandle, settings: &AppSettings) -> Result<AppSettings> {
    let settings = settings.clone().normalized();
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("creating settings dir")?;
    }
    let json = serde_json::to_string_pretty(&settings).context("serializing settings")?;
    fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(settings)
}

pub fn test_settings_shape(settings: &AppSettings) -> SettingsTestResult {
    let settings = settings.clone().normalized();
    let mut missing = settings.missing_runtime_fields();
    if settings.discord_app_id.parse::<u64>().is_err() {
        missing.push("valid numeric Application ID".into());
    }
    if missing.is_empty() {
        SettingsTestResult::ok("Discord settings have the required local shape.")
    } else {
        SettingsTestResult::error(format!("Missing or invalid: {}", missing.join(", ")))
    }
}

fn read_settings(path: &PathBuf) -> Result<AppSettings> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let settings = serde_json::from_str::<AppSettings>(&contents)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(settings.normalized())
}

fn load_legacy_env(app: &AppHandle) -> Result<Option<AppSettings>> {
    let path = legacy_env_path(app)?;
    if !path.exists() {
        return Ok(None);
    }

    let contents =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut settings = AppSettings::default();
    let mut found = false;

    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        match key.trim() {
            "DISCORD_APP_ID" => settings.discord_app_id = value,
            "DISCORD_BOT_TOKEN" => settings.discord_bot_token = value,
            "DISCORD_USER_TOKEN" => settings.discord_user_token = value,
            "CRUMB_AI_MODEL" => settings.ai_model = value,
            "CRUMB_AI_EFFORT" => settings.ai_effort = value,
            "CRUMB_CLAUDE_CONFIG_DIR" => settings.claude_config_dir = value,
            "CRUMB_ACP_AGENT_COMMAND" => settings.acp_agent_command = value,
            "CRUMB_KEEP_POPOVER_OPEN_ON_VIEW" => {
                settings.keep_popover_open_on_view =
                    matches!(value.to_lowercase().as_str(), "1" | "true" | "yes" | "on");
            }
            _ => continue,
        }
        found = true;
    }

    Ok(found.then(|| settings.normalized()))
}

fn settings_path(app: &AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .context("resolving app data dir")?;
    Ok(dir.join("settings.json"))
}

fn legacy_env_path(app: &AppHandle) -> Result<PathBuf> {
    if cfg!(debug_assertions) {
        let cwd = std::env::current_dir()?;
        if cwd.ends_with("src-tauri") {
            return Ok(cwd.parent().unwrap().join(".env"));
        }
        return Ok(cwd.join(".env"));
    }

    let dir = app
        .path()
        .app_data_dir()
        .context("resolving app data dir")?;
    Ok(dir.join(".env"))
}

fn normalize_ai_model(value: &str) -> String {
    let trimmed = value.trim();
    let normalized = trimmed.to_lowercase();
    if normalized.contains("sonnet") || normalized.contains("haiku") {
        trimmed.to_string()
    } else {
        default_ai_model()
    }
}

fn normalize_ai_effort(value: &str) -> String {
    let normalized = value.trim().to_lowercase();
    if matches!(normalized.as_str(), "low" | "medium" | "high" | "xhigh") {
        normalized
    } else {
        default_ai_effort()
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn default_ai_model() -> String {
    "sonnet".into()
}

fn default_ai_effort() -> String {
    "low".into()
}

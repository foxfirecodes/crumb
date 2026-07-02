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
    #[serde(default)]
    pub acp_connector: AcpConnector,
    #[serde(default)]
    pub claude_code: ClaudeCodeSettings,
    #[serde(default)]
    pub codex: CodexSettings,
    #[serde(default)]
    pub custom_acp: CustomAcpSettings,
    #[serde(default)]
    pub keep_popover_open_on_view: bool,
    #[serde(default, skip_serializing)]
    pub ai_model: String,
    #[serde(default, skip_serializing)]
    pub ai_effort: String,
    #[serde(default, skip_serializing)]
    pub claude_config_dir: String,
    #[serde(default, skip_serializing)]
    pub acp_agent_command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AcpConnector {
    ClaudeCode,
    Codex,
    Custom,
}

impl Default for AcpConnector {
    fn default() -> Self {
        Self::ClaudeCode
    }
}

impl AcpConnector {
    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Custom => "Custom ACP",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeSettings {
    #[serde(default = "default_claude_model")]
    pub model: String,
    #[serde(default = "default_claude_effort")]
    pub effort: String,
    #[serde(default)]
    pub config_dir: String,
    #[serde(default)]
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSettings {
    #[serde(default = "default_codex_model")]
    pub model: String,
    #[serde(default = "default_codex_effort")]
    pub effort: String,
    #[serde(default)]
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomAcpSettings {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub env: String,
    #[serde(default)]
    pub session_meta: String,
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
            acp_connector: AcpConnector::default(),
            claude_code: ClaudeCodeSettings::default(),
            codex: CodexSettings::default(),
            custom_acp: CustomAcpSettings::default(),
            keep_popover_open_on_view: false,
            ai_model: String::new(),
            ai_effort: String::new(),
            claude_config_dir: String::new(),
            acp_agent_command: String::new(),
        }
    }
}

impl Default for ClaudeCodeSettings {
    fn default() -> Self {
        Self {
            model: default_claude_model(),
            effort: default_claude_effort(),
            config_dir: String::new(),
            command: String::new(),
        }
    }
}

impl ClaudeCodeSettings {
    pub fn normalized(mut self) -> Self {
        self.model = normalize_claude_model(&self.model);
        self.effort = normalize_claude_effort(&self.effort);
        self.config_dir = self.config_dir.trim().to_string();
        self.command = normalize_acp_command(&self.command);
        self
    }

    pub fn config_dir(&self) -> Option<String> {
        non_empty(&self.config_dir)
    }

    pub fn command(&self) -> Option<String> {
        non_empty(&self.command)
    }
}

impl Default for CodexSettings {
    fn default() -> Self {
        Self {
            model: default_codex_model(),
            effort: default_codex_effort(),
            command: String::new(),
        }
    }
}

impl CodexSettings {
    pub fn normalized(mut self) -> Self {
        self.model = normalize_codex_model(&self.model);
        self.effort = normalize_codex_effort(&self.effort);
        self.command = normalize_acp_command(&self.command);
        self
    }

    pub fn command(&self) -> Option<String> {
        non_empty(&self.command)
    }
}

impl Default for CustomAcpSettings {
    fn default() -> Self {
        Self {
            command: String::new(),
            env: String::new(),
            session_meta: String::new(),
        }
    }
}

impl CustomAcpSettings {
    pub fn normalized(mut self) -> Self {
        self.command = normalize_acp_command(&self.command);
        self.env = normalize_multiline(&self.env);
        self.session_meta = self.session_meta.trim().to_string();
        self
    }

    pub fn command(&self) -> Option<String> {
        non_empty(&self.command)
    }

    pub fn env(&self) -> Option<String> {
        non_empty(&self.env)
    }

    pub fn session_meta(&self) -> Option<String> {
        non_empty(&self.session_meta)
    }
}

impl AppSettings {
    pub fn normalized(mut self) -> Self {
        self.discord_app_id = self.discord_app_id.trim().to_string();
        self.discord_bot_token = self.discord_bot_token.trim().to_string();
        self.discord_user_token = self.discord_user_token.trim().to_string();
        self.migrate_legacy_ai_settings();
        self.claude_code = self.claude_code.normalized();
        self.codex = self.codex.normalized();
        self.custom_acp = self.custom_acp.normalized();
        self.ai_model.clear();
        self.ai_effort.clear();
        self.claude_config_dir.clear();
        self.acp_agent_command.clear();
        self
    }

    fn migrate_legacy_ai_settings(&mut self) {
        let legacy_model = self.ai_model.trim();
        let legacy_effort = self.ai_effort.trim();
        let legacy_config_dir = self.claude_config_dir.trim();
        let legacy_command = self.acp_agent_command.trim();
        if legacy_model.is_empty()
            && legacy_effort.is_empty()
            && legacy_config_dir.is_empty()
            && legacy_command.is_empty()
        {
            return;
        }

        if !legacy_model.is_empty() {
            self.claude_code.model = legacy_model.to_string();
        }
        if !legacy_effort.is_empty() {
            self.claude_code.effort = legacy_effort.to_string();
        }
        if !legacy_config_dir.is_empty() {
            self.claude_code.config_dir = legacy_config_dir.to_string();
        }

        if legacy_command.is_empty() {
            return;
        }

        let command = legacy_command.to_string();
        let normalized = legacy_command.to_ascii_lowercase();
        if normalized.contains("codex") {
            self.acp_connector = AcpConnector::Codex;
            self.codex.command = command;
        } else if normalized.contains("claude") || normalized.contains("anthropic") {
            self.acp_connector = AcpConnector::ClaudeCode;
            self.claude_code.command = command;
        } else {
            self.acp_connector = AcpConnector::Custom;
            self.custom_acp.command = command;
        }
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

fn normalize_claude_model(value: &str) -> String {
    let trimmed = value.trim();
    let normalized = trimmed.to_lowercase();
    if normalized.contains("sonnet") || normalized.contains("haiku") {
        trimmed.to_string()
    } else {
        default_claude_model()
    }
}

fn normalize_claude_effort(value: &str) -> String {
    let normalized = value.trim().to_lowercase();
    if matches!(normalized.as_str(), "low" | "medium" | "high" | "xhigh") {
        normalized
    } else {
        default_claude_effort()
    }
}

fn normalize_codex_model(value: &str) -> String {
    match value.trim() {
        "gpt-5.5" | "gpt-5.4" | "gpt-5.4-mini" => value.trim().to_string(),
        _ => default_codex_model(),
    }
}

fn normalize_codex_effort(value: &str) -> String {
    let normalized = value.trim().to_lowercase();
    if matches!(normalized.as_str(), "low" | "medium" | "high" | "xhigh") {
        normalized
    } else {
        default_codex_effort()
    }
}

fn normalize_acp_command(value: &str) -> String {
    let trimmed = value.trim();
    strip_login_shell_launcher(trimmed)
        .unwrap_or_else(|| trimmed.to_string())
        .trim()
        .to_string()
}

fn strip_login_shell_launcher(value: &str) -> Option<String> {
    ["bash", "zsh"]
        .into_iter()
        .flat_map(|shell| {
            [
                format!("{shell} -ic "),
                format!("{shell} -lc "),
                format!("{shell} -i -c "),
                format!("{shell} -l -c "),
            ]
        })
        .find_map(|prefix| {
            value
                .strip_prefix(&prefix)
                .map(|command| unquote_shell_arg(command.trim()))
        })
}

fn unquote_shell_arg(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1].replace("'\\''", "'");
    }
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        return value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
    }
    value.to_string()
}

fn normalize_multiline(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn default_claude_model() -> String {
    "sonnet".into()
}

fn default_claude_effort() -> String {
    "low".into()
}

fn default_codex_model() -> String {
    "gpt-5.4-mini".into()
}

fn default_codex_effort() -> String {
    "low".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_claude_settings() {
        let raw = r#"{
          "discordAppId": "123",
          "discordBotToken": "bot",
          "aiModel": "haiku",
          "aiEffort": "medium",
          "claudeConfigDir": "~/.claude-crumb",
          "acpAgentCommand": "bash -ic 'npx -y @agentclientprotocol/claude-agent-acp@0.33.1'"
        }"#;

        let settings = serde_json::from_str::<AppSettings>(raw)
            .unwrap()
            .normalized();

        assert_eq!(settings.acp_connector, AcpConnector::ClaudeCode);
        assert_eq!(settings.claude_code.model, "haiku");
        assert_eq!(settings.claude_code.effort, "medium");
        assert_eq!(settings.claude_code.config_dir, "~/.claude-crumb");
        assert_eq!(
            settings.claude_code.command,
            "npx -y @agentclientprotocol/claude-agent-acp@0.33.1"
        );
        assert!(serde_json::to_value(settings)
            .unwrap()
            .get("acpAgentCommand")
            .is_none());
    }

    #[test]
    fn migrates_legacy_codex_command_to_codex_connector() {
        let raw = r#"{
          "discordAppId": "123",
          "discordBotToken": "bot",
          "acpAgentCommand": "bash -ic 'codex-acp-adapter'"
        }"#;

        let settings = serde_json::from_str::<AppSettings>(raw)
            .unwrap()
            .normalized();

        assert_eq!(settings.acp_connector, AcpConnector::Codex);
        assert_eq!(settings.codex.command, "codex-acp-adapter");
        assert!(settings.claude_code.command.is_empty());
    }

    #[test]
    fn codex_defaults_to_low_effort_mini_model() {
        let settings = AppSettings {
            acp_connector: AcpConnector::Codex,
            ..AppSettings::default()
        }
        .normalized();

        assert_eq!(settings.codex.model, "gpt-5.4-mini");
        assert_eq!(settings.codex.effort, "low");
    }

    #[test]
    fn strips_legacy_login_shell_wrappers_from_commands() {
        assert_eq!(
            normalize_acp_command("bash -ic 'npx -y @agentclientprotocol/codex-acp@0.0.44'"),
            "npx -y @agentclientprotocol/codex-acp@0.0.44"
        );
        assert_eq!(
            normalize_acp_command("zsh -lc \"codex-acp --flag\""),
            "codex-acp --flag"
        );
    }
}

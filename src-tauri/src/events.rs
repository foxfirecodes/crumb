// Typed payloads emitted to the frontend over Tauri events.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ScrapeSummary {
    pub id: String,
    pub source: String,
    #[serde(rename = "channelId")]
    pub channel_id: String,
    #[serde(rename = "channelName")]
    pub channel_name: Option<String>,
    #[serde(rename = "guildId")]
    pub guild_id: Option<String>,
    #[serde(rename = "guildName")]
    pub guild_name: Option<String>,
    #[serde(rename = "triggeredBy")]
    pub triggered_by: String,
    #[serde(rename = "triggeredAt")]
    pub triggered_at: i64,
    pub status: String,
    #[serde(rename = "messageCount")]
    pub message_count: Option<i64>,
    pub summary: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub id: String,
    #[serde(rename = "scrapeId")]
    pub scrape_id: String,
    pub text: String,
    pub context: Option<String>,
    #[serde(rename = "messageIds")]
    pub message_ids: Vec<String>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionItem {
    pub id: String,
    #[serde(rename = "scrapeId")]
    pub scrape_id: String,
    pub text: String,
    #[serde(rename = "assigneeKey")]
    pub assignee_key: Option<String>,
    pub assignee: Option<String>,
    pub due: Option<String>,
    #[serde(rename = "messageIds")]
    pub message_ids: Vec<String>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CanonicalActionItem {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(rename = "sourceKind")]
    pub source_kind: String,
    #[serde(rename = "sourceScope")]
    pub source_scope: String,
    #[serde(rename = "sourceLabel")]
    pub source_label: Option<String>,
    #[serde(rename = "assigneeKey")]
    pub assignee_key: Option<String>,
    pub assignee: Option<String>,
    pub due: Option<String>,
    pub priority: i64,
    #[serde(rename = "relevanceScore")]
    pub relevance_score: f64,
    #[serde(rename = "firstSeenAt")]
    pub first_seen_at: i64,
    #[serde(rename = "lastSeenAt")]
    pub last_seen_at: i64,
    #[serde(rename = "completedAt")]
    pub completed_at: Option<i64>,
    #[serde(rename = "snoozedUntil")]
    pub snoozed_until: Option<i64>,
    #[serde(rename = "latestContext")]
    pub latest_context: Option<String>,
    #[serde(rename = "evidenceCount")]
    pub evidence_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScrapeDetail {
    pub scrape: ScrapeSummary,
    pub decisions: Vec<Decision>,
    #[serde(rename = "actionItems")]
    pub action_items: Vec<ActionItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SidecarStatus {
    Starting,
    Connected {
        #[serde(rename = "botUser")]
        bot_user: Option<String>,
        #[serde(rename = "selfUser")]
        self_user: Option<String>,
    },
    Disconnected,
    Error {
        message: String,
    },
}

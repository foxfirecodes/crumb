use agent_client_protocol as acp;
use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, Implementation, InitializeRequest, NewSessionRequest,
    PromptRequest, ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SessionNotification, SessionUpdate, TextContent,
};
use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use crate::discord::{NormalizedMessage, NormalizedPerson};
use crate::events::{CanonicalActionItem, Decision};

const DEFAULT_ACP_AGENT_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp@0.33.1";

const SYSTEM_PROMPT: &str = r#"You are an extraction and reconciliation specialist. You receive a chronological transcript of Discord messages from a single source plus existing records Crumb already knows about from that source. Your job is to identify:

1. DECISIONS: concrete choices made during the conversation. A decision is something the group settled on, not a question or proposal.
2. ACTION ITEMS: concrete things someone committed to do, with assignee, assignee_key, and due date if mentioned.
3. A two-sentence SUMMARY of the conversation.

Rules:
- Be conservative. Only surface things that are clearly decisions or commitments, not idle discussion.
- Reconcile aggressively. If a newly extracted action item is the same real-world task as an existing action item, set "merge_with" to the existing action item's id.
- Treat wording variations as duplicates when the owner, object, and outcome are substantially the same. Example: "provide partner user IDs for whitelisting" and "send Lew partner user IDs to add to the experiment" are the same task.
- Deduplicate within this response too. Return one action item per real-world task and one decision per real-world decision.
- For action item "text", produce a concise canonical title. If "merge_with" is set, prefer the existing title unless the new wording is clearly better.
- You receive current_user_json for the signed-in Discord user and known_people_json with stable Discord user keys. If an action item's responsible party matches a known person by name, username, author, or @ mention, set "assignee_key" to that person's key and "assignee" to their display name.
- If the responsible party is a team or group, set "assignee_key" to "team:" plus a stable lowercase slug. If it is an unknown individual, use "person:" plus a stable lowercase slug. Leave both assignee fields null only when no responsible party is stated.
- For "due", preserve any explicit target date, target day, deadline, or timeframe from the source, including relative values like "today", "this week", or "next Friday". Leave it null only when no target date/timeframe is stated.
- For PR notification action items where the next action is to merge a PR, resolve a merge queue failure, or address human review/BugBot feedback, assign the item to current_user_json.
- If a PR has an approval notification and there is no later merge queue success or successfully merged notification for the same PR in this transcript, create an action item to merge the PR.
- For PR review or BugBot feedback, summarize the requested changes into a short action title and put the PR URL in "url" when present. Do not copy full raw review text into the title.
- For PR notifications, prefer one action item per review or feedback batch, not one per individual comment. Use a new stable dedupe_key for each subsequent distinct human review or BugBot feedback event.
- For decisions, quote the original wording in "context"; do not paraphrase the evidence.
- "dedupe_key" must be stable across repeated scrapes. Use a short lowercase semantic key, not a hash and not a sentence copy.
- For repeated evidence from the same Discord messages, reuse the same "message_ids".
- "message_ids" should list the IDs of the messages that establish each item.
- If there are no decisions or action items, return empty arrays. Do not invent items.
- Do not use tools, inspect files, run commands, browse, or ask follow-up questions.
- Return ONLY JSON matching this shape:
{
  "summary": "string",
  "decisions": [
    { "text": "string", "context": "string", "message_ids": ["string"], "dedupe_key": "string", "merge_with": "string or null" }
  ],
  "action_items": [
    { "text": "string", "assignee": "string", "assignee_key": "string", "due": "string", "url": "string", "message_ids": ["string"], "dedupe_key": "string", "merge_with": "string or null" }
  ]
}"#;

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedDecision {
    pub text: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub message_ids: Option<Vec<String>>,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub merge_with: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedActionItem {
    pub text: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default, alias = "assigneeKey")]
    pub assignee_key: Option<String>,
    #[serde(default, alias = "target_date", alias = "targetDate")]
    pub due: Option<String>,
    #[serde(default, alias = "pr_url", alias = "prUrl", alias = "html_url")]
    pub url: Option<String>,
    #[serde(default)]
    pub message_ids: Option<Vec<String>>,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub merge_with: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Extraction {
    summary: String,
    #[serde(default)]
    decisions: Vec<ExtractedDecision>,
    #[serde(default, rename = "action_items")]
    action_items: Vec<ExtractedActionItem>,
}

#[derive(Debug, Clone)]
pub struct ExtractionResult {
    pub summary: String,
    pub decisions: Vec<ExtractedDecision>,
    pub action_items: Vec<ExtractedActionItem>,
}

pub async fn extract(
    messages: &[NormalizedMessage],
    existing_actions: &[CanonicalActionItem],
    existing_decisions: &[Decision],
    current_user: Option<&NormalizedPerson>,
) -> Result<ExtractionResult> {
    if messages.is_empty() {
        return Ok(ExtractionResult {
            summary: "No messages found.".into(),
            decisions: Vec::new(),
            action_items: Vec::new(),
        });
    }

    let agent = build_acp_agent()?;

    let response = run_acp_prompt(
        agent,
        build_prompt(messages, existing_actions, existing_decisions, current_user)?,
    )
    .await?;
    let json = extract_json_object(&response).with_context(|| {
        format!("Claude ACP response did not contain a JSON object: {response}")
    })?;
    let parsed: Extraction = serde_json::from_str(json).context("extraction schema mismatch")?;

    Ok(ExtractionResult {
        summary: parsed.summary,
        decisions: parsed.decisions,
        action_items: parsed.action_items,
    })
}

async fn run_acp_prompt(agent: acp::AcpAgent, prompt: String) -> Result<String> {
    let output = Arc::new(Mutex::new(String::new()));
    let output_for_handler = output.clone();

    acp::Client
        .builder()
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                if let SessionUpdate::AgentMessageChunk(ContentChunk {
                    content: ContentBlock::Text(text),
                    ..
                }) = notification.update
                {
                    output_for_handler.lock().push_str(&text.text);
                }
                Ok(())
            },
            acp::on_receive_notification!(),
        )
        .on_receive_request(
            async move |_request: RequestPermissionRequest, responder, _connection| {
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ))
            },
            acp::on_receive_request!(),
        )
        .connect_with(agent, move |connection: acp::ConnectionTo<acp::Agent>| {
            let prompt = prompt.clone();
            async move {
                connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1).client_info(
                        Implementation::new("crumb", env!("CARGO_PKG_VERSION")).title("Crumb"),
                    ))
                    .block_task()
                    .await?;

                let session = connection
                    .send_request(
                        NewSessionRequest::new(agent_workspace()).meta(acp_session_meta()),
                    )
                    .block_task()
                    .await?;

                connection
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![ContentBlock::Text(TextContent::new(prompt))],
                    ))
                    .block_task()
                    .await?;

                Ok(())
            }
        })
        .await?;

    let text = output.lock().trim().to_string();
    if text.is_empty() {
        Err(anyhow!("Claude ACP connector returned no text"))
    } else {
        Ok(text)
    }
}

fn agent_workspace() -> PathBuf {
    std::env::temp_dir()
}

fn build_acp_agent() -> Result<acp::AcpAgent> {
    if let Ok(command) = std::env::var("CRUMB_ACP_AGENT_COMMAND") {
        if command.trim_start().starts_with('{') {
            return acp::AcpAgent::from_str(&command)
                .with_context(|| format!("parsing CRUMB_ACP_AGENT_COMMAND: {command}"));
        }

        let command = format!("{} {command}", acp_agent_env_prefix())
            .trim()
            .to_string();
        return acp::AcpAgent::from_str(&command)
            .with_context(|| format!("parsing CRUMB_ACP_AGENT_COMMAND: {command}"));
    }

    let mut args = acp_agent_env_args();
    args.extend(["npx".into(), "-y".into(), DEFAULT_ACP_AGENT_PACKAGE.into()]);
    acp::AcpAgent::from_args(args).context("building pinned Claude ACP command")
}

fn acp_agent_env_args() -> Vec<String> {
    let model = crumb_ai_model();
    let effort = crumb_ai_effort();
    let mut env = vec![
        format!("ANTHROPIC_MODEL={model}"),
        format!("CLAUDE_CODE_EFFORT_LEVEL={effort}"),
        format!("CLAUDE_CODE_SUBAGENT_MODEL={model}"),
        "CLAUDE_CODE_DISABLE_CLAUDE_MDS=1".into(),
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY=1".into(),
        "CLAUDE_CODE_SKIP_PROMPT_HISTORY=1".into(),
        "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1".into(),
        "CLAUDE_CODE_DISABLE_AGENT_VIEW=1".into(),
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1".into(),
        "DISABLE_TELEMETRY=1".into(),
        "MAX_THINKING_TOKENS=0".into(),
    ];
    if let Some(config_dir) = crumb_claude_config_dir() {
        env.push(format!("CLAUDE_CONFIG_DIR={config_dir}"));
    }
    env
}

fn acp_agent_env_prefix() -> String {
    acp_agent_env_args()
        .into_iter()
        .map(|assignment| shell_quote(&assignment))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '=' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn build_prompt(
    messages: &[NormalizedMessage],
    existing_actions: &[CanonicalActionItem],
    existing_decisions: &[Decision],
    current_user: Option<&NormalizedPerson>,
) -> Result<String> {
    let existing_actions = existing_actions
        .iter()
        .map(ExistingActionForPrompt::from)
        .collect::<Vec<_>>();
    let existing_decisions = existing_decisions
        .iter()
        .map(ExistingDecisionForPrompt::from)
        .collect::<Vec<_>>();
    let existing_actions = serde_json::to_string(&existing_actions)?;
    let existing_decisions = serde_json::to_string(&existing_decisions)?;
    let current_user = current_user.map(current_user_for_prompt);
    let current_user_json = serde_json::to_string(&current_user)?;
    let known_people =
        serde_json::to_string(&known_people_from_messages(messages, current_user.as_ref()))?;

    Ok(format!(
        "{SYSTEM_PROMPT}\n\nAnalyze this Discord source transcript and extract decisions, action items, and a summary. Use the existing records to merge duplicates rather than creating new variants.\n\n<current_user_json>\n{current_user_json}\n</current_user_json>\n\n<known_people_json>\n{known_people}\n</known_people_json>\n\n<existing_action_items_json>\n{existing_actions}\n</existing_action_items_json>\n\n<existing_decisions_json>\n{existing_decisions}\n</existing_decisions_json>\n\n<transcript>\n{}\n</transcript>",
        format_transcript(messages)
    ))
}

fn acp_session_meta() -> Map<String, Value> {
    let model = crumb_ai_model();
    let effort = crumb_ai_effort();
    let mut env = Map::new();
    env.insert("ANTHROPIC_MODEL".into(), Value::String(model.clone()));
    env.insert(
        "CLAUDE_CODE_EFFORT_LEVEL".into(),
        Value::String(effort.clone()),
    );
    env.insert(
        "CLAUDE_CODE_SUBAGENT_MODEL".into(),
        Value::String(model.clone()),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_CLAUDE_MDS".into(),
        Value::String("1".into()),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(),
        Value::String("1".into()),
    );
    env.insert(
        "CLAUDE_CODE_SKIP_PROMPT_HISTORY".into(),
        Value::String("1".into()),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS".into(),
        Value::String("1".into()),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_AGENT_VIEW".into(),
        Value::String("1".into()),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
        Value::String("1".into()),
    );
    env.insert("DISABLE_TELEMETRY".into(), Value::String("1".into()));
    env.insert("MAX_THINKING_TOKENS".into(), Value::String("0".into()));
    if let Some(config_dir) = crumb_claude_config_dir() {
        env.insert("CLAUDE_CONFIG_DIR".into(), Value::String(config_dir));
    }

    let meta = json!({
        "disableBuiltInTools": true,
        "claudeCode": {
            "options": {
                "model": model,
                "effortLevel": effort,
                "settingSources": [],
                "tools": [],
                "disallowedTools": [
                    "Bash",
                    "Edit",
                    "MultiEdit",
                    "NotebookEdit",
                    "Read",
                    "Write",
                    "WebFetch",
                    "WebSearch"
                ],
                "settings": {
                    "model": model,
                    "availableModels": [model],
                    "effortLevel": effort,
                    "disableAllHooks": true,
                    "disableBackgroundAgents": true,
                    "alwaysThinkingEnabled": false
                },
                "env": env
            }
        }
    });

    match meta {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn crumb_ai_model() -> String {
    let requested = std::env::var("CRUMB_AI_MODEL").unwrap_or_else(|_| "sonnet".into());
    let normalized = requested.trim().to_lowercase();
    if normalized.contains("sonnet") || normalized.contains("haiku") {
        requested
    } else {
        tracing::warn!("unsupported CRUMB_AI_MODEL={requested}; falling back to sonnet");
        "sonnet".into()
    }
}

fn crumb_ai_effort() -> String {
    let requested = std::env::var("CRUMB_AI_EFFORT").unwrap_or_else(|_| "low".into());
    let normalized = requested.trim().to_lowercase();
    if matches!(normalized.as_str(), "low" | "medium" | "high" | "xhigh") {
        normalized
    } else {
        tracing::warn!("unsupported CRUMB_AI_EFFORT={requested}; falling back to low");
        "low".into()
    }
}

fn crumb_claude_config_dir() -> Option<String> {
    std::env::var("CRUMB_CLAUDE_CONFIG_DIR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExistingActionForPrompt<'a> {
    id: &'a str,
    title: &'a str,
    status: &'a str,
    assignee_key: Option<&'a str>,
    assignee: Option<&'a str>,
    due: Option<&'a str>,
    url: Option<&'a str>,
    latest_context: Option<&'a str>,
}

impl<'a> From<&'a CanonicalActionItem> for ExistingActionForPrompt<'a> {
    fn from(item: &'a CanonicalActionItem) -> Self {
        Self {
            id: &item.id,
            title: &item.title,
            status: &item.status,
            assignee_key: item.assignee_key.as_deref(),
            assignee: item.assignee.as_deref(),
            due: item.due.as_deref(),
            url: item.url.as_deref(),
            latest_context: item.latest_context.as_deref(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExistingDecisionForPrompt<'a> {
    id: &'a str,
    text: &'a str,
    context: Option<&'a str>,
    message_ids: &'a [String],
}

impl<'a> From<&'a Decision> for ExistingDecisionForPrompt<'a> {
    fn from(item: &'a Decision) -> Self {
        Self {
            id: &item.id,
            text: &item.text,
            context: item.context.as_deref(),
            message_ids: &item.message_ids,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnownPersonForPrompt {
    key: String,
    display_name: String,
    username: String,
    aliases: Vec<String>,
}

fn known_people_from_messages(
    messages: &[NormalizedMessage],
    current_user: Option<&KnownPersonForPrompt>,
) -> Vec<KnownPersonForPrompt> {
    let mut people: BTreeMap<String, KnownPersonForPrompt> = BTreeMap::new();

    if let Some(current_user) = current_user {
        people.insert(current_user.key.clone(), current_user.clone());
    }

    for message in messages {
        people.entry(message.author_key.clone()).or_insert_with(|| {
            person_for_prompt(
                &message.author_key,
                &message.author,
                &message.author_username,
                None,
            )
        });

        for mention in &message.mentions {
            people.entry(mention.key.clone()).or_insert_with(|| {
                person_for_prompt(
                    &mention.key,
                    &mention.display_name,
                    &mention.username,
                    Some(&mention.id),
                )
            });
        }
    }

    people.into_values().collect()
}

fn current_user_for_prompt(user: &NormalizedPerson) -> KnownPersonForPrompt {
    let mut person = person_for_prompt(
        &user.key,
        &user.display_name,
        &user.username,
        Some(&user.id),
    );
    person.aliases.push("me".into());
    person.aliases.push("myself".into());
    person.aliases.push("current user".into());
    person.aliases.sort();
    person.aliases.dedup();
    person
}

fn person_for_prompt(
    key: &str,
    display_name: &str,
    username: &str,
    discord_id: Option<&str>,
) -> KnownPersonForPrompt {
    let mut aliases = vec![display_name.to_string(), username.to_string()];
    if let Some(id) = discord_id {
        aliases.push(format!("<@{id}>"));
        aliases.push(format!("<@!{id}>"));
    }
    aliases.sort();
    aliases.dedup();

    KnownPersonForPrompt {
        key: key.to_string(),
        display_name: display_name.to_string(),
        username: username.to_string(),
        aliases,
    }
}

fn format_transcript(messages: &[NormalizedMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            let reply = m
                .reply_to_id
                .as_ref()
                .map(|id| format!(" (reply to {id})"))
                .unwrap_or_default();
            let attachments = if m.attachments.is_empty() {
                String::new()
            } else {
                format!(" [+{} attachment(s)]", m.attachments.len())
            };
            let embeds = if m.embeds.is_empty() {
                String::new()
            } else {
                format!(" [embeds: {}]", m.embeds.join(" || "))
            };
            let components = if m.components.is_empty() {
                String::new()
            } else {
                format!(" [components: {}]", m.components.join(" || "))
            };
            format!(
                "[{}] [{}] <{} | {}>{}{}{}{}: {}",
                m.timestamp,
                m.id,
                m.author,
                m.author_key,
                reply,
                attachments,
                embeds,
                components,
                m.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_json_object(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);

    let start = without_fence.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in without_fence[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return Some(&without_fence[start..end]);
                }
            }
            _ => {}
        }
    }

    None
}

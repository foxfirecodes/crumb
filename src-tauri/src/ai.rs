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
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;

use crate::discord::{NormalizedMessage, NormalizedPerson};
use crate::events::{CanonicalActionItem, Decision};
use crate::settings::{AppSettings, SettingsTestResult};

const DEFAULT_ACP_AGENT_COMMAND: &str =
    "bash -ic 'npx -y @agentclientprotocol/claude-agent-acp@0.33.1'";

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
- For GitHub app-authored PR notifications where the actor/title URL is under github.com/app/ or github.com/apps/ and the next action is to merge a PR, resolve a merge queue failure, or address machine review/BugBot feedback, assign the item to current_user_json.
- For human-authored GitHub PR notifications where the actor/title URL is a github.com/<user> profile, infer the responsible party from the comment/review itself. If the human says they will do the work, assign it to that human; if they ask current_user_json to respond or change something, assign it to current_user_json.
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

const TARGETED_ACTION_PROMPT: &str = r#"TARGETED ACTION MODE

The user explicitly selected one Discord message and asked Crumb to add an action item.

Use the surrounding transcript only as context. The selected message is the anchor.
Derive at most one action item from the selected message and its immediate context.

The action item should represent what current_user_json should do next about the selected message's own ask, request, concern, notification, review, or thing to investigate. Do not replace the selected message's ask with a different question or follow-up from transcript_after, even if it is from the same person or related to the same topic.

Examples:
- If the selected message asks current_user_json a question, create an action item to respond.
- If the selected message asks for investigation, create an action item to investigate or follow up.
- If the selected message reports a problem current_user_json likely owns, create an action item to look into it.
- If context shows the selected message's own ask was already answered or resolved, return no action items, even if transcript_after contains a newer unresolved question.
- If transcript_after contains a later question, create an action item for that later question only when that later question is the selected message.
- If the selected message is unrelated chatter, thanks, an emoji-like response, or has no plausible follow-up, return no action items.

Rules for this mode:
- Do not extract unrelated action items from nearby messages.
- Do not extract tasks from transcript_before or transcript_after. Use those messages only to interpret or resolve the selected message.
- Do not extract decisions.
- Return zero or one action_items entry. Never return more than one.
- Prefer assigning the action item to current_user_json unless the selected message clearly names another responsible person.
- Make the title a concise next action, not a summary of the message.
- Reuse merge_with when this is the same real-world task as an existing action item.
- Use a stable dedupe_key based on the selected message's underlying task, not on wording alone."#;

const NOTED_ACTION_PROMPT: &str = r#"NOTED ACTION MODE

The user explicitly selected one Discord message and provided a note describing the action item Crumb should add.

The user's note is authoritative. Always return exactly one action_items entry, even if the selected message and surrounding transcript would not independently look action-worthy.

Use the surrounding transcript only to clarify the note, enrich the title, infer a mentioned assignee/due date/URL, and understand what the selected message refers to. Do not create extra action items from nearby messages.

Rules for this mode:
- Do not extract decisions.
- Return exactly one action_items entry. Never return zero and never return more than one.
- The action item must be based on user_note_json. If the note is terse, combine it with selected_message and surrounding context into a concise next-action title.
- Do not replace the user's note with a different task from transcript_before or transcript_after.
- Prefer assigning the action item to current_user_json unless the note or selected message clearly names another responsible person.
- Reuse merge_with when this is the same real-world task as an existing action item.
- Use a stable dedupe_key based on both the selected message id and the user's note."#;

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
    settings: &AppSettings,
) -> Result<ExtractionResult> {
    if messages.is_empty() {
        return Ok(ExtractionResult {
            summary: "No messages found.".into(),
            decisions: Vec::new(),
            action_items: Vec::new(),
        });
    }

    let agent = build_acp_agent(settings)?;

    let response = run_acp_prompt(
        agent,
        build_prompt(messages, existing_actions, existing_decisions, current_user)?,
        settings,
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

pub async fn extract_targeted_action(
    messages: &[NormalizedMessage],
    selected_message_id: &str,
    existing_actions: &[CanonicalActionItem],
    existing_decisions: &[Decision],
    current_user: Option<&NormalizedPerson>,
    settings: &AppSettings,
) -> Result<ExtractionResult> {
    if messages.is_empty() {
        return Ok(ExtractionResult {
            summary: "No messages found.".into(),
            decisions: Vec::new(),
            action_items: Vec::new(),
        });
    }

    let agent = build_acp_agent(settings)?;

    let response = run_acp_prompt(
        agent,
        build_targeted_action_prompt(
            messages,
            selected_message_id,
            existing_actions,
            existing_decisions,
            current_user,
        )?,
        settings,
    )
    .await?;
    let json = extract_json_object(&response).with_context(|| {
        format!("Claude ACP response did not contain a JSON object: {response}")
    })?;
    let parsed: Extraction = serde_json::from_str(json).context("extraction schema mismatch")?;

    Ok(ExtractionResult {
        summary: parsed.summary,
        decisions: Vec::new(),
        action_items: parsed.action_items.into_iter().take(1).collect(),
    })
}

pub async fn extract_noted_action(
    messages: &[NormalizedMessage],
    selected_message_id: &str,
    note: &str,
    existing_actions: &[CanonicalActionItem],
    existing_decisions: &[Decision],
    current_user: Option<&NormalizedPerson>,
    settings: &AppSettings,
) -> Result<ExtractionResult> {
    let trimmed_note = note.trim();
    if trimmed_note.is_empty() {
        return Ok(ExtractionResult {
            summary: "No note provided.".into(),
            decisions: Vec::new(),
            action_items: Vec::new(),
        });
    }
    if messages.is_empty() {
        return Ok(ExtractionResult {
            summary: "Added action item from note.".into(),
            decisions: Vec::new(),
            action_items: vec![fallback_noted_action(
                selected_message_id,
                trimmed_note,
                current_user,
            )],
        });
    }

    let parsed = match async {
        let agent = build_acp_agent(settings)?;
        let response = run_acp_prompt(
            agent,
            build_noted_action_prompt(
                messages,
                selected_message_id,
                trimmed_note,
                existing_actions,
                existing_decisions,
                current_user,
            )?,
            settings,
        )
        .await?;
        let json = extract_json_object(&response).with_context(|| {
            format!("Claude ACP response did not contain a JSON object: {response}")
        })?;
        serde_json::from_str::<Extraction>(json).context("extraction schema mismatch")
    }
    .await
    {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!("noted action extraction failed; falling back to note text: {e}");
            return Ok(ExtractionResult {
                summary: "Added action item from note.".into(),
                decisions: Vec::new(),
                action_items: vec![fallback_noted_action(
                    selected_message_id,
                    trimmed_note,
                    current_user,
                )],
            });
        }
    };

    let mut action_items = parsed.action_items.into_iter().take(1).collect::<Vec<_>>();
    if action_items.is_empty() {
        action_items.push(fallback_noted_action(
            selected_message_id,
            trimmed_note,
            current_user,
        ));
    }
    for item in &mut action_items {
        item.text = item.text.trim().to_string();
        if item.text.is_empty() {
            item.text = concise_fallback_note_title(trimmed_note);
        }
        item.message_ids = Some(vec![selected_message_id.to_string()]);
        if item.dedupe_key.as_deref().map_or(true, str::is_empty) {
            item.dedupe_key = Some(noted_action_dedupe_key(selected_message_id, trimmed_note));
        }
        if item.assignee_key.is_none() && item.assignee.is_none() {
            if let Some(user) = current_user {
                item.assignee_key = Some(user.key.clone());
                item.assignee = Some(user.display_name.clone());
            }
        }
    }

    Ok(ExtractionResult {
        summary: parsed.summary,
        decisions: Vec::new(),
        action_items,
    })
}

async fn run_acp_prompt(
    agent: acp::AcpAgent,
    prompt: String,
    settings: &AppSettings,
) -> Result<String> {
    let output = Arc::new(Mutex::new(String::new()));
    let output_for_handler = output.clone();
    let session_meta = acp_session_meta(settings);

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
            let session_meta = session_meta.clone();
            async move {
                connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1).client_info(
                        Implementation::new("crumb", env!("CARGO_PKG_VERSION")).title("Crumb"),
                    ))
                    .block_task()
                    .await?;

                let session = connection
                    .send_request(NewSessionRequest::new(agent_workspace()).meta(session_meta))
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

pub fn test_settings(settings: &AppSettings) -> SettingsTestResult {
    let settings = settings.clone().normalized();
    if settings
        .acp_agent_command()
        .is_some_and(|command| command.trim_start().starts_with('{'))
    {
        return SettingsTestResult::ok("Custom ACP JSON is configured.");
    }

    let command = settings
        .acp_agent_command()
        .unwrap_or_else(|| DEFAULT_ACP_AGENT_COMMAND.to_string());
    let Some(executable) = first_executable(&command) else {
        return SettingsTestResult::error("AI command is empty.");
    };

    match Command::new(&executable).arg("--version").output() {
        Ok(output) if output.status.success() => SettingsTestResult::ok(format!(
            "Found `{executable}`. Claude extraction command is locally launchable."
        )),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("`{executable} --version` exited with {}", output.status)
            } else {
                stderr
            };
            SettingsTestResult::error(format!("Could not validate `{executable}`: {detail}"))
        }
        Err(e) => SettingsTestResult::error(format!(
            "Could not launch `{executable}`. If Crumb is launched from Finder, configure an explicit ACP command path. {e}"
        )),
    }
}

fn build_acp_agent(settings: &AppSettings) -> Result<acp::AcpAgent> {
    let command = settings
        .acp_agent_command()
        .unwrap_or_else(|| DEFAULT_ACP_AGENT_COMMAND.to_string());
    if command.trim_start().starts_with('{') {
        return acp::AcpAgent::from_str(&command)
            .with_context(|| format!("parsing ACP command: {command}"));
    }

    let command = format!("{} {command}", acp_agent_env_prefix(settings))
        .trim()
        .to_string();
    acp::AcpAgent::from_str(&command).with_context(|| format!("parsing ACP command: {command}"))
}

fn acp_agent_env_args(settings: &AppSettings) -> Vec<String> {
    let model = crumb_ai_model(settings);
    let effort = crumb_ai_effort(settings);
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
    if let Some(config_dir) = settings.claude_config_dir() {
        env.push(format!("CLAUDE_CONFIG_DIR={config_dir}"));
    }
    env
}

fn acp_agent_env_prefix(settings: &AppSettings) -> String {
    acp_agent_env_args(settings)
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

fn build_targeted_action_prompt(
    messages: &[NormalizedMessage],
    selected_message_id: &str,
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
    let (before, selected, after) = split_messages_around_selected(messages, selected_message_id);
    let selected_message = selected
        .map(format_message_line)
        .unwrap_or_else(|| format!("Selected message {selected_message_id} was not present."));

    Ok(format!(
        "{SYSTEM_PROMPT}\n\n{TARGETED_ACTION_PROMPT}\n\nIf you return one action item, its message_ids must be exactly [\"{selected_message_id}\"]. Do not include surrounding context message IDs in message_ids.\n\nAnalyze the selected Discord message and surrounding context. Use the existing records to merge duplicates rather than creating new variants.\n\n<selected_message_id>\n{selected_message_id}\n</selected_message_id>\n\n<current_user_json>\n{current_user_json}\n</current_user_json>\n\n<known_people_json>\n{known_people}\n</known_people_json>\n\n<existing_action_items_json>\n{existing_actions}\n</existing_action_items_json>\n\n<existing_decisions_json>\n{existing_decisions}\n</existing_decisions_json>\n\n<transcript_before>\n{}\n</transcript_before>\n\n<selected_message>\n{selected_message}\n</selected_message>\n\n<transcript_after>\n{}\n</transcript_after>",
        format_transcript(before),
        format_transcript(after)
    ))
}

fn build_noted_action_prompt(
    messages: &[NormalizedMessage],
    selected_message_id: &str,
    note: &str,
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
    let note_json = serde_json::to_string(note)?;
    let (before, selected, after) = split_messages_around_selected(messages, selected_message_id);
    let selected_message = selected
        .map(format_message_line)
        .unwrap_or_else(|| format!("Selected message {selected_message_id} was not present."));

    Ok(format!(
        "{SYSTEM_PROMPT}\n\n{NOTED_ACTION_PROMPT}\n\nThe action item's message_ids must be exactly [\"{selected_message_id}\"]. Do not include surrounding context message IDs in message_ids.\n\nAnalyze the user's note, selected Discord message, and surrounding context. Use the existing records to merge duplicates rather than creating new variants.\n\n<selected_message_id>\n{selected_message_id}\n</selected_message_id>\n\n<user_note_json>\n{note_json}\n</user_note_json>\n\n<current_user_json>\n{current_user_json}\n</current_user_json>\n\n<known_people_json>\n{known_people}\n</known_people_json>\n\n<existing_action_items_json>\n{existing_actions}\n</existing_action_items_json>\n\n<existing_decisions_json>\n{existing_decisions}\n</existing_decisions_json>\n\n<transcript_before>\n{}\n</transcript_before>\n\n<selected_message>\n{selected_message}\n</selected_message>\n\n<transcript_after>\n{}\n</transcript_after>",
        format_transcript(before),
        format_transcript(after)
    ))
}

fn acp_session_meta(settings: &AppSettings) -> Map<String, Value> {
    let model = crumb_ai_model(settings);
    let effort = crumb_ai_effort(settings);
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
    if let Some(config_dir) = settings.claude_config_dir() {
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

fn crumb_ai_model(settings: &AppSettings) -> String {
    let requested = settings.ai_model.trim();
    let normalized = requested.trim().to_lowercase();
    if normalized.contains("sonnet") || normalized.contains("haiku") {
        requested.to_string()
    } else {
        tracing::warn!("unsupported AI model setting {requested}; falling back to sonnet");
        "sonnet".into()
    }
}

fn crumb_ai_effort(settings: &AppSettings) -> String {
    let requested = settings.ai_effort.trim();
    let normalized = requested.trim().to_lowercase();
    if matches!(normalized.as_str(), "low" | "medium" | "high" | "xhigh") {
        normalized
    } else {
        tracing::warn!("unsupported AI effort setting {requested}; falling back to low");
        "low".into()
    }
}

fn first_executable(command: &str) -> Option<String> {
    command
        .split_whitespace()
        .map(|part| part.trim_matches('"').trim_matches('\''))
        .find(|part| !part.is_empty() && !looks_like_env_assignment(part))
        .map(ToOwned::to_owned)
}

fn looks_like_env_assignment(part: &str) -> bool {
    let Some((key, _)) = part.split_once('=') else {
        return false;
    };
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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
        .map(format_message_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_message_line(m: &NormalizedMessage) -> String {
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
}

fn split_messages_around_selected<'a>(
    messages: &'a [NormalizedMessage],
    selected_message_id: &str,
) -> (
    &'a [NormalizedMessage],
    Option<&'a NormalizedMessage>,
    &'a [NormalizedMessage],
) {
    let Some(index) = messages
        .iter()
        .position(|message| message.id == selected_message_id)
    else {
        return (messages, None, &[]);
    };
    (
        &messages[..index],
        Some(&messages[index]),
        &messages[index + 1..],
    )
}

fn fallback_noted_action(
    selected_message_id: &str,
    note: &str,
    current_user: Option<&NormalizedPerson>,
) -> ExtractedActionItem {
    ExtractedActionItem {
        text: concise_fallback_note_title(note),
        assignee: current_user.map(|user| user.display_name.clone()),
        assignee_key: current_user.map(|user| user.key.clone()),
        due: None,
        url: None,
        message_ids: Some(vec![selected_message_id.to_string()]),
        dedupe_key: Some(noted_action_dedupe_key(selected_message_id, note)),
        merge_with: None,
    }
}

fn concise_fallback_note_title(note: &str) -> String {
    let trimmed = note.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() <= 160 {
        return trimmed;
    }
    format!("{}...", trimmed.chars().take(157).collect::<String>())
}

fn noted_action_dedupe_key(selected_message_id: &str, note: &str) -> String {
    let mut normalized = String::new();
    let mut last_was_separator = false;
    for ch in note.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            normalized.push('-');
            last_was_separator = true;
        }
        if normalized.len() >= 80 {
            break;
        }
    }
    let normalized = normalized.trim_matches('-');
    if normalized.is_empty() {
        format!("noted-action-{selected_message_id}")
    } else {
        format!("noted-action-{selected_message_id}-{normalized}")
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targeted_prompt_splits_context_and_anchors_message_ids() {
        let messages = vec![
            message("1", "Before context"),
            message("2", "Can you look at this?"),
            message("3", "After context"),
        ];

        let prompt = build_targeted_action_prompt(&messages, "2", &[], &[], None).unwrap();

        assert!(prompt.contains("message_ids must be exactly [\"2\"]"));
        assert!(prompt.contains("<transcript_before>\n[2026-05-26T00:00:00Z] [1]"));
        assert!(prompt.contains("<selected_message>\n[2026-05-26T00:00:00Z] [2]"));
        assert!(prompt.contains("<transcript_after>\n[2026-05-26T00:00:00Z] [3]"));
        assert!(
            prompt.contains("Return zero or one action_items entry. Never return more than one.")
        );
        assert!(prompt.contains(
            "return no action items, even if transcript_after contains a newer unresolved question"
        ));
    }

    #[test]
    fn noted_prompt_includes_note_and_requires_one_anchored_item() {
        let messages = vec![
            message("1", "Before context"),
            message("2", "FYI this changed"),
            message("3", "After context"),
        ];

        let prompt = build_noted_action_prompt(
            &messages,
            "2",
            "Follow up with Ada about the rollout",
            &[],
            &[],
            None,
        )
        .unwrap();

        assert!(prompt.contains("message_ids must be exactly [\"2\"]"));
        assert!(prompt.contains("Return exactly one action_items entry."));
        assert!(prompt.contains("<user_note_json>\n\"Follow up with Ada about the rollout\""));
        assert!(prompt.contains("<selected_message>\n[2026-05-26T00:00:00Z] [2]"));
    }

    #[test]
    fn fallback_noted_action_is_anchored_to_selected_message() {
        let action = fallback_noted_action("123", "Follow up with Ada", None);

        assert_eq!(action.text, "Follow up with Ada");
        assert_eq!(action.message_ids, Some(vec!["123".into()]));
        assert_eq!(
            action.dedupe_key.as_deref(),
            Some("noted-action-123-follow-up-with-ada")
        );
    }

    fn message(id: &str, content: &str) -> NormalizedMessage {
        NormalizedMessage {
            id: id.into(),
            author: "Ada".into(),
            author_key: "discord:user:ada".into(),
            author_username: "ada".into(),
            content: content.into(),
            timestamp: "2026-05-26T00:00:00Z".into(),
            reply_to_id: None,
            attachments: Vec::new(),
            embeds: Vec::new(),
            embed_bodies: Vec::new(),
            components: Vec::new(),
            mentions: Vec::new(),
        }
    }
}

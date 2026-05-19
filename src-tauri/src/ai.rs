use agent_client_protocol as acp;
use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, Implementation, InitializeRequest, NewSessionRequest,
    PromptRequest, ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SessionNotification, SessionUpdate, TextContent,
};
use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use serde::Deserialize;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use crate::discord::NormalizedMessage;

const DEFAULT_ACP_AGENT_COMMAND: &str = "npx -y @agentclientprotocol/claude-agent-acp@0.33.1";

const SYSTEM_PROMPT: &str = r#"You are an extraction specialist. You receive a chronological transcript of Discord messages from a single channel. Your job is to identify:

1. DECISIONS: concrete choices made during the conversation. A decision is something the group settled on, not a question or proposal.
2. ACTION ITEMS: concrete things someone committed to do, with assignee and due date if mentioned.
3. A two-sentence SUMMARY of the conversation.

Rules:
- Be conservative. Only surface things that are clearly decisions or commitments, not idle discussion.
- Quote the original wording in "context"; do not paraphrase decisions.
- "message_ids" should list the IDs of the messages that establish each item.
- If there are no decisions or action items, return empty arrays. Do not invent items.
- Do not use tools, inspect files, run commands, browse, or ask follow-up questions.
- Return ONLY JSON matching this shape:
{
  "summary": "string",
  "decisions": [
    { "text": "string", "context": "string", "message_ids": ["string"] }
  ],
  "action_items": [
    { "text": "string", "assignee": "string", "due": "string", "message_ids": ["string"] }
  ]
}"#;

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedDecision {
    pub text: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub message_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedActionItem {
    pub text: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub message_ids: Option<Vec<String>>,
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

pub async fn extract(messages: &[NormalizedMessage]) -> Result<ExtractionResult> {
    if messages.is_empty() {
        return Ok(ExtractionResult {
            summary: "No messages found.".into(),
            decisions: Vec::new(),
            action_items: Vec::new(),
        });
    }

    let agent = match std::env::var("CRUMB_ACP_AGENT_COMMAND") {
        Ok(command) => acp::AcpAgent::from_str(&command)
            .with_context(|| format!("parsing CRUMB_ACP_AGENT_COMMAND: {command}"))?,
        Err(_) => acp::AcpAgent::from_str(DEFAULT_ACP_AGENT_COMMAND)
            .expect("valid pinned Claude Code ACP command"),
    };

    let response = run_acp_prompt(agent, build_prompt(messages)).await?;
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
                    .send_request(NewSessionRequest::new(agent_workspace()))
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

fn build_prompt(messages: &[NormalizedMessage]) -> String {
    format!(
        "{SYSTEM_PROMPT}\n\nAnalyze this Discord channel transcript and extract decisions, action items, and a summary.\n\n<transcript>\n{}\n</transcript>",
        format_transcript(messages)
    )
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
            format!(
                "[{}] [{}] <{}>{}{}: {}",
                m.timestamp, m.id, m.author, reply, attachments, m.content
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

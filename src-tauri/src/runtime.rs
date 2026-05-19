use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tauri::async_runtime;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, watch};

use crate::ai;
use crate::db::{ActionCandidate, Db, DecisionCandidate};
use crate::discord::{
    DiscordBot, DiscordScraper, NormalizedMessage, NormalizedPerson, ScrapeRequest,
};
use crate::events::SidecarStatus;

#[derive(Clone)]
pub struct RuntimeHandle {
    status: Arc<Mutex<SidecarStatus>>,
    shutdown_tx: watch::Sender<bool>,
}

impl RuntimeHandle {
    pub fn status(&self) -> SidecarStatus {
        self.status.lock().clone()
    }

    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    fn set_status(&self, s: SidecarStatus, app: &AppHandle) {
        *self.status.lock() = s.clone();
        let _ = app.emit("sidecar:status", &s);
    }
}

pub fn spawn(app: AppHandle, db: Db) -> Result<RuntimeHandle> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = RuntimeHandle {
        status: Arc::new(Mutex::new(SidecarStatus::Starting)),
        shutdown_tx,
    };
    handle.set_status(SidecarStatus::Starting, &app);

    let handle_for_task = handle.clone();
    async_runtime::spawn(async move {
        if let Err(e) = run(app.clone(), db, handle_for_task.clone(), shutdown_rx).await {
            tracing::error!("runtime init failed: {e}");
            handle_for_task.set_status(
                SidecarStatus::Error {
                    message: e.to_string(),
                },
                &app,
            );
        }
    });

    Ok(handle)
}

async fn run(
    app: AppHandle,
    db: Db,
    handle: RuntimeHandle,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let (bot_token, app_id, user_token) = crate::env::load_discord_env(&app)?;
    let bot_token = bot_token.ok_or_else(|| anyhow!("missing DISCORD_BOT_TOKEN"))?;
    let app_id = app_id.ok_or_else(|| anyhow!("missing DISCORD_APP_ID"))?;

    let scraper = match user_token {
        Some(token) => match DiscordScraper::connect(token).await {
            Ok(scraper) => {
                tracing::info!(
                    "scraper ready as {}",
                    scraper.user().as_deref().unwrap_or("unknown")
                );
                Some(scraper)
            }
            Err(e) => {
                tracing::warn!(
                    "scraper offline: {e}. /scrape will reject until DISCORD_USER_TOKEN is valid."
                );
                None
            }
        },
        None => {
            tracing::warn!("no DISCORD_USER_TOKEN provided; /scrape will be rejected");
            None
        }
    };

    let bot = DiscordBot::new(app_id.clone(), bot_token);
    bot.register_commands().await?;

    let (scrape_tx, scrape_rx) = mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    async_runtime::spawn(bot.run(scrape_tx, shutdown_rx.clone(), ready_tx));

    let ready = tokio::time::timeout(Duration::from_secs(30), ready_rx)
        .await
        .map_err(|_| anyhow!("Discord bot gateway ready timed out"))?
        .map_err(|_| anyhow!("Discord bot gateway stopped before READY"))?;

    handle.set_status(
        SidecarStatus::Connected {
            bot_user: ready.bot_user,
            self_user: scraper.as_ref().and_then(DiscordScraper::user),
        },
        &app,
    );

    scrape_loop(app.clone(), db, scraper, scrape_rx, shutdown_rx).await;
    handle.set_status(SidecarStatus::Disconnected, &app);
    Ok(())
}

async fn scrape_loop(
    app: AppHandle,
    db: Db,
    scraper: Option<DiscordScraper>,
    mut scrape_rx: mpsc::UnboundedReceiver<ScrapeRequest>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                let _ = changed;
                break;
            }
            req = scrape_rx.recv() => {
                let Some(req) = req else {
                    break;
                };
                let app = app.clone();
                let db = db.clone();
                let scraper = scraper.clone();
                async_runtime::spawn(async move {
                    do_scrape(app, db, scraper, req).await;
                });
            }
        }
    }
}

async fn do_scrape(app: AppHandle, db: Db, scraper: Option<DiscordScraper>, req: ScrapeRequest) {
    match db.insert_running(
        &req.scrape_id,
        &req.channel_id,
        req.channel_name.as_deref(),
        req.guild_id.as_deref(),
        req.guild_name.as_deref(),
        &req.triggered_by,
    ) {
        Ok(summary) => {
            let _ = app.emit("scrape:new", &summary);
        }
        Err(e) => {
            tracing::error!("insert_running: {e}");
            let _ = req.reply.send(format!("Scrape failed: {e}")).await;
            return;
        }
    }

    let Some(scraper) = scraper else {
        let msg = "Scraper is offline. DISCORD_USER_TOKEN is missing or rejected. Re-extract it from the Discord web app and restart Crumb.";
        emit_failed(&app, &db, &req.scrape_id, msg);
        let _ = req.reply.send(msg).await;
        return;
    };
    let current_user = scraper.self_user();

    let result = async {
        let scraped_range = db
            .scraped_message_range(&req.scrape_id)
            .unwrap_or_else(|e| {
                tracing::warn!("failed to load scrape range: {e}");
                Default::default()
            });
        let messages = scraper
            .fetch_channel_messages(&req.channel_id, req.limit, |fetched| {
                tracing::debug!("progress {}: {}", req.scrape_id, fetched);
            })
            .await?;
        let messages = messages
            .into_iter()
            .filter(|message| {
                is_outside_scraped_range(
                    message,
                    scraped_range.first_message_id.as_deref(),
                    scraped_range.last_message_id.as_deref(),
                )
            })
            .collect::<Vec<_>>();

        let _ = req
            .reply
            .send(format!(
                "Scraped {} message{}. Extracting...",
                messages.len(),
                if messages.len() == 1 { "" } else { "s" }
            ))
            .await;

        let existing_actions = db
            .list_source_actions("discord", &req.channel_id)
            .unwrap_or_else(|e| {
                tracing::warn!("failed to load existing source actions: {e}");
                Vec::new()
            });
        let existing_decisions = db
            .list_source_decisions(&req.scrape_id)
            .unwrap_or_else(|e| {
                tracing::warn!("failed to load existing source decisions: {e}");
                Vec::new()
            });

        let extracted = ai::extract(
            &messages,
            &existing_actions,
            &existing_decisions,
            Some(&current_user),
        )
        .await
        .context("Claude extraction failed")?;
        Ok::<_, anyhow::Error>((messages, extracted))
    }
    .await;

    match result {
        Ok((messages, extracted)) => {
            let decisions: Vec<_> = extracted
                .decisions
                .into_iter()
                .map(|d| DecisionCandidate {
                    text: d.text,
                    context: d.context,
                    message_ids: d.message_ids.unwrap_or_default(),
                    dedupe_key: d.dedupe_key,
                    merge_with: d.merge_with,
                })
                .collect();
            let mut action_items: Vec<_> = extracted
                .action_items
                .into_iter()
                .map(|a| {
                    let message_ids = a.message_ids.unwrap_or_default();
                    let url = normalize_pr_url(a.url.as_deref())
                        .or_else(|| find_pr_url_for_action(&messages, &message_ids));

                    ActionCandidate {
                        text: a.text,
                        assignee: a.assignee,
                        assignee_key: a.assignee_key,
                        due: a.due,
                        url,
                        message_ids,
                        dedupe_key: a.dedupe_key,
                        merge_with: a.merge_with,
                    }
                })
                .collect();
            add_pr_notification_fallbacks(&mut action_items, &messages, Some(&current_user));

            match db.mark_extracted(
                &req.scrape_id,
                messages.first().map(|message| message.id.as_str()),
                messages.last().map(|message| message.id.as_str()),
                messages.len() as i64,
                &extracted.summary,
                &decisions,
                &action_items,
            ) {
                Ok(updated) => {
                    let _ = app.emit("scrape:updated", &updated);
                    if let Ok(actions) = db.list_open_action_items() {
                        let _ = app.emit("actions:updated", &actions);
                    }
                    let _ = req
                        .reply
                        .send(format!(
                            "Done: {} messages, {} decision{}, {} action item{}. Open Crumb to view.",
                            messages.len(),
                            decisions.len(),
                            if decisions.len() == 1 { "" } else { "s" },
                            action_items.len(),
                            if action_items.len() == 1 { "" } else { "s" }
                        ))
                        .await;
                }
                Err(e) => {
                    tracing::error!("mark_extracted: {e}");
                    emit_failed(&app, &db, &req.scrape_id, &e.to_string());
                    let _ = req.reply.send(format!("Scrape failed: {e}")).await;
                }
            }
        }
        Err(e) => {
            let msg = e.to_string();
            let user_msg = user_facing_scrape_error(&msg);
            tracing::error!("scrape failed: {msg}");
            emit_failed(&app, &db, &req.scrape_id, &user_msg);
            let _ = req.reply.send(format!("Scrape failed: {user_msg}")).await;
        }
    }
}

fn is_outside_scraped_range(
    message: &NormalizedMessage,
    first_message_id: Option<&str>,
    last_message_id: Option<&str>,
) -> bool {
    match (first_message_id, last_message_id) {
        (Some(first), Some(last)) => {
            compare_snowflake_ids(&message.id, first).is_lt()
                || compare_snowflake_ids(&message.id, last).is_gt()
        }
        (Some(first), None) => compare_snowflake_ids(&message.id, first).is_lt(),
        (None, Some(last)) => compare_snowflake_ids(&message.id, last).is_gt(),
        (None, None) => true,
    }
}

fn compare_snowflake_ids(a: &str, b: &str) -> std::cmp::Ordering {
    let parsed = a.parse::<u128>().ok().zip(b.parse::<u128>().ok());
    match parsed {
        Some((a, b)) => a.cmp(&b),
        None => a.cmp(b),
    }
}

#[derive(Debug, Default)]
struct PrApprovalState {
    url: String,
    approval_message_ids: Vec<String>,
    merged_after_approval: bool,
}

fn add_pr_notification_fallbacks(
    action_items: &mut Vec<ActionCandidate>,
    messages: &[NormalizedMessage],
    current_user: Option<&NormalizedPerson>,
) {
    let mut approvals: BTreeMap<String, PrApprovalState> = BTreeMap::new();

    for message in messages {
        let Some(url) = find_pr_url_in_message(message) else {
            continue;
        };
        let text = notification_text(message);
        let key = stable_pr_key(&url);
        let state = approvals.entry(key).or_insert_with(|| PrApprovalState {
            url: url.clone(),
            ..Default::default()
        });

        if is_pr_approval_notification(&text) {
            state.approval_message_ids = vec![message.id.clone()];
            state.merged_after_approval = false;
        }
        if is_pr_merge_success_notification(&text) && !state.approval_message_ids.is_empty() {
            state.merged_after_approval = true;
        }
    }

    for state in approvals.into_values() {
        if state.approval_message_ids.is_empty() || state.merged_after_approval {
            continue;
        }
        if action_items.iter().any(|item| {
            item.url.as_deref() == Some(state.url.as_str()) && is_merge_action(&item.text)
        }) {
            continue;
        }

        action_items.push(ActionCandidate {
            text: format!("Merge approved PR {}", pr_label(&state.url)),
            assignee_key: current_user.map(|user| user.key.clone()),
            assignee: current_user.map(|user| user.display_name.clone()),
            due: None,
            url: Some(state.url.clone()),
            message_ids: state.approval_message_ids,
            dedupe_key: Some(format!("merge-approved-pr-{}", stable_pr_key(&state.url))),
            merge_with: None,
        });
    }

    if let Some(current_user) = current_user {
        for item in action_items.iter_mut().filter(|item| {
            item.url
                .as_deref()
                .and_then(|url| normalize_pr_url(Some(url)))
                .is_some()
        }) {
            item.assignee_key = Some(current_user.key.clone());
            item.assignee = Some(current_user.display_name.clone());
        }
    }
}

fn notification_text(message: &NormalizedMessage) -> String {
    let mut parts = vec![message.content.as_str()];
    parts.extend(message.embeds.iter().map(String::as_str));
    parts.extend(message.components.iter().map(String::as_str));
    parts.join(" ").to_lowercase()
}

fn is_pr_approval_notification(text: &str) -> bool {
    text.contains("approved")
        && !text.contains("not approved")
        && !text.contains("approval denied")
        && !text.contains("changes requested")
        && (text.contains("/pull/") || text.contains("pull request") || text.contains(" pr "))
}

fn is_pr_merge_success_notification(text: &str) -> bool {
    text.contains("successfully merged")
        || text.contains("merged pull request")
        || (text.contains("merge queue") && text.contains(" merged"))
}

fn is_merge_action(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("merge") && (text.contains("pr") || text.contains("pull request"))
}

fn find_pr_url_for_action(
    messages: &[NormalizedMessage],
    message_ids: &[String],
) -> Option<String> {
    let selected = messages
        .iter()
        .filter(|message| message_ids.iter().any(|id| id == &message.id))
        .collect::<Vec<_>>();
    let candidates = if selected.is_empty() {
        messages.iter().collect::<Vec<_>>()
    } else {
        selected
    };

    candidates.into_iter().find_map(find_pr_url_in_message)
}

fn find_pr_url_in_message(message: &NormalizedMessage) -> Option<String> {
    extract_pr_url(&message.content)
        .or_else(|| {
            message
                .embeds
                .iter()
                .find_map(|value| extract_pr_url(value))
        })
        .or_else(|| {
            message
                .components
                .iter()
                .find_map(|value| extract_pr_url(value))
        })
        .or_else(|| {
            message
                .attachments
                .iter()
                .find_map(|value| extract_pr_url(value))
        })
}

fn extract_pr_url(value: &str) -> Option<String> {
    let mut offset = 0;
    while offset < value.len() {
        let Some(start) = next_url_start(&value[offset..]).map(|start| start + offset) else {
            break;
        };
        let end = value[start..]
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | ')' | ']'))
            .map(|end| start + end)
            .unwrap_or(value.len());
        if let Some(url) = normalize_pr_url(Some(&value[start..end])) {
            return Some(url);
        }
        offset = end.saturating_add(1);
    }
    None
}

fn next_url_start(value: &str) -> Option<usize> {
    [
        value.find("https://"),
        value.find("http://"),
        value.find("github.com/"),
    ]
    .into_iter()
    .flatten()
    .min()
}

fn normalize_pr_url(value: Option<&str>) -> Option<String> {
    let raw = value
        .map(str::trim)?
        .trim_matches(|ch: char| matches!(ch, '<' | '>' | ')' | ']' | '.' | ',' | ';' | ':'));
    let normalized;
    let value = if raw.starts_with("github.com/") {
        normalized = format!("https://{raw}");
        normalized.as_str()
    } else {
        raw
    };
    if !value.starts_with("https://") && !value.starts_with("http://") {
        return None;
    }
    let pull_index = value.find("/pull/")?;
    let number_start = pull_index + "/pull/".len();
    let number_len = value[number_start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    if number_len == 0 {
        return None;
    }
    Some(value[..number_start + number_len].to_string())
}

fn stable_pr_key(url: &str) -> String {
    normalize_pr_url(Some(url))
        .unwrap_or_else(|| url.to_string())
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn pr_label(url: &str) -> String {
    normalize_pr_url(Some(url))
        .and_then(|url| {
            url.rsplit_once("/pull/")
                .map(|(_, number)| format!("#{number}"))
        })
        .unwrap_or_else(|| "PR".into())
}

fn user_facing_scrape_error(error: &str) -> String {
    if error.contains("Authentication required") {
        return "Claude Code authentication is required for extraction. Run `claude` in a terminal and complete login, or unset/fix `CRUMB_CLAUDE_CONFIG_DIR` if it points at an unauthenticated config.".into();
    }
    error.into()
}

fn emit_failed(app: &AppHandle, db: &Db, scrape_id: &str, error: &str) {
    match db.mark_failed(scrape_id, error) {
        Ok(updated) => {
            let _ = app.emit("scrape:updated", &updated);
        }
        Err(e) => tracing::error!("mark_failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_canonical_pr_url_from_message_content() {
        let message = message(
            "1",
            "BugBot commented on https://github.com/example/repo/pull/123#discussion_r456.",
        );

        assert_eq!(
            find_pr_url_for_action(&[message], &["1".into()]).as_deref(),
            Some("https://github.com/example/repo/pull/123")
        );
    }

    #[test]
    fn extracts_canonical_pr_url_from_embed_content() {
        let mut message = message("1", "");
        message.embeds =
            vec!["Review submitted | https://github.com/example/repo/pull/456/files".into()];

        assert_eq!(
            find_pr_url_for_action(&[message], &["1".into()]).as_deref(),
            Some("https://github.com/example/repo/pull/456")
        );
    }

    #[test]
    fn adds_merge_action_for_approved_pr_without_later_merge_success() {
        let mut actions = Vec::new();
        let mut approved = message("1", "");
        approved.embeds =
            vec!["Ada approved pull request https://github.com/example/repo/pull/789".into()];
        let current_user = current_user();

        add_pr_notification_fallbacks(&mut actions, &[approved], Some(&current_user));

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].text, "Merge approved PR #789");
        assert_eq!(
            actions[0].assignee_key.as_deref(),
            Some("discord:user:current")
        );
        assert_eq!(actions[0].assignee.as_deref(), Some("Current User"));
        assert_eq!(
            actions[0].url.as_deref(),
            Some("https://github.com/example/repo/pull/789")
        );
    }

    #[test]
    fn skips_merge_action_for_approved_pr_with_later_merge_success() {
        let mut actions = Vec::new();
        let mut approved = message("1", "");
        approved.embeds =
            vec!["Ada approved pull request https://github.com/example/repo/pull/789".into()];
        let mut merged = message("2", "");
        merged.embeds =
            vec!["Merge queue successfully merged https://github.com/example/repo/pull/789".into()];
        let current_user = current_user();

        add_pr_notification_fallbacks(&mut actions, &[approved, merged], Some(&current_user));

        assert!(actions.is_empty());
    }

    #[test]
    fn assigns_pr_review_actions_to_current_user() {
        let mut actions = vec![ActionCandidate {
            text: "Address BugBot feedback".into(),
            assignee_key: Some("discord:user:bugbot".into()),
            assignee: Some("BugBot".into()),
            due: None,
            url: Some("https://github.com/example/repo/pull/456".into()),
            message_ids: vec!["1".into()],
            dedupe_key: Some("bugbot-feedback-456".into()),
            merge_with: None,
        }];
        let current_user = current_user();

        add_pr_notification_fallbacks(&mut actions, &[], Some(&current_user));

        assert_eq!(
            actions[0].assignee_key.as_deref(),
            Some("discord:user:current")
        );
        assert_eq!(actions[0].assignee.as_deref(), Some("Current User"));
    }

    fn message(id: &str, content: &str) -> NormalizedMessage {
        NormalizedMessage {
            id: id.into(),
            author: "Nelly".into(),
            author_key: "discord:user:267054589960912898".into(),
            author_username: "nelly".into(),
            content: content.into(),
            timestamp: "2026-05-19T00:00:00Z".into(),
            reply_to_id: None,
            attachments: Vec::new(),
            embeds: Vec::new(),
            components: Vec::new(),
            mentions: Vec::new(),
        }
    }

    fn current_user() -> NormalizedPerson {
        NormalizedPerson {
            id: "current".into(),
            key: "discord:user:current".into(),
            display_name: "Current User".into(),
            username: "current".into(),
        }
    }
}

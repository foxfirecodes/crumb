use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::async_runtime;
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval_at, Instant, MissedTickBehavior};

use crate::ai;
use crate::db::{ActionCandidate, Db, DecisionCandidate, WatchedChannel};
use crate::discord::{
    DiscordBot, DiscordCommand, DiscordScraper, NormalizedMessage, NormalizedPerson, ScrapeRequest,
    WatchRequest,
};
use crate::events::{CanonicalActionItem, ScrapeSummary, SidecarStatus};
use crate::settings::AppSettings;

const WATCH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const WATCH_FETCH_LIMIT: usize = 100;
const TARGETED_MESSAGE_CONTEXT_LIMIT: usize = 11;

#[derive(Clone)]
pub struct RuntimeHandle {
    status: Arc<Mutex<SidecarStatus>>,
    shutdown_tx: watch::Sender<bool>,
    active: Arc<AtomicBool>,
}

impl RuntimeHandle {
    pub fn status(&self) -> SidecarStatus {
        self.status.lock().clone()
    }

    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    fn deactivate(&self) {
        self.active.store(false, Ordering::SeqCst);
    }

    fn set_status(&self, s: SidecarStatus, app: &AppHandle) {
        if !self.active.load(Ordering::SeqCst) {
            return;
        }
        *self.status.lock() = s.clone();
        let _ = app.emit("sidecar:status", &s);
    }
}

#[derive(Clone)]
pub struct RuntimeManager {
    app: AppHandle,
    db: Db,
    current: Arc<Mutex<RuntimeHandle>>,
}

impl RuntimeManager {
    pub fn start(app: AppHandle, db: Db) -> Result<Self> {
        let handle = spawn(app.clone(), db.clone())?;
        Ok(Self {
            app,
            db,
            current: Arc::new(Mutex::new(handle)),
        })
    }

    pub fn status(&self) -> SidecarStatus {
        self.current.lock().status()
    }

    pub async fn restart(&self) -> Result<()> {
        let old = self.current.lock().clone();
        old.deactivate();
        old.shutdown().await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let next = spawn(self.app.clone(), self.db.clone())?;
        *self.current.lock() = next;
        Ok(())
    }

    pub async fn shutdown(&self) {
        let current = self.current.lock().clone();
        current.shutdown().await;
    }
}

pub fn spawn(app: AppHandle, db: Db) -> Result<RuntimeHandle> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = RuntimeHandle {
        status: Arc::new(Mutex::new(SidecarStatus::Starting)),
        shutdown_tx,
        active: Arc::new(AtomicBool::new(true)),
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
    let settings = crate::settings::load_or_import(&app)?;
    let missing = settings.missing_runtime_fields();
    if !missing.is_empty() {
        handle.set_status(SidecarStatus::NeedsSetup { missing }, &app);
        let mut shutdown_rx = shutdown_rx;
        let _ = shutdown_rx.changed().await;
        return Ok(());
    }

    let bot_token = settings.discord_bot_token.clone();
    let app_id = settings.discord_app_id.clone();

    let scraper = match settings.discord_user_token() {
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
                    "scraper offline: {e}. /scrape will reject until the Discord user token is valid."
                );
                None
            }
        },
        None => {
            tracing::warn!("no Discord user token provided; /scrape will be rejected");
            None
        }
    };

    let bot = DiscordBot::new(app_id.clone(), bot_token);
    bot.register_commands().await?;

    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    async_runtime::spawn(bot.run(command_tx, shutdown_rx.clone(), ready_tx));

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

    let (work_tx, work_rx) = mpsc::unbounded_channel();
    async_runtime::spawn(command_queue_loop(
        command_rx,
        work_tx.clone(),
        shutdown_rx.clone(),
    ));
    async_runtime::spawn(watch_scheduler_loop(
        db.clone(),
        work_tx,
        shutdown_rx.clone(),
    ));
    work_loop(app.clone(), db, scraper, settings, work_rx, shutdown_rx).await;
    handle.set_status(SidecarStatus::Disconnected, &app);
    Ok(())
}

enum WorkItem {
    Scrape(ScrapeRequest),
    Watch(WatchRequest),
    Unwatch(WatchRequest),
    Poll(WatchedChannel),
}

async fn command_queue_loop(
    mut command_rx: mpsc::UnboundedReceiver<DiscordCommand>,
    work_tx: mpsc::UnboundedSender<WorkItem>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                let _ = changed;
                break;
            }
            command = command_rx.recv() => {
                let Some(command) = command else {
                    break;
                };
                let item = match command {
                    DiscordCommand::Scrape(req) => WorkItem::Scrape(req),
                    DiscordCommand::Watch(req) => WorkItem::Watch(req),
                    DiscordCommand::Unwatch(req) => WorkItem::Unwatch(req),
                };
                if work_tx.send(item).is_err() {
                    break;
                }
            }
        }
    }
}

async fn watch_scheduler_loop(
    db: Db,
    work_tx: mpsc::UnboundedSender<WorkItem>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut interval = interval_at(Instant::now() + WATCH_INTERVAL, WATCH_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                let _ = changed;
                break;
            }
            _ = interval.tick() => {
                let watched = match db.list_watched_channels() {
                    Ok(watched) => watched,
                    Err(e) => {
                        tracing::warn!("failed to list watched channels: {e}");
                        continue;
                    }
                };
                for channel in watched {
                    tracing::debug!(
                        "queueing watch poll for {} watched by {} at {} last polled {:?}",
                        channel.channel_id,
                        channel.watched_by,
                        channel.watched_at,
                        channel.last_polled_at
                    );
                    if work_tx.send(WorkItem::Poll(channel)).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

async fn work_loop(
    app: AppHandle,
    db: Db,
    scraper: Option<DiscordScraper>,
    settings: AppSettings,
    mut work_rx: mpsc::UnboundedReceiver<WorkItem>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                let _ = changed;
                break;
            }
            item = work_rx.recv() => {
                let Some(item) = item else {
                    break;
                };
                match item {
                    WorkItem::Scrape(req) => do_scrape(app.clone(), db.clone(), scraper.clone(), settings.clone(), req).await,
                    WorkItem::Watch(req) => do_watch(db.clone(), scraper.clone(), req).await,
                    WorkItem::Unwatch(req) => do_unwatch(db.clone(), req).await,
                    WorkItem::Poll(channel) => do_watch_poll(app.clone(), db.clone(), scraper.clone(), settings.clone(), channel).await,
                }
            }
        }
    }
}

async fn do_scrape(
    app: AppHandle,
    db: Db,
    scraper: Option<DiscordScraper>,
    settings: AppSettings,
    req: ScrapeRequest,
) {
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
        let msg = "Scraper is offline. The Discord user token is missing or rejected. Re-extract it from the Discord web app and restart Crumb.";
        emit_failed(&app, &db, &req.scrape_id, msg);
        let _ = req.reply.send(msg).await;
        return;
    };
    let current_user = scraper.self_user();

    let result = async {
        let is_targeted_message = req.target_message_id.is_some();
        let messages = if let Some(target_message_id) = req.target_message_id.as_deref() {
            scraper
                .fetch_channel_messages_around(
                    &req.channel_id,
                    target_message_id,
                    TARGETED_MESSAGE_CONTEXT_LIMIT,
                    req.target_message.clone(),
                )
                .await?
        } else {
            let scraped_range = db
                .scraped_message_range(&req.scrape_id)
                .unwrap_or_else(|e| {
                    tracing::warn!("failed to load scrape range: {e}");
                    Default::default()
                });
            scraper
                .fetch_channel_messages(&req.channel_id, req.limit, |fetched| {
                    tracing::debug!("progress {}: {}", req.scrape_id, fetched);
                })
                .await?
                .into_iter()
                .filter(|message| {
                    is_outside_scraped_range(
                        message,
                        scraped_range.first_message_id.as_deref(),
                        scraped_range.last_message_id.as_deref(),
                    )
                })
                .collect::<Vec<_>>()
        };

        let _ = req
            .reply
            .send(format!(
                "Scraped {} message{}. Extracting...",
                messages.len(),
                if messages.len() == 1 { "" } else { "s" }
            ))
            .await;

        extract_and_store(
            &app,
            &db,
            &current_user,
            &req.scrape_id,
            &req.channel_id,
            &messages,
            &settings,
            req.target_message_id.as_deref(),
            !is_targeted_message,
        )
        .await
        .context("extraction failed")
    }
    .await;

    match result {
        Ok(outcome) => {
            notify_new_actions(
                &app,
                outcome.new_action_count,
                outcome.source_label.as_deref(),
            );
            let _ = req
                .reply
                .send(format!(
                    "Done: {} messages, {} decision{}, {} action item{}. Open Crumb to view.",
                    outcome.message_count,
                    outcome.decision_count,
                    if outcome.decision_count == 1 { "" } else { "s" },
                    outcome.action_count,
                    if outcome.action_count == 1 { "" } else { "s" }
                ))
                .await;
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

async fn do_watch(db: Db, scraper: Option<DiscordScraper>, req: WatchRequest) {
    let Some(scraper) = scraper else {
        let msg = "Watcher is offline. The Discord user token is missing or rejected. Re-extract it from the Discord web app and restart Crumb.";
        let _ = req.reply.send(msg).await;
        return;
    };

    let result = async {
        let latest = scraper
            .fetch_channel_messages(&req.channel_id, 1, |_| {})
            .await
            .context("fetching latest message for watch cursor")?;
        let cursor = latest
            .last()
            .map(|message| message.id.as_str())
            .unwrap_or(req.interaction_id.as_str());
        db.watch_channel(
            &req.channel_id,
            req.channel_name.as_deref(),
            req.guild_id.as_deref(),
            req.guild_name.as_deref(),
            &req.triggered_by,
            Some(cursor),
        )?;
        Ok::<_, anyhow::Error>(cursor.to_string())
    }
    .await;

    match result {
        Ok(_) => {
            let label = channel_label(
                req.guild_name.as_deref(),
                req.channel_name.as_deref(),
                &req.channel_id,
            );
            let _ = req
                .reply
                .send(format!(
                    "Watching {label}. I'll only process messages posted after now."
                ))
                .await;
        }
        Err(e) => {
            tracing::error!("watch failed: {e}");
            let _ = req.reply.send(format!("Watch failed: {e}")).await;
        }
    }
}

async fn do_unwatch(db: Db, req: WatchRequest) {
    match db.unwatch_channel(&req.channel_id) {
        Ok(true) => {
            let label = channel_label(
                req.guild_name.as_deref(),
                req.channel_name.as_deref(),
                &req.channel_id,
            );
            let _ = req.reply.send(format!("Stopped watching {label}.")).await;
        }
        Ok(false) => {
            let _ = req.reply.send("This channel was not being watched.").await;
        }
        Err(e) => {
            tracing::error!("unwatch failed: {e}");
            let _ = req.reply.send(format!("Unwatch failed: {e}")).await;
        }
    }
}

async fn do_watch_poll(
    app: AppHandle,
    db: Db,
    scraper: Option<DiscordScraper>,
    settings: AppSettings,
    channel: WatchedChannel,
) {
    let Some(scraper) = scraper else {
        tracing::warn!("skipping watch poll; scraper is offline");
        return;
    };
    let current_user = scraper.self_user();
    let scrape_id = format!("{}:{}", channel.source, channel.channel_id);

    let result = async {
        let Some(cursor) = channel.last_seen_message_id.as_deref() else {
            let latest = scraper
                .fetch_channel_messages(&channel.channel_id, 1, |_| {})
                .await
                .context("fetching latest watched channel cursor")?;
            db.update_watch_cursor(
                &channel.channel_id,
                latest.last().map(|message| message.id.as_str()),
            )?;
            return Ok::<_, anyhow::Error>(None);
        };
        let fetched = scraper
            .fetch_channel_messages_after(&channel.channel_id, cursor, WATCH_FETCH_LIMIT, |_| {})
            .await
            .context("fetching watched channel messages after cursor")?;
        let messages = filter_messages_after_cursor(fetched, cursor);

        if messages.is_empty() {
            db.update_watch_cursor(&channel.channel_id, None)?;
            return Ok(None);
        }

        let summary = db.insert_running(
            &scrape_id,
            &channel.channel_id,
            channel.channel_name.as_deref(),
            channel.guild_id.as_deref(),
            channel.guild_name.as_deref(),
            "watcher",
        )?;
        let _ = app.emit("scrape:new", &summary);

        let outcome = extract_and_store(
            &app,
            &db,
            &current_user,
            &scrape_id,
            &channel.channel_id,
            &messages,
            &settings,
            None,
            true,
        )
        .await?;
        db.update_watch_cursor(
            &channel.channel_id,
            messages.last().map(|message| message.id.as_str()),
        )?;
        Ok(Some(outcome))
    }
    .await;

    match result {
        Ok(Some(outcome)) => {
            notify_new_actions(
                &app,
                outcome.new_action_count,
                outcome.source_label.as_deref(),
            );
        }
        Ok(None) => {}
        Err(e) => {
            tracing::error!("watch poll failed for {}: {e}", channel.channel_id);
            emit_failed(&app, &db, &scrape_id, &e.to_string());
        }
    }
}

struct ExtractionOutcome {
    message_count: usize,
    decision_count: usize,
    action_count: usize,
    new_action_count: usize,
    source_label: Option<String>,
}

async fn extract_and_store(
    app: &AppHandle,
    db: &Db,
    current_user: &NormalizedPerson,
    scrape_id: &str,
    channel_id: &str,
    messages: &[NormalizedMessage],
    settings: &AppSettings,
    selected_message_id: Option<&str>,
    update_message_range: bool,
) -> Result<ExtractionOutcome> {
    let existing_actions = db.list_source_actions("discord", channel_id)?;
    let existing_action_ids = existing_actions
        .iter()
        .map(|action| action.id.clone())
        .collect::<HashSet<_>>();
    let existing_action_statuses = existing_actions
        .iter()
        .map(|action| (action.id.clone(), action.status.clone()))
        .collect::<HashMap<_, _>>();
    let existing_decisions = db.list_source_decisions(scrape_id)?;

    let extracted = if let Some(selected_message_id) = selected_message_id {
        ai::extract_targeted_action(
            messages,
            selected_message_id,
            &existing_actions,
            &existing_decisions,
            Some(current_user),
            settings,
        )
        .await?
    } else {
        ai::extract(
            messages,
            &existing_actions,
            &existing_decisions,
            Some(current_user),
            settings,
        )
        .await?
    };

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
                .or_else(|| find_pr_url_for_action(messages, &message_ids));

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
    if selected_message_id.is_none() {
        add_pr_notification_fallbacks(&mut action_items, messages, Some(current_user));
        reconcile_pr_merge_outcomes(&existing_actions, &mut action_items, messages, current_user);
    }

    let updated = db.mark_extracted(
        scrape_id,
        update_message_range
            .then(|| messages.first().map(|message| message.id.as_str()))
            .flatten(),
        update_message_range
            .then(|| messages.last().map(|message| message.id.as_str()))
            .flatten(),
        messages.len() as i64,
        &extracted.summary,
        &decisions,
        &action_items,
    )?;
    let after_actions = db.list_source_actions("discord", channel_id)?;
    let auto_dismissed_count = if selected_message_id.is_some() {
        0
    } else {
        dismiss_resolved_pr_merge_actions(db, &after_actions, messages)?
    };
    let final_actions = if auto_dismissed_count > 0 {
        db.list_source_actions("discord", channel_id)?
    } else {
        after_actions
    };
    let new_action_count = final_actions
        .iter()
        .filter(|action| {
            !existing_action_ids.contains(&action.id)
                || existing_action_statuses
                    .get(&action.id)
                    .is_some_and(|status| matches!(status.as_str(), "done" | "archived"))
                    && matches!(action.status.as_str(), "inbox" | "active")
        })
        .count();

    let _ = app.emit("scrape:updated", &updated);
    if let Ok(actions) = db.list_open_action_items() {
        let _ = app.emit("actions:updated", &actions);
    }

    Ok(ExtractionOutcome {
        message_count: messages.len(),
        decision_count: decisions.len(),
        action_count: action_items.len(),
        new_action_count,
        source_label: Some(source_label_from_summary(&updated)),
    })
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

fn filter_messages_after_cursor(
    messages: Vec<NormalizedMessage>,
    cursor: &str,
) -> Vec<NormalizedMessage> {
    messages
        .into_iter()
        .filter(|message| compare_snowflake_ids(&message.id, cursor).is_gt())
        .collect()
}

fn compare_snowflake_ids(a: &str, b: &str) -> std::cmp::Ordering {
    let parsed = a.parse::<u128>().ok().zip(b.parse::<u128>().ok());
    match parsed {
        Some((a, b)) => a.cmp(&b),
        None => a.cmp(b),
    }
}

fn channel_label(guild_name: Option<&str>, channel_name: Option<&str>, channel_id: &str) -> String {
    match (guild_name, channel_name) {
        (Some(guild), Some(channel)) => format!("{guild} / {channel}"),
        (None, Some(channel)) => channel.to_string(),
        (Some(guild), None) => format!("{guild} / {channel_id}"),
        (None, None) => channel_id.to_string(),
    }
}

fn source_label_from_summary(summary: &ScrapeSummary) -> String {
    channel_label(
        summary.guild_name.as_deref(),
        summary.channel_name.as_deref(),
        &summary.channel_id,
    )
}

fn notify_new_actions(app: &AppHandle, count: usize, source_label: Option<&str>) {
    if count == 0 {
        return;
    }
    let source = source_label.unwrap_or("watched source");
    let body = format!(
        "{count} new action item{} from {source}",
        if count == 1 { "" } else { "s" }
    );
    if let Err(e) = app
        .notification()
        .builder()
        .title("Crumb")
        .body(body)
        .show()
    {
        tracing::warn!("failed to send notification: {e}");
    }
}

#[derive(Debug, Default)]
struct PrApprovalState {
    url: String,
    approval_message_ids: Vec<String>,
    merged_after_approval: bool,
}

#[derive(Debug, Default)]
struct PrMergeOutcome {
    success_message_ids: Vec<String>,
    failure_message_ids: Vec<String>,
}

fn reconcile_pr_merge_outcomes(
    existing_actions: &[CanonicalActionItem],
    action_items: &mut Vec<ActionCandidate>,
    messages: &[NormalizedMessage],
    current_user: &NormalizedPerson,
) {
    let outcomes = pr_merge_outcomes(messages);
    for (url, outcome) in outcomes {
        if !outcome.failure_message_ids.is_empty() {
            let merge_with = existing_merge_failure_action(existing_actions, &url)
                .map(|action| action.id.clone());
            let dedupe_key = merge_failure_dedupe_key(&url);
            let mut found_existing_candidate = false;
            for item in action_items.iter_mut().filter(|item| {
                item.url.as_deref() == Some(url.as_str()) && is_merge_failure_action(&item.text)
            }) {
                item.assignee_key = Some(current_user.key.clone());
                item.assignee = Some(current_user.display_name.clone());
                item.dedupe_key = Some(dedupe_key.clone());
                item.merge_with = merge_with.clone();
                if item.message_ids.is_empty() {
                    item.message_ids = outcome.failure_message_ids.clone();
                }
                found_existing_candidate = true;
            }
            if found_existing_candidate {
                continue;
            }
            action_items.push(ActionCandidate {
                text: format!("Resolve merge queue failure for PR {}", pr_label(&url)),
                assignee_key: Some(current_user.key.clone()),
                assignee: Some(current_user.display_name.clone()),
                due: None,
                url: Some(url.clone()),
                message_ids: outcome.failure_message_ids,
                dedupe_key: Some(dedupe_key),
                merge_with,
            });
        }
    }
}

fn dismiss_resolved_pr_merge_actions(
    db: &Db,
    actions: &[CanonicalActionItem],
    messages: &[NormalizedMessage],
) -> Result<usize> {
    let mut resolved_urls = HashSet::new();
    for (url, outcome) in pr_merge_outcomes(messages) {
        if !outcome.success_message_ids.is_empty() {
            resolved_urls.insert(url.clone());
        }
        if !outcome.failure_message_ids.is_empty() && has_open_merge_failure_action(actions, &url) {
            resolved_urls.insert(url);
        }
    }
    if resolved_urls.is_empty() {
        return Ok(0);
    }

    let ids = actions
        .iter()
        .filter(|action| matches!(action.status.as_str(), "inbox" | "active" | "snoozed"))
        .filter(|action| is_pr_merge_todo_action(&action.title))
        .filter(|action| {
            action
                .url
                .as_deref()
                .and_then(|url| normalize_pr_url(Some(url)))
                .is_some_and(|url| resolved_urls.contains(&url))
        })
        .map(|action| action.id.clone())
        .collect::<Vec<_>>();

    db.set_actions_status(&ids, "done")
}

fn existing_merge_failure_action<'a>(
    actions: &'a [CanonicalActionItem],
    url: &str,
) -> Option<&'a CanonicalActionItem> {
    actions.iter().find(|action| {
        action
            .url
            .as_deref()
            .and_then(|value| normalize_pr_url(Some(value)))
            .is_some_and(|value| value == url)
            && is_merge_failure_action(&action.title)
    })
}

fn has_open_merge_failure_action(actions: &[CanonicalActionItem], url: &str) -> bool {
    existing_merge_failure_action(actions, url)
        .is_some_and(|action| matches!(action.status.as_str(), "inbox" | "active" | "snoozed"))
}

fn pr_merge_outcomes(messages: &[NormalizedMessage]) -> BTreeMap<String, PrMergeOutcome> {
    let mut outcomes = BTreeMap::new();
    for message in messages {
        let Some(url) = find_pr_url_in_message(message) else {
            continue;
        };
        let text = pr_outcome_text(message);
        let outcome: &mut PrMergeOutcome = outcomes.entry(url).or_default();
        let is_success = is_pr_merge_success_notification(&text);
        if is_success {
            outcome.success_message_ids.push(message.id.clone());
        }
        if !is_success && is_pr_merge_failure_notification(&text) {
            outcome.failure_message_ids.push(message.id.clone());
        }
    }
    outcomes
}

fn merge_failure_dedupe_key(url: &str) -> String {
    format!("resolve-merge-queue-failure-{}", stable_pr_key(url))
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

fn pr_outcome_text(message: &NormalizedMessage) -> String {
    let mut parts = vec![message.content.as_str()];
    if message.embed_bodies.is_empty() {
        parts.extend(message.embeds.iter().map(String::as_str));
    } else {
        parts.extend(message.embed_bodies.iter().map(String::as_str));
    }
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
        || text.contains("successfuwwy mewged")
        || text.contains("merged pull request")
        || (text.contains("merge queue") && text.contains(" merged"))
}

fn is_pr_merge_failure_notification(text: &str) -> bool {
    ((text.contains("merge queue") || text.contains("mergequeue"))
        && (text.contains("failed")
            || text.contains("failure")
            || text.contains("error")
            || text.contains("could not merge")
            || text.contains("unable to merge")))
        || ((text.contains("tests failed") || text.contains("test failed"))
            && (text.contains("won't merge") || text.contains("wont merge")))
}

fn is_merge_action(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("merge") && (text.contains("pr") || text.contains("pull request"))
}

fn is_pr_merge_todo_action(text: &str) -> bool {
    let text = text.to_lowercase();
    !is_merge_failure_action(&text)
        && ((text.contains("merge") && text.contains("approved") && is_merge_action(&text))
            || text.contains("merge approved pr")
            || text.contains("merge approved pull request")
            || text.starts_with("merge pr")
            || text.starts_with("merge pull request")
            || text.contains(" merge pr ")
            || text.contains(" merge pull request "))
}

fn is_merge_failure_action(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("merge queue") && (text.contains("failure") || text.contains("failed"))
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
        return "Claude Code authentication is required for extraction. Run `claude` in a terminal and complete login, or clear/fix the configured Claude config dir if it points at an unauthenticated config.".into();
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
    fn merge_outcomes_canonicalize_embed_url_fragments() {
        let mut merged = message("1", "");
        merged.embeds = vec![
            "Merge queue successfully merged https://github.com/example/repo/pull/789#issuecomment-123"
                .into(),
        ];

        let outcomes = pr_merge_outcomes(&[merged]);

        assert!(outcomes
            .get("https://github.com/example/repo/pull/789")
            .is_some_and(|outcome| outcome.success_message_ids == vec!["1".to_string()]));
    }

    #[test]
    fn merge_outcomes_detect_uwu_merge_success() {
        let mut merged = message("1", "");
        merged.embeds =
            vec!["Merge queue successfuwwy mewged https://github.com/example/repo/pull/789".into()];

        let outcomes = pr_merge_outcomes(&[merged]);

        assert!(outcomes
            .get("https://github.com/example/repo/pull/789")
            .is_some_and(|outcome| outcome.success_message_ids == vec!["1".to_string()]));
    }

    #[test]
    fn merge_success_with_validation_errors_in_title_is_not_a_failure() {
        let mut merged = message("1", "");
        merged.embeds = vec![
            "Merge Queue: PR #285481 - [dev] show feedback when attempting to save or publish widget configs with validation errors | Your PR has been successfuwwy mewged. | https://github.com/example/repo/pull/285481"
                .into(),
        ];
        merged.embed_bodies = vec!["Your PR has been successfuwwy mewged.".into()];

        let outcomes = pr_merge_outcomes(&[merged]);
        let outcome = outcomes
            .get("https://github.com/example/repo/pull/285481")
            .expect("PR outcome");

        assert_eq!(outcome.success_message_ids, vec!["1".to_string()]);
        assert!(outcome.failure_message_ids.is_empty());
    }

    #[test]
    fn merge_queue_outcomes_ignore_embed_titles() {
        let mut failed = message("1", "");
        failed.embeds = vec![
            "Merge Queue: PR #285481 - [dev] show feedback when attempting to save or publish widget configs with validation errors | Tests failed. The PR won't merge until all tests pass. Either submit again using /merge, or retry the failed test(s) - View tests | https://github.com/example/repo/pull/285481"
                .into(),
        ];
        failed.embed_bodies = vec![
            "Tests failed. The PR won't merge until all tests pass. Either submit again using /merge, or retry the failed test(s) - View tests"
                .into(),
        ];

        let outcomes = pr_merge_outcomes(&[failed]);
        let outcome = outcomes
            .get("https://github.com/example/repo/pull/285481")
            .expect("PR outcome");

        assert!(outcome.success_message_ids.is_empty());
        assert_eq!(outcome.failure_message_ids, vec!["1".to_string()]);
    }

    #[test]
    fn merge_queue_failure_creates_fix_action_for_current_user() {
        let mut actions = Vec::new();
        let mut failed = message("1", "");
        failed.embeds = vec![
            "Merge queue failed for https://github.com/example/repo/pull/789#issuecomment-123"
                .into(),
        ];
        let current_user = current_user();

        reconcile_pr_merge_outcomes(&[], &mut actions, &[failed], &current_user);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].text, "Resolve merge queue failure for PR #789");
        assert_eq!(
            actions[0].assignee_key.as_deref(),
            Some("discord:user:current")
        );
        assert_eq!(
            actions[0].url.as_deref(),
            Some("https://github.com/example/repo/pull/789")
        );
        assert_eq!(
            actions[0].dedupe_key.as_deref(),
            Some("resolve-merge-queue-failure-github-com-example-repo-pull-789")
        );
    }

    #[test]
    fn merge_queue_failure_action_is_not_treated_as_merge_todo() {
        assert!(is_pr_merge_todo_action("Merge approved PR #789"));
        assert!(!is_pr_merge_todo_action(
            "Resolve merge queue failure for PR #789"
        ));
    }

    #[test]
    fn merge_success_dismisses_matching_merge_todo() -> Result<()> {
        let db_path =
            std::env::temp_dir().join(format!("crumb-runtime-merge-{}.db", uuid::Uuid::new_v4()));
        let db = Db::open(&db_path)?;
        db.insert_running(
            "scrape-1",
            "channel-1",
            Some("Nelly (DM)"),
            None,
            None,
            "tester",
        )?;
        db.mark_extracted(
            "scrape-1",
            Some("1"),
            Some("1"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Merge approved PR #789".into(),
                assignee_key: Some("discord:user:current".into()),
                assignee: Some("Current User".into()),
                due: None,
                url: Some("https://github.com/example/repo/pull/789".into()),
                message_ids: vec!["1".into()],
                dedupe_key: Some("merge-approved-pr-github-com-example-repo-pull-789".into()),
                merge_with: None,
            }],
        )?;
        let mut merged = message("2", "");
        merged.embeds = vec![
            "Merge queue successfully merged https://github.com/example/repo/pull/789#issuecomment-123"
                .into(),
        ];

        let actions = db.list_source_actions("discord", "channel-1")?;
        let dismissed = dismiss_resolved_pr_merge_actions(&db, &actions, &[merged])?;

        assert_eq!(dismissed, 1);
        assert!(db.list_open_action_items()?.is_empty());
        assert_eq!(
            db.list_source_actions("discord", "channel-1")?[0].status,
            "done"
        );

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn merge_failure_dismisses_merge_todo_but_keeps_failure_action() -> Result<()> {
        let db_path = std::env::temp_dir().join(format!(
            "crumb-runtime-merge-failure-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Db::open(&db_path)?;
        db.insert_running(
            "scrape-1",
            "channel-1",
            Some("Nelly (DM)"),
            None,
            None,
            "tester",
        )?;
        db.mark_extracted(
            "scrape-1",
            Some("1"),
            Some("2"),
            2,
            "summary",
            &[],
            &[
                ActionCandidate {
                    text: "Merge approved PR #789".into(),
                    assignee_key: Some("discord:user:current".into()),
                    assignee: Some("Current User".into()),
                    due: None,
                    url: Some("https://github.com/example/repo/pull/789".into()),
                    message_ids: vec!["1".into()],
                    dedupe_key: Some("merge-approved-pr-github-com-example-repo-pull-789".into()),
                    merge_with: None,
                },
                ActionCandidate {
                    text: "Resolve merge queue failure for PR #789".into(),
                    assignee_key: Some("discord:user:current".into()),
                    assignee: Some("Current User".into()),
                    due: None,
                    url: Some("https://github.com/example/repo/pull/789".into()),
                    message_ids: vec!["2".into()],
                    dedupe_key: Some(
                        "resolve-merge-queue-failure-github-com-example-repo-pull-789".into(),
                    ),
                    merge_with: None,
                },
            ],
        )?;
        let mut failed = message("2", "");
        failed.embeds =
            vec!["Merge queue failed for https://github.com/example/repo/pull/789".into()];

        let actions = db.list_source_actions("discord", "channel-1")?;
        let dismissed = dismiss_resolved_pr_merge_actions(&db, &actions, &[failed])?;
        let open = db.list_open_action_items()?;

        assert_eq!(dismissed, 1);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].title, "Resolve merge queue failure for PR #789");

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn merge_failure_ai_candidate_does_not_collapse_into_merge_todo() -> Result<()> {
        let db_path = std::env::temp_dir().join(format!(
            "crumb-runtime-merge-failure-ai-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Db::open(&db_path)?;
        db.insert_running(
            "scrape-1",
            "channel-1",
            Some("Nelly (DM)"),
            None,
            None,
            "tester",
        )?;
        db.mark_extracted(
            "scrape-1",
            Some("1"),
            Some("1"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Merge PR #285629 - generate API JSON from widget sample data".into(),
                assignee_key: Some("discord:user:current".into()),
                assignee: Some("Current User".into()),
                due: None,
                url: Some("https://github.com/discord/discord/pull/285629".into()),
                message_ids: vec!["1".into()],
                dedupe_key: Some("merge-pr-github-com-discord-discord-pull-285629".into()),
                merge_with: None,
            }],
        )?;
        let existing_actions = db.list_source_actions("discord", "channel-1")?;
        let merge_action_id = existing_actions[0].id.clone();
        let mut failed = message("2", "");
        failed.embeds = vec![
            "Merge Queue: PR #285629 - generate API JSON from widget sample data | Tests failed. The PR won't merge until all tests pass. Either submit again using /merge, or retry the failed test(s) - View tests | https://github.com/discord/discord/pull/285629"
                .into(),
        ];
        let current_user = current_user();
        let mut action_items = vec![ActionCandidate {
            text: "Resolve merge queue test failure for PR #285629 - generate API JSON from widget sample data".into(),
            assignee_key: None,
            assignee: None,
            due: None,
            url: Some("https://github.com/discord/discord/pull/285629".into()),
            message_ids: vec!["2".into()],
            dedupe_key: None,
            merge_with: Some(merge_action_id),
        }];

        reconcile_pr_merge_outcomes(
            &existing_actions,
            &mut action_items,
            &[failed.clone()],
            &current_user,
        );

        assert_eq!(
            action_items[0].dedupe_key.as_deref(),
            Some("resolve-merge-queue-failure-github-com-discord-discord-pull-285629")
        );
        assert!(action_items[0].merge_with.is_none());

        db.insert_running(
            "scrape-2",
            "channel-1",
            Some("Nelly (DM)"),
            None,
            None,
            "tester",
        )?;
        db.mark_extracted(
            "scrape-2",
            Some("2"),
            Some("2"),
            1,
            "summary",
            &[],
            &action_items,
        )?;
        let after_actions = db.list_source_actions("discord", "channel-1")?;
        let dismissed = dismiss_resolved_pr_merge_actions(&db, &after_actions, &[failed])?;
        let open = db.list_open_action_items()?;

        assert_eq!(dismissed, 1);
        assert_eq!(open.len(), 1);
        assert_eq!(
            open[0].title,
            "Resolve merge queue test failure for PR #285629 - generate API JSON from widget sample data"
        );

        let _ = std::fs::remove_file(db_path);
        Ok(())
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

    #[test]
    fn watch_filter_only_keeps_messages_after_cursor() {
        let messages = vec![
            message("99", "old"),
            message("100", "cursor"),
            message("101", "new"),
        ];

        let filtered = filter_messages_after_cursor(messages, "100");

        assert_eq!(
            filtered
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec!["101"]
        );
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
            embed_bodies: Vec::new(),
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

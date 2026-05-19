use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tauri::async_runtime;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, watch};

use crate::ai;
use crate::db::{ActionCandidate, Db, DecisionCandidate};
use crate::discord::{DiscordBot, DiscordScraper, ScrapeRequest};
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

    let result = async {
        let messages = scraper
            .fetch_channel_messages(&req.channel_id, req.limit, |fetched| {
                tracing::debug!("progress {}: {}", req.scrape_id, fetched);
            })
            .await?;

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

        let extracted = ai::extract(&messages, &existing_actions, &existing_decisions)
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
            let action_items: Vec<_> = extracted
                .action_items
                .into_iter()
                .map(|a| ActionCandidate {
                    text: a.text,
                    assignee: a.assignee,
                    assignee_key: a.assignee_key,
                    due: a.due,
                    message_ids: a.message_ids.unwrap_or_default(),
                    dedupe_key: a.dedupe_key,
                    merge_with: a.merge_with,
                })
                .collect();

            match db.mark_extracted(
                &req.scrape_id,
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

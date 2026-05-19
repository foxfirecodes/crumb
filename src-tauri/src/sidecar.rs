// Spawns and supervises the Bun-compiled sidecar binary. Translates its
// NDJSON event stream into Tauri events for the frontend and DB writes.

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tauri::async_runtime;
use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

use crate::db::Db;
use crate::events::SidecarStatus;

#[derive(Debug, Clone, Serialize)]
struct HostRequest {
    id: String,
    kind: String,
    payload: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct HostResponse {
    id: String,
    ok: bool,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
enum SidecarEvent {
    #[serde(rename = "ready")]
    Ready {
        #[serde(rename = "botUser")]
        bot_user: Option<String>,
        #[serde(rename = "selfUser")]
        self_user: Option<String>,
    },
    #[serde(rename = "log")]
    Log { level: String, msg: String },
    #[serde(rename = "scrape.started")]
    ScrapeStarted {
        #[serde(rename = "scrapeId")]
        scrape_id: String,
        #[serde(rename = "channelId")]
        channel_id: String,
        #[serde(rename = "channelName")]
        channel_name: Option<String>,
        #[serde(rename = "guildId")]
        guild_id: Option<String>,
        #[serde(rename = "guildName")]
        guild_name: Option<String>,
        #[serde(rename = "triggeredBy")]
        triggered_by: String,
    },
    #[serde(rename = "scrape.progress")]
    ScrapeProgress {
        #[serde(rename = "scrapeId")]
        scrape_id: String,
        fetched: i64,
    },
    #[serde(rename = "scrape.extracted")]
    ScrapeExtracted {
        #[serde(rename = "scrapeId")]
        scrape_id: String,
        #[serde(rename = "messageCount")]
        message_count: i64,
        summary: String,
        decisions: Vec<ExtractedDecision>,
        #[serde(rename = "actionItems")]
        action_items: Vec<ExtractedActionItem>,
    },
    #[serde(rename = "scrape.failed")]
    ScrapeFailed {
        #[serde(rename = "scrapeId")]
        scrape_id: String,
        error: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct ExtractedDecision {
    text: String,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    message_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExtractedActionItem {
    text: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    due: Option<String>,
    #[serde(default)]
    message_ids: Option<Vec<String>>,
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value, String>>>>>;

/// Public handle to the sidecar. Cheap to clone.
#[derive(Clone)]
pub struct SidecarHandle {
    tx: mpsc::UnboundedSender<HostRequest>,
    pending: Pending,
    status: Arc<Mutex<SidecarStatus>>,
}

impl SidecarHandle {
    pub fn status(&self) -> SidecarStatus {
        self.status.lock().clone()
    }

    fn set_status(&self, s: SidecarStatus, app: &AppHandle) {
        *self.status.lock() = s.clone();
        let _ = app.emit("sidecar:status", &s);
    }

    pub async fn init(
        &self,
        bot_token: Option<String>,
        app_id: Option<String>,
        user_token: Option<String>,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "botToken": bot_token,
            "appId": app_id,
            "userToken": user_token,
        });
        self.send("init", payload).await?;
        Ok(())
    }

    pub async fn shutdown(&self) {
        let _ = self.send("shutdown", serde_json::json!({})).await;
    }

    async fn send(&self, kind: &str, payload: serde_json::Value) -> Result<serde_json::Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id.clone(), tx);

        let req = HostRequest {
            id,
            kind: kind.into(),
            payload,
        };
        self.tx
            .send(req)
            .map_err(|_| anyhow!("sidecar input channel closed"))?;

        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(msg))) => Err(anyhow!(msg)),
            Ok(Err(_)) => Err(anyhow!("sidecar response channel dropped")),
            Err(_) => Err(anyhow!("sidecar request timed out")),
        }
    }
}

/// Spawn the sidecar and wire up its event stream.
///
/// Returns immediately; the actual gateway handshake happens in the background.
pub fn spawn(app: AppHandle, db: Db) -> Result<SidecarHandle> {
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<HostRequest>();
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

    let handle = SidecarHandle {
        tx: input_tx,
        pending: pending.clone(),
        status: Arc::new(Mutex::new(SidecarStatus::Starting)),
    };
    handle.set_status(SidecarStatus::Starting, &app);

    let app_for_task = app.clone();
    let handle_for_task = handle.clone();

    async_runtime::spawn(async move {
        let shell = app_for_task.shell();
        let (mut events, mut child): (_, CommandChild) = match shell
            .sidecar("crumb-sidecar")
            .and_then(|c| c.spawn().map_err(Into::into))
        {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("failed to spawn sidecar: {e}");
                handle_for_task.set_status(
                    SidecarStatus::Error {
                        message: format!("spawn failed: {e}"),
                    },
                    &app_for_task,
                );
                return;
            }
        };

        // Write loop: drain input_rx into stdin.
        async_runtime::spawn(async move {
            while let Some(req) = input_rx.recv().await {
                let mut line = match serde_json::to_string(&req) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("failed to encode sidecar request: {e}");
                        continue;
                    }
                };
                line.push('\n');
                if let Err(e) = child.write(line.as_bytes()) {
                    tracing::error!("sidecar stdin write failed: {e}");
                    break;
                }
            }
        });

        // Read loop: parse events / responses.
        while let Some(event) = events.recv().await {
            match event {
                CommandEvent::Stdout(bytes) => {
                    if let Ok(line) = std::str::from_utf8(&bytes) {
                        for piece in line.split('\n') {
                            let piece = piece.trim();
                            if piece.is_empty() {
                                continue;
                            }
                            handle_line(&app_for_task, &db, &handle_for_task, piece);
                        }
                    }
                }
                CommandEvent::Stderr(bytes) => {
                    if let Ok(s) = std::str::from_utf8(&bytes) {
                        for line in s.lines() {
                            if !line.trim().is_empty() {
                                tracing::info!(target: "sidecar", "{line}");
                            }
                        }
                    }
                }
                CommandEvent::Terminated(payload) => {
                    tracing::warn!("sidecar terminated: {:?}", payload);
                    handle_for_task.set_status(SidecarStatus::Disconnected, &app_for_task);
                    // Fail any in-flight requests so callers don't hang.
                    let mut pending = pending.lock();
                    for (_, tx) in pending.drain() {
                        let _ = tx.send(Err("sidecar terminated".into()));
                    }
                    break;
                }
                CommandEvent::Error(e) => {
                    tracing::error!("sidecar error event: {e}");
                    handle_for_task.set_status(
                        SidecarStatus::Error { message: e },
                        &app_for_task,
                    );
                }
                _ => {}
            }
        }
    });

    // Once spawned, kick the init message after a short delay so the sidecar's
    // own boot log has time to settle.
    let app_for_init = app.clone();
    let handle_for_init = handle.clone();
    async_runtime::spawn(async move {
        sleep(Duration::from_millis(200)).await;
        match crate::env::load_discord_env(&app_for_init) {
            Ok((bot, app_id, user)) => {
                if let Err(e) = handle_for_init.init(bot, app_id, user).await {
                    tracing::error!("sidecar init failed: {e}");
                    handle_for_init.set_status(
                        SidecarStatus::Error {
                            message: e.to_string(),
                        },
                        &app_for_init,
                    );
                }
            }
            Err(e) => {
                tracing::warn!("env load: {e}");
                handle_for_init.set_status(
                    SidecarStatus::Error {
                        message: format!("missing credentials: {e}"),
                    },
                    &app_for_init,
                );
            }
        }
    });

    Ok(handle)
}

fn handle_line(app: &AppHandle, db: &Db, handle: &SidecarHandle, line: &str) {
    // Try parsing as a response first; if it has `ok`, route to pending.
    if let Ok(resp) = serde_json::from_str::<HostResponse>(line) {
        if let Some(tx) = handle.pending.lock().remove(&resp.id) {
            if resp.ok {
                let _ = tx.send(Ok(resp.result.unwrap_or(serde_json::Value::Null)));
            } else {
                let _ = tx.send(Err(resp
                    .error
                    .unwrap_or_else(|| "unknown sidecar error".into())));
            }
            return;
        }
    }

    // Otherwise it's an event.
    match serde_json::from_str::<SidecarEvent>(line) {
        Ok(ev) => handle_event(app, db, handle, ev),
        Err(e) => tracing::warn!("unparseable sidecar line: {e}: {line}"),
    }
}

fn handle_event(app: &AppHandle, db: &Db, handle: &SidecarHandle, ev: SidecarEvent) {
    match ev {
        SidecarEvent::Ready {
            bot_user,
            self_user,
        } => {
            handle.set_status(
                SidecarStatus::Connected {
                    bot_user,
                    self_user,
                },
                app,
            );
        }
        SidecarEvent::Log { level, msg } => {
            tracing::info!(target: "sidecar", level = %level, "{msg}");
        }
        SidecarEvent::ScrapeStarted {
            scrape_id,
            channel_id,
            channel_name,
            guild_id,
            guild_name,
            triggered_by,
        } => match db.insert_running(
            &scrape_id,
            &channel_id,
            channel_name.as_deref(),
            guild_id.as_deref(),
            guild_name.as_deref(),
            &triggered_by,
        ) {
            Ok(summary) => {
                let _ = app.emit("scrape:new", &summary);
            }
            Err(e) => tracing::error!("insert_running: {e}"),
        },
        SidecarEvent::ScrapeProgress { scrape_id, fetched } => {
            tracing::debug!("progress {scrape_id}: {fetched}");
        }
        SidecarEvent::ScrapeExtracted {
            scrape_id,
            message_count,
            summary,
            decisions,
            action_items,
        } => {
            let decisions: Vec<_> = decisions
                .into_iter()
                .map(|d| (d.text, d.context, d.message_ids.unwrap_or_default()))
                .collect();
            let action_items: Vec<_> = action_items
                .into_iter()
                .map(|a| {
                    (
                        a.text,
                        a.assignee,
                        a.due,
                        a.message_ids.unwrap_or_default(),
                    )
                })
                .collect();
            match db.mark_extracted(
                &scrape_id,
                message_count,
                &summary,
                &decisions,
                &action_items,
            ) {
                Ok(updated) => {
                    let _ = app.emit("scrape:updated", &updated);
                }
                Err(e) => tracing::error!("mark_extracted: {e}"),
            }
        }
        SidecarEvent::ScrapeFailed { scrape_id, error } => match db.mark_failed(&scrape_id, &error)
        {
            Ok(updated) => {
                let _ = app.emit("scrape:updated", &updated);
            }
            Err(e) => tracing::error!("mark_failed: {e}"),
        },
    }
}

// Single-process SQLite access via rusqlite (we don't use tauri-plugin-sql at
// runtime — it's only there to satisfy IDE/frontend tooling expectations).
// Direct rusqlite gives us simpler typed queries and avoids two paths to the
// same DB file.

use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use serde_json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::events::{ActionItem, Decision, ScrapeDetail, ScrapeSummary};

const MIGRATION_SQL: &str = include_str!("../migrations/0001_init.sql");

#[derive(Clone)]
pub struct Db {
    inner: Arc<Mutex<rusqlite::Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("creating db dir")?;
        }
        let conn = rusqlite::Connection::open(path).context("opening sqlite")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(MIGRATION_SQL).context("running migration")?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn list_scrapes(&self) -> Result<Vec<ScrapeSummary>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT id, source, channel_id, channel_name, guild_id, guild_name,
                    triggered_by, triggered_at, status, message_count, summary, error
             FROM scrapes ORDER BY triggered_at DESC LIMIT 200",
        )?;
        let result = stmt
            .query_map([], row_to_summary)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(result)
    }

    pub fn get_scrape(&self, id: &str) -> Result<Option<ScrapeDetail>> {
        let conn = self.inner.lock();
        let summary: Option<ScrapeSummary> = conn
            .query_row(
                "SELECT id, source, channel_id, channel_name, guild_id, guild_name,
                        triggered_by, triggered_at, status, message_count, summary, error
                 FROM scrapes WHERE id = ?",
                [id],
                row_to_summary,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        let Some(scrape) = summary else { return Ok(None) };

        let decisions: Vec<_> = {
            let mut stmt = conn.prepare(
                "SELECT id, scrape_id, text, context, message_ids, created_at
                 FROM decisions WHERE scrape_id = ? ORDER BY created_at",
            )?;
            let collected = stmt
                .query_map([id], row_to_decision)?
                .collect::<Result<Vec<_>, _>>()?;
            collected
        };

        let action_items: Vec<_> = {
            let mut stmt = conn.prepare(
                "SELECT id, scrape_id, text, assignee, due, message_ids, created_at
                 FROM action_items WHERE scrape_id = ? ORDER BY created_at",
            )?;
            let collected = stmt
                .query_map([id], row_to_action)?
                .collect::<Result<Vec<_>, _>>()?;
            collected
        };

        Ok(Some(ScrapeDetail {
            scrape,
            decisions,
            action_items,
        }))
    }

    pub fn insert_running(
        &self,
        id: &str,
        channel_id: &str,
        channel_name: Option<&str>,
        guild_id: Option<&str>,
        guild_name: Option<&str>,
        triggered_by: &str,
    ) -> Result<ScrapeSummary> {
        let now = Utc::now().timestamp_millis();
        let conn = self.inner.lock();
        conn.execute(
            "INSERT INTO scrapes (id, source, channel_id, channel_name, guild_id, guild_name,
                                  triggered_by, triggered_at, status)
             VALUES (?, 'discord', ?, ?, ?, ?, ?, ?, 'running')",
            rusqlite::params![
                id,
                channel_id,
                channel_name,
                guild_id,
                guild_name,
                triggered_by,
                now
            ],
        )?;
        Ok(ScrapeSummary {
            id: id.into(),
            source: "discord".into(),
            channel_id: channel_id.into(),
            channel_name: channel_name.map(Into::into),
            guild_id: guild_id.map(Into::into),
            guild_name: guild_name.map(Into::into),
            triggered_by: triggered_by.into(),
            triggered_at: now,
            status: "running".into(),
            message_count: None,
            summary: None,
            error: None,
        })
    }

    pub fn mark_extracted(
        &self,
        scrape_id: &str,
        message_count: i64,
        summary: &str,
        decisions: &[(String, Option<String>, Vec<String>)],
        action_items: &[(String, Option<String>, Option<String>, Vec<String>)],
    ) -> Result<ScrapeSummary> {
        let mut conn = self.inner.lock();
        let now = Utc::now().timestamp_millis();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE scrapes SET status='extracted', message_count=?, summary=?, error=NULL WHERE id=?",
            rusqlite::params![message_count, summary, scrape_id],
        )?;
        tx.execute("DELETE FROM decisions WHERE scrape_id=?", [scrape_id])?;
        tx.execute("DELETE FROM action_items WHERE scrape_id=?", [scrape_id])?;
        for (text, context, msg_ids) in decisions {
            let ids = serde_json::to_string(msg_ids)?;
            tx.execute(
                "INSERT INTO decisions (id, scrape_id, text, context, message_ids, created_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    scrape_id,
                    text,
                    context,
                    ids,
                    now
                ],
            )?;
        }
        for (text, assignee, due, msg_ids) in action_items {
            let ids = serde_json::to_string(msg_ids)?;
            tx.execute(
                "INSERT INTO action_items (id, scrape_id, text, assignee, due, message_ids, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    scrape_id,
                    text,
                    assignee,
                    due,
                    ids,
                    now
                ],
            )?;
        }
        tx.commit()?;
        drop(conn);
        // Re-read the row to return the canonical state.
        self.get_scrape(scrape_id)?
            .map(|d| d.scrape)
            .context("scrape vanished after update")
    }

    pub fn mark_failed(&self, scrape_id: &str, error: &str) -> Result<ScrapeSummary> {
        let conn = self.inner.lock();
        conn.execute(
            "UPDATE scrapes SET status='failed', error=? WHERE id=?",
            rusqlite::params![error, scrape_id],
        )?;
        drop(conn);
        self.get_scrape(scrape_id)?
            .map(|d| d.scrape)
            .context("scrape vanished after failure")
    }
}

pub fn resolve_db_path(app: &AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .context("resolving app data dir")?;
    Ok(dir.join("crumb.db"))
}

fn row_to_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScrapeSummary> {
    Ok(ScrapeSummary {
        id: row.get(0)?,
        source: row.get(1)?,
        channel_id: row.get(2)?,
        channel_name: row.get(3)?,
        guild_id: row.get(4)?,
        guild_name: row.get(5)?,
        triggered_by: row.get(6)?,
        triggered_at: row.get(7)?,
        status: row.get(8)?,
        message_count: row.get(9)?,
        summary: row.get(10)?,
        error: row.get(11)?,
    })
}

fn row_to_decision(row: &rusqlite::Row<'_>) -> rusqlite::Result<Decision> {
    let raw_ids: Option<String> = row.get(4)?;
    let message_ids: Vec<String> = raw_ids
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    Ok(Decision {
        id: row.get(0)?,
        scrape_id: row.get(1)?,
        text: row.get(2)?,
        context: row.get(3)?,
        message_ids,
        created_at: row.get(5)?,
    })
}

fn row_to_action(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActionItem> {
    let raw_ids: Option<String> = row.get(5)?;
    let message_ids: Vec<String> = raw_ids
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    Ok(ActionItem {
        id: row.get(0)?,
        scrape_id: row.get(1)?,
        text: row.get(2)?,
        assignee: row.get(3)?,
        due: row.get(4)?,
        message_ids,
        created_at: row.get(6)?,
    })
}

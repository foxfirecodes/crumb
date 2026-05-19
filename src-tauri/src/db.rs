// Single-process SQLite access via rusqlite (we don't use tauri-plugin-sql at
// runtime — it's only there to satisfy IDE/frontend tooling expectations).
// Direct rusqlite gives us simpler typed queries and avoids two paths to the
// same DB file.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use serde_json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::events::{ActionItem, CanonicalActionItem, Decision, ScrapeDetail, ScrapeSummary};

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
        conn.execute_batch(MIGRATION_SQL)
            .context("running migration")?;
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

    pub fn list_open_action_items(&self) -> Result<Vec<CanonicalActionItem>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT a.id, a.title, a.status, a.source_kind, a.source_scope, a.source_label,
                    a.assignee, a.due, a.priority, a.relevance_score, a.first_seen_at,
                    a.last_seen_at, a.completed_at, a.snoozed_until, a.latest_context,
                    COUNT(e.id) AS evidence_count
             FROM canonical_action_items a
             LEFT JOIN action_item_evidence e ON e.action_item_id = a.id
             WHERE a.status IN ('inbox', 'active')
               AND (a.snoozed_until IS NULL OR a.snoozed_until <= strftime('%s','now') * 1000)
             GROUP BY a.id
             ORDER BY
               CASE WHEN a.due IS NULL OR a.due = '' THEN 1 ELSE 0 END,
               a.due,
               a.priority DESC,
               a.relevance_score DESC,
               a.last_seen_at DESC
             LIMIT 50",
        )?;
        let result = stmt
            .query_map([], row_to_canonical_action)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(result)
    }

    pub fn set_action_status(&self, id: &str, status: &str) -> Result<CanonicalActionItem> {
        if !matches!(status, "inbox" | "active" | "snoozed" | "done" | "archived") {
            bail!("invalid action item status: {status}");
        }

        let completed_at = if status == "done" {
            Some(Utc::now().timestamp_millis())
        } else {
            None
        };
        let conn = self.inner.lock();
        conn.execute(
            "UPDATE canonical_action_items
             SET status = ?, completed_at = ?
             WHERE id = ? AND ? IN ('inbox','active','snoozed','done','archived')",
            rusqlite::params![status, completed_at, id, status],
        )?;
        drop(conn);
        self.get_canonical_action(id)?
            .context("action item vanished after status update")
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
        let Some(scrape) = summary else {
            return Ok(None);
        };

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
        let source = tx.query_row(
            "SELECT channel_id, channel_name, guild_name FROM scrapes WHERE id=?",
            [scrape_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?;
        let (channel_id, channel_name, guild_name) = source;
        let source_label =
            format_source_label(guild_name.as_deref(), channel_name.as_deref(), &channel_id);

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
            upsert_canonical_action(
                &tx,
                now,
                "discord",
                &channel_id,
                &source_label,
                scrape_id,
                text,
                assignee.as_deref(),
                due.as_deref(),
                msg_ids,
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

    fn get_canonical_action(&self, id: &str) -> Result<Option<CanonicalActionItem>> {
        let conn = self.inner.lock();
        let action = conn
            .query_row(
                "SELECT a.id, a.title, a.status, a.source_kind, a.source_scope, a.source_label,
                        a.assignee, a.due, a.priority, a.relevance_score, a.first_seen_at,
                        a.last_seen_at, a.completed_at, a.snoozed_until, a.latest_context,
                        COUNT(e.id) AS evidence_count
                 FROM canonical_action_items a
                 LEFT JOIN action_item_evidence e ON e.action_item_id = a.id
                 WHERE a.id = ?
                 GROUP BY a.id",
                [id],
                row_to_canonical_action,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(action)
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

fn row_to_canonical_action(row: &rusqlite::Row<'_>) -> rusqlite::Result<CanonicalActionItem> {
    Ok(CanonicalActionItem {
        id: row.get(0)?,
        title: row.get(1)?,
        status: row.get(2)?,
        source_kind: row.get(3)?,
        source_scope: row.get(4)?,
        source_label: row.get(5)?,
        assignee: row.get(6)?,
        due: row.get(7)?,
        priority: row.get(8)?,
        relevance_score: row.get(9)?,
        first_seen_at: row.get(10)?,
        last_seen_at: row.get(11)?,
        completed_at: row.get(12)?,
        snoozed_until: row.get(13)?,
        latest_context: row.get(14)?,
        evidence_count: row.get(15)?,
    })
}

fn upsert_canonical_action(
    tx: &rusqlite::Transaction<'_>,
    now: i64,
    source_kind: &str,
    source_scope: &str,
    source_label: &str,
    scrape_id: &str,
    text: &str,
    assignee: Option<&str>,
    due: Option<&str>,
    message_ids: &[String],
) -> Result<()> {
    let dedupe_key = normalize_action_key(text);
    if dedupe_key.is_empty() {
        return Ok(());
    }

    tx.execute(
        "INSERT INTO canonical_action_items (
           id, title, status, source_kind, source_scope, source_label, dedupe_key,
           assignee, due, priority, relevance_score, first_seen_at, last_seen_at
         )
         VALUES (?, ?, 'inbox', ?, ?, ?, ?, ?, ?, 0, 0, ?, ?)
         ON CONFLICT(source_kind, source_scope, dedupe_key) DO UPDATE SET
           last_seen_at = excluded.last_seen_at,
           source_label = excluded.source_label,
           assignee = COALESCE(canonical_action_items.assignee, excluded.assignee),
           due = COALESCE(canonical_action_items.due, excluded.due),
           title = CASE
             WHEN length(excluded.title) < length(canonical_action_items.title)
             THEN excluded.title
             ELSE canonical_action_items.title
           END",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            text,
            source_kind,
            source_scope,
            source_label,
            dedupe_key,
            assignee,
            due,
            now,
            now
        ],
    )?;

    let action_id: String = tx.query_row(
        "SELECT id FROM canonical_action_items
         WHERE source_kind=? AND source_scope=? AND dedupe_key=?",
        rusqlite::params![source_kind, source_scope, dedupe_key],
        |row| row.get(0),
    )?;

    let message_json = serde_json::to_string(message_ids)?;
    let evidence_key = if message_ids.is_empty() {
        format!("text:{dedupe_key}")
    } else {
        let mut stable_message_ids = message_ids.to_vec();
        stable_message_ids.sort();
        format!("messages:{}", stable_message_ids.join(","))
    };
    tx.execute(
        "INSERT OR IGNORE INTO action_item_evidence (
           id, action_item_id, source_kind, source_id, source_label, scrape_id,
           extracted_text, context, message_ids, evidence_key, created_at
         )
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            action_id,
            source_kind,
            source_scope,
            source_label,
            scrape_id,
            text,
            message_json,
            evidence_key,
            now
        ],
    )?;

    Ok(())
}

fn format_source_label(
    guild_name: Option<&str>,
    channel_name: Option<&str>,
    channel_id: &str,
) -> String {
    match (guild_name, channel_name) {
        (Some(guild), Some(channel)) => format!("{guild} · {channel}"),
        (None, Some(channel)) => channel.to_string(),
        (Some(guild), None) => guild.to_string(),
        (None, None) => channel_id.to_string(),
    }
}

fn normalize_action_key(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut normalized = String::with_capacity(lowered.len());
    let mut last_was_space = false;

    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch);
            last_was_space = false;
        } else if ch.is_whitespace() || matches!(ch, '-' | '_' | '/' | ':' | ';' | ',' | '.') {
            if !last_was_space {
                normalized.push(' ');
                last_was_space = true;
            }
        }
    }

    let trimmed = normalized.trim();
    for prefix in [
        "i will ",
        "ill ",
        "i need to ",
        "need to ",
        "we need to ",
        "todo ",
        "action item ",
        "follow up on ",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_key_normalization_strips_common_prefixes() {
        assert_eq!(
            normalize_action_key("Need to ship the menu bar UX."),
            "ship the menu bar ux"
        );
        assert_eq!(
            normalize_action_key("Follow up on Discord scraper retries"),
            "discord scraper retries"
        );
    }

    #[test]
    fn repeated_scrapes_upsert_canonical_actions_and_evidence() -> Result<()> {
        let db_path =
            std::env::temp_dir().join(format!("crumb-action-dedupe-{}.db", uuid::Uuid::new_v4()));
        let db = Db::open(&db_path)?;
        db.insert_running(
            "scrape-1",
            "channel-1",
            Some("dev"),
            Some("guild-1"),
            Some("Crumb"),
            "tester",
        )?;
        db.mark_extracted(
            "scrape-1",
            1,
            "summary",
            &[],
            &[(
                "Need to ship the menu bar UX".into(),
                Some("fox".into()),
                None,
                vec!["message-1".into()],
            )],
        )?;

        db.insert_running(
            "scrape-2",
            "channel-1",
            Some("dev"),
            Some("guild-1"),
            Some("Crumb"),
            "tester",
        )?;
        db.mark_extracted(
            "scrape-2",
            1,
            "summary",
            &[],
            &[(
                "Need to ship the menu bar UX".into(),
                Some("fox".into()),
                None,
                vec!["message-1".into()],
            )],
        )?;

        let actions = db.list_open_action_items()?;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].evidence_count, 1);

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }
}

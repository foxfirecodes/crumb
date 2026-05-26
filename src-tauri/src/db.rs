// Single-process SQLite access via rusqlite (we don't use tauri-plugin-sql at
// runtime — it's only there to satisfy IDE/frontend tooling expectations).
// Direct rusqlite gives us simpler typed queries and avoids two paths to the
// same DB file.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::OptionalExtension;
use serde_json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::events::{ActionItem, CanonicalActionItem, Decision, ScrapeDetail, ScrapeSummary};

struct Migration {
    id: &'static str,
    sql: &'static str,
    prepare: Option<fn(&rusqlite::Connection) -> Result<()>>,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        id: "0001_init",
        sql: include_str!("../migrations/0001_init.sql"),
        prepare: None,
    },
    Migration {
        id: "0002_action_item_dedupe",
        sql: include_str!("../migrations/0002_action_item_dedupe.sql"),
        prepare: Some(prepare_action_item_dedupe_migration),
    },
    Migration {
        id: "0003_assignee_keys",
        sql: include_str!("../migrations/0003_assignee_keys.sql"),
        prepare: Some(prepare_assignee_keys_migration),
    },
    Migration {
        id: "0004_scrape_cursor",
        sql: include_str!("../migrations/0004_scrape_cursor.sql"),
        prepare: Some(prepare_scrape_cursor_migration),
    },
    Migration {
        id: "0005_scrape_range",
        sql: include_str!("../migrations/0005_scrape_range.sql"),
        prepare: Some(prepare_scrape_range_migration),
    },
    Migration {
        id: "0006_action_urls",
        sql: include_str!("../migrations/0006_action_urls.sql"),
        prepare: Some(prepare_action_urls_migration),
    },
    Migration {
        id: "0007_watched_channels",
        sql: include_str!("../migrations/0007_watched_channels.sql"),
        prepare: Some(prepare_watched_channels_migration),
    },
];

#[derive(Debug, Clone)]
pub struct DecisionCandidate {
    pub text: String,
    pub context: Option<String>,
    pub message_ids: Vec<String>,
    pub dedupe_key: Option<String>,
    pub merge_with: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActionCandidate {
    pub text: String,
    pub assignee_key: Option<String>,
    pub assignee: Option<String>,
    pub due: Option<String>,
    pub url: Option<String>,
    pub message_ids: Vec<String>,
    pub dedupe_key: Option<String>,
    pub merge_with: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ScrapeMessageRange {
    pub first_message_id: Option<String>,
    pub last_message_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WatchedChannel {
    pub source: String,
    pub channel_id: String,
    pub channel_name: Option<String>,
    pub guild_id: Option<String>,
    pub guild_name: Option<String>,
    pub watched_by: String,
    pub watched_at: i64,
    pub last_seen_message_id: Option<String>,
    pub last_polled_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct DiscordSource {
    pub channel_id: String,
    pub channel_name: Option<String>,
    pub guild_id: Option<String>,
    pub guild_name: Option<String>,
}

#[derive(Clone)]
pub struct Db {
    inner: Arc<Mutex<rusqlite::Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("creating db dir")?;
        }
        let mut conn = rusqlite::Connection::open(path).context("opening sqlite")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        run_migrations(&mut conn).context("running migrations")?;
        consolidate_existing_actions(&mut conn).context("consolidating action duplicates")?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn list_scrapes(&self) -> Result<Vec<ScrapeSummary>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT id, source, channel_id, channel_name, guild_id, guild_name,
                    triggered_by, triggered_at, status, message_count, summary, error
             FROM scrapes s
             WHERE s.triggered_at = (
                 SELECT MAX(triggered_at)
                 FROM scrapes latest
                 WHERE latest.source = s.source
                   AND latest.channel_id = s.channel_id
             )
             ORDER BY triggered_at DESC LIMIT 200",
        )?;
        let result = stmt
            .query_map([], row_to_summary)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(result)
    }

    pub fn list_action_items(&self, status_filter: &str) -> Result<Vec<CanonicalActionItem>> {
        let status_clause = match status_filter {
            "open" => {
                "a.status IN ('inbox', 'active')
                 AND (a.snoozed_until IS NULL OR a.snoozed_until <= strftime('%s','now') * 1000)"
            }
            "dismissed" => "a.status IN ('done', 'archived')",
            "all" => {
                "(a.status IN ('inbox', 'active', 'done', 'archived')
                  OR (a.status = 'snoozed'
                      AND (a.snoozed_until IS NULL OR a.snoozed_until <= strftime('%s','now') * 1000)))"
            }
            other => bail!("invalid action item status filter: {other}"),
        };
        let order_clause = match status_filter {
            "dismissed" => "COALESCE(a.completed_at, a.last_seen_at) DESC, a.last_seen_at DESC",
            _ => {
                "CASE WHEN a.due IS NULL OR a.due = '' THEN 1 ELSE 0 END,
                 a.due,
                 a.priority DESC,
                 a.relevance_score DESC,
                 a.last_seen_at DESC"
            }
        };

        let conn = self.inner.lock();
        let sql = format!(
            "SELECT a.id, a.title, a.status, a.source_kind, a.source_scope, a.source_label,
                    a.assignee_key, a.assignee, a.due, a.url, a.priority, a.relevance_score, a.first_seen_at,
                    a.last_seen_at, a.completed_at, a.snoozed_until, a.latest_context,
                    COUNT(e.id) AS evidence_count
             FROM canonical_action_items a
             LEFT JOIN action_item_evidence e ON e.action_item_id = a.id
             WHERE {status_clause}
             GROUP BY a.id
             ORDER BY {order_clause}
             LIMIT 100"
        );
        let mut stmt = conn.prepare(&sql)?;
        let result = stmt
            .query_map([], row_to_canonical_action)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(result)
    }

    pub fn list_open_action_items(&self) -> Result<Vec<CanonicalActionItem>> {
        self.list_action_items("open")
    }

    pub fn discord_source_for_action(&self, action_id: &str) -> Result<Option<DiscordSource>> {
        let conn = self.inner.lock();
        let source: Option<(String, String)> = conn
            .query_row(
                "SELECT source_kind, source_scope
                 FROM canonical_action_items
                 WHERE id = ?",
                [action_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((source_kind, source_scope)) = source else {
            return Ok(None);
        };
        if source_kind != "discord" {
            bail!("action item source is not Discord");
        }

        query_latest_discord_source(&conn, &source_scope)
    }

    pub fn latest_discord_message_id_for_action(
        &self,
        action_id: &str,
        channel_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT message_ids
             FROM action_item_evidence
             WHERE action_item_id = ?
               AND source_kind = 'discord'
               AND source_id = ?
             ORDER BY created_at DESC, rowid DESC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![action_id, channel_id], |row| {
                row.get::<_, Option<String>>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;

        for raw_ids in rows {
            let message_ids = raw_ids
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                .unwrap_or_default();
            if let Some(message_id) = newest_discord_message_id(&message_ids) {
                return Ok(Some(message_id));
            }
        }

        Ok(None)
    }

    pub fn update_discord_source_metadata(
        &self,
        channel_id: &str,
        channel_name: Option<&str>,
        guild_id: Option<&str>,
        guild_name: Option<&str>,
    ) -> Result<Option<DiscordSource>> {
        let mut conn = self.inner.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE scrapes
             SET channel_name = COALESCE(?, channel_name),
                 guild_id = COALESCE(?, guild_id),
                 guild_name = COALESCE(?, guild_name)
             WHERE source = 'discord' AND channel_id = ?",
            rusqlite::params![channel_name, guild_id, guild_name, channel_id],
        )?;
        tx.execute(
            "UPDATE watched_channels
             SET channel_name = COALESCE(?, channel_name),
                 guild_id = COALESCE(?, guild_id),
                 guild_name = COALESCE(?, guild_name)
             WHERE source = 'discord' AND channel_id = ?",
            rusqlite::params![channel_name, guild_id, guild_name, channel_id],
        )?;

        let source = tx
            .query_row(
                "SELECT channel_id, channel_name, guild_id, guild_name
                 FROM scrapes
                 WHERE source = 'discord' AND channel_id = ?
                 ORDER BY triggered_at DESC
                 LIMIT 1",
                [channel_id],
                row_to_discord_source,
            )
            .optional()?;
        if let Some(source) = source.as_ref() {
            let source_label = format_source_label(
                source.guild_name.as_deref(),
                source.channel_name.as_deref(),
                &source.channel_id,
            );
            tx.execute(
                "UPDATE canonical_action_items
                 SET source_label = ?
                 WHERE source_kind = 'discord' AND source_scope = ?",
                rusqlite::params![source_label, source.channel_id],
            )?;
            tx.execute(
                "UPDATE action_item_evidence
                 SET source_label = ?
                 WHERE source_kind = 'discord' AND source_id = ?",
                rusqlite::params![source_label, source.channel_id],
            )?;
        }

        tx.commit()?;
        Ok(source)
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

    pub fn set_actions_status(&self, ids: &[String], status: &str) -> Result<usize> {
        if !matches!(status, "inbox" | "active" | "snoozed" | "done" | "archived") {
            bail!("invalid action item status: {status}");
        }
        let completed_at = if status == "done" {
            Some(Utc::now().timestamp_millis())
        } else {
            None
        };
        let mut conn = self.inner.lock();
        let tx = conn.transaction()?;
        let mut changed = 0;
        for id in ids {
            changed += tx.execute(
                "UPDATE canonical_action_items
                 SET status = ?, completed_at = ?
                 WHERE id = ? AND ? IN ('inbox','active','snoozed','done','archived')",
                rusqlite::params![status, completed_at, id, status],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn set_action_assignee(
        &self,
        id: &str,
        assignee_key: Option<&str>,
        assignee: Option<&str>,
    ) -> Result<CanonicalActionItem> {
        let assignee = assignee.map(str::trim).filter(|value| !value.is_empty());
        let assignee_key = stable_assignee_key(assignee_key, assignee);
        let conn = self.inner.lock();
        conn.execute(
            "UPDATE canonical_action_items
             SET assignee_key = ?, assignee = ?
             WHERE id = ?",
            rusqlite::params![assignee_key.as_deref(), assignee, id],
        )?;
        drop(conn);
        self.get_canonical_action(id)?
            .context("action item vanished after assignee update")
    }

    pub fn scraped_message_range(&self, scrape_id: &str) -> Result<ScrapeMessageRange> {
        let conn = self.inner.lock();
        let range = conn
            .query_row(
                "SELECT first_message_id, last_message_id FROM scrapes WHERE id = ?",
                [scrape_id],
                |row| {
                    Ok(ScrapeMessageRange {
                        first_message_id: row.get(0)?,
                        last_message_id: row.get(1)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default();
        Ok(range)
    }

    pub fn watch_channel(
        &self,
        channel_id: &str,
        channel_name: Option<&str>,
        guild_id: Option<&str>,
        guild_name: Option<&str>,
        watched_by: &str,
        last_seen_message_id: Option<&str>,
    ) -> Result<WatchedChannel> {
        let now = Utc::now().timestamp_millis();
        let conn = self.inner.lock();
        conn.execute(
            "INSERT INTO watched_channels (
               source, channel_id, channel_name, guild_id, guild_name,
               watched_by, watched_at, last_seen_message_id, last_polled_at
             )
             VALUES ('discord', ?, ?, ?, ?, ?, ?, ?, NULL)
             ON CONFLICT(source, channel_id) DO UPDATE SET
               channel_name = excluded.channel_name,
               guild_id = excluded.guild_id,
               guild_name = excluded.guild_name,
               watched_by = excluded.watched_by,
               watched_at = excluded.watched_at,
               last_seen_message_id = excluded.last_seen_message_id,
               last_polled_at = NULL",
            rusqlite::params![
                channel_id,
                channel_name,
                guild_id,
                guild_name,
                watched_by,
                now,
                last_seen_message_id
            ],
        )?;
        drop(conn);
        self.get_watched_channel("discord", channel_id)?
            .context("watched channel vanished after insert")
    }

    pub fn unwatch_channel(&self, channel_id: &str) -> Result<bool> {
        let conn = self.inner.lock();
        let changed = conn.execute(
            "DELETE FROM watched_channels WHERE source = 'discord' AND channel_id = ?",
            [channel_id],
        )?;
        Ok(changed > 0)
    }

    pub fn list_watched_channels(&self) -> Result<Vec<WatchedChannel>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT source, channel_id, channel_name, guild_id, guild_name,
                    watched_by, watched_at, last_seen_message_id, last_polled_at
             FROM watched_channels
             ORDER BY watched_at",
        )?;
        let result = stmt
            .query_map([], row_to_watched_channel)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(result)
    }

    fn get_watched_channel(
        &self,
        source: &str,
        channel_id: &str,
    ) -> Result<Option<WatchedChannel>> {
        let conn = self.inner.lock();
        let watched = conn
            .query_row(
                "SELECT source, channel_id, channel_name, guild_id, guild_name,
                        watched_by, watched_at, last_seen_message_id, last_polled_at
                 FROM watched_channels
                 WHERE source = ? AND channel_id = ?",
                rusqlite::params![source, channel_id],
                row_to_watched_channel,
            )
            .optional()?;
        Ok(watched)
    }

    pub fn update_watch_cursor(
        &self,
        channel_id: &str,
        last_seen_message_id: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        let conn = self.inner.lock();
        conn.execute(
            "UPDATE watched_channels
             SET last_polled_at = ?,
                 last_seen_message_id = CASE
                   WHEN ? IS NULL THEN last_seen_message_id
                   WHEN last_seen_message_id IS NULL THEN ?
                   WHEN CAST(? AS INTEGER) > CAST(last_seen_message_id AS INTEGER) THEN ?
                   ELSE last_seen_message_id
                 END
             WHERE source = 'discord' AND channel_id = ?",
            rusqlite::params![
                now,
                last_seen_message_id,
                last_seen_message_id,
                last_seen_message_id,
                last_seen_message_id,
                channel_id
            ],
        )?;
        Ok(())
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
                "SELECT id, scrape_id, text, assignee_key, assignee, due, url, message_ids, created_at
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

    pub fn delete_source(&self, id: &str) -> Result<()> {
        let mut conn = self.inner.lock();
        let tx = conn.transaction()?;
        let source: Option<(String, String)> = tx
            .query_row(
                "SELECT source, channel_id FROM scrapes WHERE id = ?",
                [id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;

        let Some((source_kind, source_scope)) = source else {
            tx.commit()?;
            return Ok(());
        };

        tx.execute(
            "DELETE FROM canonical_action_items
             WHERE source_kind = ? AND source_scope = ?",
            rusqlite::params![source_kind, source_scope],
        )?;
        tx.execute(
            "DELETE FROM scrapes
             WHERE source = ? AND channel_id = ?",
            rusqlite::params![source_kind, source_scope],
        )?;
        tx.execute(
            "DELETE FROM watched_channels
             WHERE source = ? AND channel_id = ?",
            rusqlite::params![source_kind, source_scope],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_source_actions(
        &self,
        source_kind: &str,
        source_scope: &str,
    ) -> Result<Vec<CanonicalActionItem>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT a.id, a.title, a.status, a.source_kind, a.source_scope, a.source_label,
                    a.assignee_key, a.assignee, a.due, a.url, a.priority, a.relevance_score, a.first_seen_at,
                    a.last_seen_at, a.completed_at, a.snoozed_until, a.latest_context,
                    COUNT(e.id) AS evidence_count
             FROM canonical_action_items a
             LEFT JOIN action_item_evidence e ON e.action_item_id = a.id
             WHERE a.source_kind = ? AND a.source_scope = ?
             GROUP BY a.id
             ORDER BY a.status IN ('done','archived'), a.last_seen_at DESC
             LIMIT 100",
        )?;
        let result = stmt
            .query_map(
                rusqlite::params![source_kind, source_scope],
                row_to_canonical_action,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(result)
    }

    pub fn list_source_decisions(&self, source_id: &str) -> Result<Vec<Decision>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT id, scrape_id, text, context, message_ids, created_at
             FROM decisions WHERE scrape_id = ? ORDER BY created_at",
        )?;
        let result = stmt
            .query_map([source_id], row_to_decision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(result)
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
             VALUES (?, 'discord', ?, ?, ?, ?, ?, ?, 'running')
             ON CONFLICT(id) DO UPDATE SET
               channel_id = excluded.channel_id,
               channel_name = excluded.channel_name,
               guild_id = excluded.guild_id,
               guild_name = excluded.guild_name,
               triggered_by = excluded.triggered_by,
               triggered_at = excluded.triggered_at,
               status = 'running',
               error = NULL",
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
        drop(conn);
        self.get_scrape(id)?
            .map(|d| d.scrape)
            .context("source vanished after insert")
    }

    pub fn mark_extracted(
        &self,
        scrape_id: &str,
        first_message_id: Option<&str>,
        last_message_id: Option<&str>,
        message_count: i64,
        summary: &str,
        decisions: &[DecisionCandidate],
        action_items: &[ActionCandidate],
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
            "UPDATE scrapes
             SET status='extracted',
                message_count=?,
                 summary=?,
                 error=NULL,
                 first_message_id=CASE
                   WHEN ? IS NULL THEN first_message_id
                   WHEN first_message_id IS NULL THEN ?
                   WHEN CAST(? AS INTEGER) < CAST(first_message_id AS INTEGER) THEN ?
                   ELSE first_message_id
                 END,
                 last_message_id=CASE
                   WHEN ? IS NULL THEN last_message_id
                   WHEN last_message_id IS NULL THEN ?
                   WHEN CAST(? AS INTEGER) > CAST(last_message_id AS INTEGER) THEN ?
                   ELSE last_message_id
                 END
             WHERE id=?",
            rusqlite::params![
                message_count,
                summary,
                first_message_id,
                first_message_id,
                first_message_id,
                first_message_id,
                last_message_id,
                last_message_id,
                last_message_id,
                last_message_id,
                scrape_id
            ],
        )?;
        for decision in decisions {
            let ids = serde_json::to_string(&decision.message_ids)?;
            let dedupe_key = decision_item_key(
                decision.dedupe_key.as_deref(),
                decision.merge_with.as_deref(),
                &decision.text,
                &decision.message_ids,
            );
            if let Some(decision_id) =
                valid_decision_merge_target(&tx, scrape_id, decision.merge_with.as_deref())?
            {
                tx.execute(
                    "UPDATE decisions
                     SET text = ?,
                         context = COALESCE(?, context),
                         message_ids = ?,
                         dedupe_key = COALESCE(dedupe_key, ?)
                     WHERE id = ?",
                    rusqlite::params![
                        decision.text.as_str(),
                        decision.context.as_deref(),
                        ids,
                        dedupe_key,
                        decision_id
                    ],
                )?;
                continue;
            }
            tx.execute(
                "INSERT INTO decisions (id, scrape_id, text, context, message_ids, created_at, dedupe_key)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(scrape_id, dedupe_key) DO UPDATE SET
                   text = excluded.text,
                   context = COALESCE(excluded.context, decisions.context),
                   message_ids = excluded.message_ids",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    scrape_id,
                    decision.text.as_str(),
                    decision.context.as_deref(),
                    ids,
                    now,
                    dedupe_key
                ],
            )?;
        }
        for action in action_items {
            let ids = serde_json::to_string(&action.message_ids)?;
            let assignee_key =
                stable_assignee_key(action.assignee_key.as_deref(), action.assignee.as_deref());
            let canonical_id = upsert_canonical_action(
                &tx,
                now,
                "discord",
                &channel_id,
                &source_label,
                scrape_id,
                &action.text,
                assignee_key.as_deref(),
                action.assignee.as_deref(),
                action.due.as_deref(),
                action.url.as_deref(),
                action.dedupe_key.as_deref(),
                action.merge_with.as_deref(),
                &action.message_ids,
            )?;
            let item_key = format!("canonical:{canonical_id}");
            tx.execute(
                "INSERT INTO action_items (id, scrape_id, text, assignee_key, assignee, due, url, message_ids, created_at, dedupe_key)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(scrape_id, dedupe_key) DO UPDATE SET
                   text = excluded.text,
                   assignee_key = COALESCE(excluded.assignee_key, action_items.assignee_key),
                   assignee = COALESCE(excluded.assignee, action_items.assignee),
                   due = COALESCE(excluded.due, action_items.due),
                   url = COALESCE(excluded.url, action_items.url),
                   message_ids = excluded.message_ids",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    scrape_id,
                    action.text.as_str(),
                    assignee_key.as_deref(),
                    action.assignee.as_deref(),
                    action.due.as_deref(),
                    action.url.as_deref(),
                    ids,
                    now,
                    item_key
                ],
            )?;
        }
        merge_similar_canonical_actions(&tx, "discord", &channel_id)?;
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
                        a.assignee_key, a.assignee, a.due, a.url, a.priority, a.relevance_score, a.first_seen_at,
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

fn run_migrations(conn: &mut rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
           id         TEXT PRIMARY KEY,
           applied_at INTEGER NOT NULL
         );",
    )?;

    for migration in MIGRATIONS {
        let already_applied = conn
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE id = ?",
                [migration.id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if already_applied {
            continue;
        }

        let tx = conn.transaction()?;
        if let Some(prepare) = migration.prepare {
            prepare(&tx)?;
        }
        tx.execute_batch(migration.sql)
            .with_context(|| format!("running migration {}", migration.id))?;
        tx.execute(
            "INSERT INTO schema_migrations (id, applied_at) VALUES (?, ?)",
            rusqlite::params![migration.id, Utc::now().timestamp_millis()],
        )?;
        tx.commit()
            .with_context(|| format!("committing migration {}", migration.id))?;
    }

    Ok(())
}

fn prepare_action_item_dedupe_migration(conn: &rusqlite::Connection) -> Result<()> {
    ensure_column(conn, "decisions", "dedupe_key", "TEXT")?;
    ensure_column(conn, "action_items", "dedupe_key", "TEXT")?;
    dedupe_source_rows(conn, "decisions")?;
    dedupe_source_rows(conn, "action_items")?;
    Ok(())
}

fn prepare_assignee_keys_migration(conn: &rusqlite::Connection) -> Result<()> {
    ensure_column(conn, "action_items", "assignee_key", "TEXT")?;
    ensure_column(conn, "canonical_action_items", "assignee_key", "TEXT")?;
    backfill_assignee_keys(conn, "action_items")?;
    backfill_assignee_keys(conn, "canonical_action_items")?;
    Ok(())
}

fn prepare_scrape_cursor_migration(conn: &rusqlite::Connection) -> Result<()> {
    ensure_column(conn, "scrapes", "last_message_id", "TEXT")?;
    Ok(())
}

fn prepare_scrape_range_migration(conn: &rusqlite::Connection) -> Result<()> {
    ensure_column(conn, "scrapes", "first_message_id", "TEXT")?;
    ensure_column(conn, "scrapes", "last_message_id", "TEXT")?;
    Ok(())
}

fn prepare_action_urls_migration(conn: &rusqlite::Connection) -> Result<()> {
    ensure_column(conn, "action_items", "url", "TEXT")?;
    ensure_column(conn, "canonical_action_items", "url", "TEXT")?;
    Ok(())
}

fn prepare_watched_channels_migration(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS watched_channels (
           source               TEXT NOT NULL CHECK(source IN ('discord')),
           channel_id           TEXT NOT NULL,
           channel_name         TEXT,
           guild_id             TEXT,
           guild_name           TEXT,
           watched_by           TEXT NOT NULL,
           watched_at           INTEGER NOT NULL,
           last_seen_message_id TEXT,
           last_polled_at       INTEGER,
           PRIMARY KEY(source, channel_id)
         );",
    )?;
    Ok(())
}

fn backfill_assignee_keys(conn: &rusqlite::Connection, table: &str) -> Result<()> {
    let rows = {
        let mut stmt = conn.prepare(&format!(
            "SELECT id, assignee FROM {table}
             WHERE assignee_key IS NULL AND assignee IS NOT NULL AND trim(assignee) != ''"
        ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };

    for (id, assignee) in rows {
        if let Some(assignee_key) = stable_assignee_key(None, Some(&assignee)) {
            conn.execute(
                &format!("UPDATE {table} SET assignee_key = ? WHERE id = ?"),
                rusqlite::params![assignee_key, id],
            )?;
        }
    }

    Ok(())
}

fn dedupe_source_rows(conn: &rusqlite::Connection, table: &str) -> Result<()> {
    conn.execute(
        &format!(
            "DELETE FROM {table}
             WHERE dedupe_key IS NOT NULL
               AND rowid NOT IN (
                 SELECT MIN(rowid)
                 FROM {table}
                 GROUP BY scrape_id, dedupe_key
               )"
        ),
        [],
    )?;
    Ok(())
}

fn ensure_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column);
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn consolidate_existing_actions(conn: &mut rusqlite::Connection) -> Result<()> {
    let tx = conn.transaction()?;
    let sources = {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT source_kind, source_scope
             FROM canonical_action_items
             WHERE status IN ('inbox','active','snoozed','done')",
        )?;
        let collected = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        collected
    };

    for (source_kind, source_scope) in sources {
        merge_similar_canonical_actions(&tx, &source_kind, &source_scope)?;
    }

    tx.commit()?;
    Ok(())
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

fn row_to_watched_channel(row: &rusqlite::Row<'_>) -> rusqlite::Result<WatchedChannel> {
    Ok(WatchedChannel {
        source: row.get(0)?,
        channel_id: row.get(1)?,
        channel_name: row.get(2)?,
        guild_id: row.get(3)?,
        guild_name: row.get(4)?,
        watched_by: row.get(5)?,
        watched_at: row.get(6)?,
        last_seen_message_id: row.get(7)?,
        last_polled_at: row.get(8)?,
    })
}

fn row_to_discord_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<DiscordSource> {
    Ok(DiscordSource {
        channel_id: row.get(0)?,
        channel_name: row.get(1)?,
        guild_id: row.get(2)?,
        guild_name: row.get(3)?,
    })
}

fn query_latest_discord_source(
    conn: &rusqlite::Connection,
    channel_id: &str,
) -> Result<Option<DiscordSource>> {
    let source = conn
        .query_row(
            "SELECT channel_id, channel_name, guild_id, guild_name
             FROM scrapes
             WHERE source = 'discord' AND channel_id = ?
             ORDER BY triggered_at DESC
             LIMIT 1",
            [channel_id],
            row_to_discord_source,
        )
        .optional()?;
    Ok(source)
}

fn newest_discord_message_id(message_ids: &[String]) -> Option<String> {
    message_ids
        .iter()
        .map(String::as_str)
        .filter(|id| !id.trim().is_empty())
        .max_by(|a, b| compare_discord_ids(a, b))
        .map(str::to_string)
}

fn compare_discord_ids(a: &str, b: &str) -> std::cmp::Ordering {
    let parsed = a.parse::<u128>().ok().zip(b.parse::<u128>().ok());
    match parsed {
        Some((a, b)) => a.cmp(&b),
        None => a.cmp(b),
    }
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
    let raw_ids: Option<String> = row.get(7)?;
    let message_ids: Vec<String> = raw_ids
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    Ok(ActionItem {
        id: row.get(0)?,
        scrape_id: row.get(1)?,
        text: row.get(2)?,
        assignee_key: row.get(3)?,
        assignee: row.get(4)?,
        due: row.get(5)?,
        url: row.get(6)?,
        message_ids,
        created_at: row.get(8)?,
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
        assignee_key: row.get(6)?,
        assignee: row.get(7)?,
        due: row.get(8)?,
        url: row.get(9)?,
        priority: row.get(10)?,
        relevance_score: row.get(11)?,
        first_seen_at: row.get(12)?,
        last_seen_at: row.get(13)?,
        completed_at: row.get(14)?,
        snoozed_until: row.get(15)?,
        latest_context: row.get(16)?,
        evidence_count: row.get(17)?,
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
    assignee_key: Option<&str>,
    assignee: Option<&str>,
    due: Option<&str>,
    url: Option<&str>,
    ai_dedupe_key: Option<&str>,
    merge_with: Option<&str>,
    message_ids: &[String],
) -> Result<String> {
    if let Some(action_id) = valid_merge_target(tx, source_kind, source_scope, merge_with)? {
        update_canonical_action(
            tx,
            &action_id,
            now,
            source_label,
            text,
            assignee_key,
            assignee,
            due,
            url,
        )?;
        insert_action_evidence(
            tx,
            now,
            &action_id,
            source_kind,
            source_scope,
            source_label,
            scrape_id,
            text,
            message_ids,
        )?;
        return Ok(action_id);
    }

    if ai_dedupe_key.is_none() {
        if let Some(action_id) = find_similar_action_id(tx, source_kind, source_scope, text)? {
            update_canonical_action(
                tx,
                &action_id,
                now,
                source_label,
                text,
                assignee_key,
                assignee,
                due,
                url,
            )?;
            insert_action_evidence(
                tx,
                now,
                &action_id,
                source_kind,
                source_scope,
                source_label,
                scrape_id,
                text,
                message_ids,
            )?;
            return Ok(action_id);
        }
    }

    let dedupe_key = normalize_key(ai_dedupe_key.unwrap_or(text));
    if dedupe_key.is_empty() {
        bail!("action item dedupe key is empty");
    }

    tx.execute(
        "INSERT INTO canonical_action_items (
           id, title, status, source_kind, source_scope, source_label, dedupe_key,
           assignee_key, assignee, due, url, priority, relevance_score, first_seen_at, last_seen_at, latest_context
         )
         VALUES (?, ?, 'inbox', ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, ?, ?, ?)
         ON CONFLICT(source_kind, source_scope, dedupe_key) DO UPDATE SET
           last_seen_at = excluded.last_seen_at,
           source_label = excluded.source_label,
           latest_context = excluded.latest_context,
           status = CASE
             WHEN canonical_action_items.status IN ('done', 'archived')
             THEN 'inbox'
             ELSE canonical_action_items.status
           END,
           completed_at = CASE
             WHEN canonical_action_items.status IN ('done', 'archived')
             THEN NULL
             ELSE canonical_action_items.completed_at
           END,
           snoozed_until = CASE
             WHEN canonical_action_items.status IN ('done', 'archived')
             THEN NULL
             ELSE canonical_action_items.snoozed_until
           END,
           assignee_key = COALESCE(canonical_action_items.assignee_key, excluded.assignee_key),
           assignee = COALESCE(canonical_action_items.assignee, excluded.assignee),
           due = COALESCE(canonical_action_items.due, excluded.due),
           url = COALESCE(canonical_action_items.url, excluded.url),
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
            assignee_key,
            assignee,
            due,
            url,
            now,
            now,
            text
        ],
    )?;

    let action_id: String = tx.query_row(
        "SELECT id FROM canonical_action_items
         WHERE source_kind=? AND source_scope=? AND dedupe_key=?",
        rusqlite::params![source_kind, source_scope, dedupe_key],
        |row| row.get(0),
    )?;

    insert_action_evidence(
        tx,
        now,
        &action_id,
        source_kind,
        source_scope,
        source_label,
        scrape_id,
        text,
        message_ids,
    )?;

    Ok(action_id)
}

fn update_canonical_action(
    tx: &rusqlite::Transaction<'_>,
    action_id: &str,
    now: i64,
    source_label: &str,
    text: &str,
    assignee_key: Option<&str>,
    assignee: Option<&str>,
    due: Option<&str>,
    url: Option<&str>,
) -> Result<()> {
    tx.execute(
        "UPDATE canonical_action_items
         SET last_seen_at = ?,
             source_label = ?,
             latest_context = ?,
             status = CASE
               WHEN status IN ('done', 'archived')
               THEN 'inbox'
               ELSE status
             END,
             completed_at = CASE
               WHEN status IN ('done', 'archived')
               THEN NULL
               ELSE completed_at
             END,
             snoozed_until = CASE
               WHEN status IN ('done', 'archived')
               THEN NULL
               ELSE snoozed_until
             END,
             assignee_key = COALESCE(canonical_action_items.assignee_key, ?),
             assignee = COALESCE(canonical_action_items.assignee, ?),
             due = COALESCE(canonical_action_items.due, ?),
             url = COALESCE(canonical_action_items.url, ?),
             title = CASE
               WHEN length(?) < length(canonical_action_items.title)
               THEN ?
               ELSE canonical_action_items.title
             END
         WHERE id = ?",
        rusqlite::params![
            now,
            source_label,
            text,
            assignee_key,
            assignee,
            due,
            url,
            text,
            text,
            action_id
        ],
    )?;

    Ok(())
}

fn insert_action_evidence(
    tx: &rusqlite::Transaction<'_>,
    now: i64,
    action_id: &str,
    source_kind: &str,
    source_scope: &str,
    source_label: &str,
    scrape_id: &str,
    text: &str,
    message_ids: &[String],
) -> Result<()> {
    let message_json = serde_json::to_string(message_ids)?;
    let evidence_key = if message_ids.is_empty() {
        format!("text:{}", normalize_key(text))
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

fn valid_merge_target(
    tx: &rusqlite::Transaction<'_>,
    source_kind: &str,
    source_scope: &str,
    merge_with: Option<&str>,
) -> Result<Option<String>> {
    let Some(merge_with) = merge_with.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(None);
    };

    let found = tx
        .query_row(
            "SELECT id FROM canonical_action_items
             WHERE id = ? AND source_kind = ? AND source_scope = ?",
            rusqlite::params![merge_with, source_kind, source_scope],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found)
}

fn valid_decision_merge_target(
    tx: &rusqlite::Transaction<'_>,
    scrape_id: &str,
    merge_with: Option<&str>,
) -> Result<Option<String>> {
    let Some(merge_with) = merge_with.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(None);
    };

    let found = tx
        .query_row(
            "SELECT id FROM decisions WHERE id = ? AND scrape_id = ?",
            rusqlite::params![merge_with, scrape_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found)
}

fn find_similar_action_id(
    tx: &rusqlite::Transaction<'_>,
    source_kind: &str,
    source_scope: &str,
    text: &str,
) -> Result<Option<String>> {
    let incoming = action_tokens(text);
    if incoming.len() < 3 {
        return Ok(None);
    }

    let mut stmt = tx.prepare(
        "SELECT id, title FROM canonical_action_items
         WHERE source_kind = ? AND source_scope = ?
           AND status IN ('inbox','active','snoozed','done')",
    )?;
    let existing = stmt
        .query_map(rusqlite::params![source_kind, source_scope], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut best: Option<(String, f64)> = None;
    for (id, title) in existing {
        let score = token_similarity(&incoming, &action_tokens(&title));
        if score >= 0.66
            && best
                .as_ref()
                .map_or(true, |(_, best_score)| score > *best_score)
        {
            best = Some((id, score));
        }
    }

    Ok(best.map(|(id, _)| id))
}

fn merge_similar_canonical_actions(
    tx: &rusqlite::Transaction<'_>,
    source_kind: &str,
    source_scope: &str,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT id, title, status, first_seen_at, last_seen_at, url
         FROM canonical_action_items
         WHERE source_kind = ? AND source_scope = ?
           AND status IN ('inbox','active','snoozed','done')
         ORDER BY first_seen_at ASC",
    )?;
    let actions = stmt
        .query_map(rusqlite::params![source_kind, source_scope], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut removed = HashSet::new();
    for i in 0..actions.len() {
        if removed.contains(&actions[i].0) {
            continue;
        }
        let keep_id = &actions[i].0;
        let keep_tokens = action_tokens(&actions[i].1);
        if keep_tokens.len() < 3 {
            continue;
        }

        for candidate in actions.iter().skip(i + 1) {
            if removed.contains(&candidate.0) {
                continue;
            }
            if should_skip_pr_outcome_similarity_merge(
                &actions[i].1,
                actions[i].5.as_deref(),
                &candidate.1,
                candidate.5.as_deref(),
            ) {
                continue;
            }
            let score = token_similarity(&keep_tokens, &action_tokens(&candidate.1));
            if score < 0.72 {
                continue;
            }

            merge_canonical_action(tx, keep_id, &candidate.0)?;
            removed.insert(candidate.0.clone());
        }
    }

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum PrActionKind {
    MergeTodo,
    MergeFailure,
}

fn should_skip_pr_outcome_similarity_merge(
    left_title: &str,
    left_url: Option<&str>,
    right_title: &str,
    right_url: Option<&str>,
) -> bool {
    let Some(left_url) = left_url else {
        return false;
    };
    let Some(right_url) = right_url else {
        return false;
    };
    if left_url != right_url {
        return false;
    }

    matches!(
        (pr_action_kind(left_title), pr_action_kind(right_title)),
        (
            Some(PrActionKind::MergeTodo),
            Some(PrActionKind::MergeFailure)
        ) | (
            Some(PrActionKind::MergeFailure),
            Some(PrActionKind::MergeTodo)
        )
    )
}

fn pr_action_kind(text: &str) -> Option<PrActionKind> {
    let text = text.to_lowercase();
    if text.contains("merge queue")
        && (text.contains("failure") || text.contains("failed") || text.contains("failing"))
    {
        return Some(PrActionKind::MergeFailure);
    }
    if text.contains("merge")
        && (text.contains("pr") || text.contains("pull request"))
        && !text.contains("failure")
        && !text.contains("failed")
        && !text.contains("failing")
    {
        return Some(PrActionKind::MergeTodo);
    }
    None
}

fn merge_canonical_action(
    tx: &rusqlite::Transaction<'_>,
    keep_id: &str,
    duplicate_id: &str,
) -> Result<()> {
    tx.execute(
        "UPDATE OR IGNORE action_item_evidence SET action_item_id = ? WHERE action_item_id = ?",
        rusqlite::params![keep_id, duplicate_id],
    )?;
    tx.execute(
        "DELETE FROM action_item_evidence WHERE action_item_id = ?",
        [duplicate_id],
    )?;
    tx.execute(
        "UPDATE OR IGNORE action_items
         SET dedupe_key = ?
         WHERE dedupe_key = ?",
        rusqlite::params![
            format!("canonical:{keep_id}"),
            format!("canonical:{duplicate_id}")
        ],
    )?;
    tx.execute(
        "DELETE FROM action_items WHERE dedupe_key = ?",
        [format!("canonical:{duplicate_id}")],
    )?;
    tx.execute(
        "DELETE FROM canonical_action_items WHERE id = ?",
        [duplicate_id],
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

fn normalize_key(text: &str) -> String {
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

fn stable_assignee_key(extracted_key: Option<&str>, assignee: Option<&str>) -> Option<String> {
    extracted_key
        .and_then(normalize_assignee_key)
        .or_else(|| assignee.and_then(assignee_key_from_label))
}

fn normalize_assignee_key(key: &str) -> Option<String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lowered = trimmed.to_lowercase();
    let mut normalized = String::with_capacity(lowered.len());
    let mut last_was_separator = false;

    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '.') {
            normalized.push(ch);
            last_was_separator = false;
        } else if ch.is_whitespace() || matches!(ch, '-' | '/' | ',' | ';') {
            if !last_was_separator {
                normalized.push('-');
                last_was_separator = true;
            }
        }
    }

    let normalized = normalized.trim_matches('-').to_string();
    (!normalized.is_empty()).then_some(normalized)
}

fn assignee_key_from_label(label: &str) -> Option<String> {
    let normalized = normalize_key(label).replace(' ', "-");
    if normalized.is_empty() {
        None
    } else {
        Some(format!("person:{normalized}"))
    }
}

fn decision_item_key(
    ai_dedupe_key: Option<&str>,
    merge_with: Option<&str>,
    text: &str,
    message_ids: &[String],
) -> String {
    if let Some(merge_with) = merge_with.map(str::trim).filter(|value| !value.is_empty()) {
        return format!("decision:{merge_with}");
    }
    if let Some(ai_dedupe_key) = ai_dedupe_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return normalize_key(ai_dedupe_key);
    }
    item_key_from_messages_or_text("decision", text, message_ids)
}

fn item_key_from_messages_or_text(kind: &str, text: &str, message_ids: &[String]) -> String {
    if message_ids.is_empty() {
        return normalize_key(text);
    }
    let mut stable_message_ids = message_ids.to_vec();
    stable_message_ids.sort();
    format!("{kind}:{}", stable_message_ids.join(","))
}

fn action_tokens(text: &str) -> HashSet<String> {
    normalize_key(text)
        .split_whitespace()
        .filter(|token| {
            token.len() > 2
                && !matches!(
                    *token,
                    "the"
                        | "and"
                        | "for"
                        | "with"
                        | "that"
                        | "this"
                        | "into"
                        | "onto"
                        | "from"
                        | "about"
                        | "should"
                        | "would"
                        | "could"
                        | "will"
                        | "need"
                        | "needs"
                        | "add"
                        | "send"
                        | "provide"
                        | "sync"
                        | "make"
                )
        })
        .map(ToOwned::to_owned)
        .collect()
}

fn token_similarity(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f64;
    let smaller = a.len().min(b.len()) as f64;
    intersection / smaller
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_key_normalization_strips_common_prefixes() {
        assert_eq!(
            normalize_key("Need to ship the menu bar UX."),
            "ship the menu bar ux"
        );
        assert_eq!(
            normalize_key("Follow up on Discord scraper retries"),
            "discord scraper retries"
        );
    }

    #[test]
    fn opening_legacy_db_adds_dedupe_columns_before_indexes() -> Result<()> {
        let db_path = std::env::temp_dir().join(format!(
            "crumb-legacy-migration-{}.db",
            uuid::Uuid::new_v4()
        ));
        {
            let conn = rusqlite::Connection::open(&db_path)?;
            conn.execute_batch(
                "CREATE TABLE scrapes (
                   id            TEXT PRIMARY KEY,
                   source        TEXT NOT NULL CHECK(source IN ('discord')),
                   channel_id    TEXT NOT NULL,
                   channel_name  TEXT,
                   guild_id      TEXT,
                   guild_name    TEXT,
                   triggered_by  TEXT NOT NULL,
                   triggered_at  INTEGER NOT NULL,
                   status        TEXT NOT NULL CHECK(status IN ('running','extracted','failed')),
                   message_count INTEGER,
                   summary       TEXT,
                   error         TEXT
                 );
                 CREATE TABLE decisions (
                   id          TEXT PRIMARY KEY,
                   scrape_id   TEXT NOT NULL REFERENCES scrapes(id) ON DELETE CASCADE,
                   text        TEXT NOT NULL,
                   context     TEXT,
                   message_ids TEXT,
                   created_at  INTEGER NOT NULL
                 );
                 CREATE TABLE action_items (
                   id          TEXT PRIMARY KEY,
                   scrape_id   TEXT NOT NULL REFERENCES scrapes(id) ON DELETE CASCADE,
                   text        TEXT NOT NULL,
                   assignee    TEXT,
                   due         TEXT,
                   message_ids TEXT,
                   created_at  INTEGER NOT NULL
                 );",
            )?;
        }

        let db = Db::open(&db_path)?;
        db.insert_running(
            "discord:legacy-channel",
            "legacy-channel",
            Some("legacy"),
            None,
            None,
            "tester",
        )?;
        db.mark_extracted(
            "discord:legacy-channel",
            Some("message-1"),
            Some("message-2"),
            1,
            "summary",
            &[DecisionCandidate {
                text: "Keep migrating old Crumb databases.".into(),
                context: None,
                message_ids: vec!["message-1".into()],
                dedupe_key: Some("migrate-old-dbs".into()),
                merge_with: None,
            }],
            &[ActionCandidate {
                text: "Verify old Crumb databases migrate at startup".into(),
                assignee_key: None,
                assignee: None,
                due: None,
                url: None,
                message_ids: vec!["message-2".into()],
                dedupe_key: Some("verify-old-dbs-migrate".into()),
                merge_with: None,
            }],
        )?;

        let sources = db.list_scrapes()?;
        assert_eq!(sources.len(), 1);
        assert_eq!(db.list_open_action_items()?.len(), 1);

        drop(db);
        let conn = rusqlite::Connection::open(&db_path)?;
        let applied = applied_migrations(&conn)?;
        assert_eq!(
            applied,
            vec![
                "0001_init",
                "0002_action_item_dedupe",
                "0003_assignee_keys",
                "0004_scrape_cursor",
                "0005_scrape_range",
                "0006_action_urls",
                "0007_watched_channels"
            ]
        );

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn action_dedupe_migration_removes_duplicate_source_rows_before_indexing() -> Result<()> {
        let db_path = std::env::temp_dir().join(format!(
            "crumb-duplicate-source-rows-{}.db",
            uuid::Uuid::new_v4()
        ));
        {
            let conn = rusqlite::Connection::open(&db_path)?;
            conn.execute_batch(
                "CREATE TABLE scrapes (
                   id            TEXT PRIMARY KEY,
                   source        TEXT NOT NULL CHECK(source IN ('discord')),
                   channel_id    TEXT NOT NULL,
                   channel_name  TEXT,
                   guild_id      TEXT,
                   guild_name    TEXT,
                   triggered_by  TEXT NOT NULL,
                   triggered_at  INTEGER NOT NULL,
                   status        TEXT NOT NULL CHECK(status IN ('running','extracted','failed')),
                   message_count INTEGER,
                   summary       TEXT,
                   error         TEXT
                 );
                 CREATE TABLE decisions (
                   id          TEXT PRIMARY KEY,
                   scrape_id   TEXT NOT NULL REFERENCES scrapes(id) ON DELETE CASCADE,
                   text        TEXT NOT NULL,
                   context     TEXT,
                   message_ids TEXT,
                   created_at  INTEGER NOT NULL,
                   dedupe_key  TEXT
                 );
                 CREATE TABLE action_items (
                   id          TEXT PRIMARY KEY,
                   scrape_id   TEXT NOT NULL REFERENCES scrapes(id) ON DELETE CASCADE,
                   text        TEXT NOT NULL,
                   assignee    TEXT,
                   due         TEXT,
                   message_ids TEXT,
                   created_at  INTEGER NOT NULL,
                   dedupe_key  TEXT
                 );
                 INSERT INTO scrapes (
                   id, source, channel_id, triggered_by, triggered_at, status,
                   message_count, summary
                 )
                 VALUES ('scrape-1', 'discord', 'channel-1', 'tester', 1,
                         'extracted', 2, 'summary');
                 INSERT INTO decisions (id, scrape_id, text, created_at, dedupe_key)
                 VALUES
                   ('decision-1', 'scrape-1', 'Decision', 1, 'same-decision'),
                   ('decision-2', 'scrape-1', 'Decision duplicate', 2, 'same-decision');
                 INSERT INTO action_items (id, scrape_id, text, created_at, dedupe_key)
                 VALUES
                   ('action-1', 'scrape-1', 'Action', 1, 'same-action'),
                   ('action-2', 'scrape-1', 'Action duplicate', 2, 'same-action');",
            )?;
        }

        let db = Db::open(&db_path)?;
        let detail = db.get_scrape("scrape-1")?.expect("scrape exists");
        assert_eq!(detail.decisions.len(), 1);
        assert_eq!(detail.action_items.len(), 1);

        drop(db);
        let conn = rusqlite::Connection::open(&db_path)?;
        let applied = applied_migrations(&conn)?;
        assert_eq!(
            applied,
            vec![
                "0001_init",
                "0002_action_item_dedupe",
                "0003_assignee_keys",
                "0004_scrape_cursor",
                "0005_scrape_range",
                "0006_action_urls",
                "0007_watched_channels"
            ]
        );

        let _ = std::fs::remove_file(db_path);
        Ok(())
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
            Some("message-1"),
            Some("message-1"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Need to ship the menu bar UX".into(),
                assignee_key: Some("discord:user:fox".into()),
                assignee: Some("fox".into()),
                due: None,
                url: None,
                message_ids: vec!["message-1".into()],
                dedupe_key: Some("ship menu bar ux".into()),
                merge_with: None,
            }],
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
            Some("message-1"),
            Some("message-1"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Ship the menu bar UX".into(),
                assignee_key: Some("discord:user:fox".into()),
                assignee: Some("fox".into()),
                due: None,
                url: None,
                message_ids: vec!["message-1".into()],
                dedupe_key: Some("ship menu bar ux".into()),
                merge_with: None,
            }],
        )?;

        let actions = db.list_open_action_items()?;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].evidence_count, 1);
        assert_eq!(actions[0].assignee_key.as_deref(), Some("discord:user:fox"));

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn dismissed_actions_can_be_listed_and_restored() -> Result<()> {
        let db_path =
            std::env::temp_dir().join(format!("crumb-action-status-{}.db", uuid::Uuid::new_v4()));
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
            Some("message-1"),
            Some("message-1"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Send launch checklist".into(),
                assignee_key: Some("discord:user:fox".into()),
                assignee: Some("fox".into()),
                due: Some("Friday".into()),
                url: None,
                message_ids: vec!["message-1".into()],
                dedupe_key: Some("send-launch-checklist".into()),
                merge_with: None,
            }],
        )?;

        let action_id = db.list_open_action_items()?[0].id.clone();
        db.set_action_status(&action_id, "done")?;
        assert!(db.list_open_action_items()?.is_empty());
        assert_eq!(db.list_action_items("dismissed")?.len(), 1);

        db.set_action_status(&action_id, "inbox")?;
        assert_eq!(db.list_open_action_items()?.len(), 1);
        assert!(db.list_action_items("dismissed")?.is_empty());

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn re_seen_dismissed_actions_return_to_inbox() -> Result<()> {
        let db_path =
            std::env::temp_dir().join(format!("crumb-action-reseen-{}.db", uuid::Uuid::new_v4()));
        let db = Db::open(&db_path)?;
        db.insert_running(
            "discord:channel-1",
            "channel-1",
            Some("Nelly (DM)"),
            None,
            None,
            "tester",
        )?;
        db.mark_extracted(
            "discord:channel-1",
            Some("100"),
            Some("100"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Merge approved PR #123".into(),
                assignee_key: Some("discord:user:fox".into()),
                assignee: Some("fox".into()),
                due: None,
                url: Some("https://github.com/example/repo/pull/123".into()),
                message_ids: vec!["100".into()],
                dedupe_key: Some("merge-approved-pr-github-com-example-repo-pull-123".into()),
                merge_with: None,
            }],
        )?;

        let action_id = db.list_open_action_items()?[0].id.clone();
        db.set_action_status(&action_id, "done")?;
        assert!(db.list_open_action_items()?.is_empty());

        db.mark_extracted(
            "discord:channel-1",
            Some("101"),
            Some("101"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Merge approved PR #123".into(),
                assignee_key: Some("discord:user:fox".into()),
                assignee: Some("fox".into()),
                due: None,
                url: Some("https://github.com/example/repo/pull/123".into()),
                message_ids: vec!["101".into()],
                dedupe_key: Some("merge-approved-pr-github-com-example-repo-pull-123".into()),
                merge_with: None,
            }],
        )?;

        let actions = db.list_open_action_items()?;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].id, action_id);
        assert_eq!(actions[0].status, "inbox");
        assert_eq!(actions[0].completed_at, None);

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn action_assignee_can_be_updated() -> Result<()> {
        let db_path =
            std::env::temp_dir().join(format!("crumb-action-assignee-{}.db", uuid::Uuid::new_v4()));
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
            Some("message-1"),
            Some("message-1"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Review launch plan".into(),
                assignee_key: Some("discord:user:fox".into()),
                assignee: Some("fox".into()),
                due: None,
                url: Some("https://github.com/example/repo/pull/123".into()),
                message_ids: vec!["message-1".into()],
                dedupe_key: Some("review-launch-plan".into()),
                merge_with: None,
            }],
        )?;

        let action_id = db.list_open_action_items()?[0].id.clone();
        assert_eq!(
            db.list_open_action_items()?[0].url.as_deref(),
            Some("https://github.com/example/repo/pull/123")
        );
        let updated = db.set_action_assignee(&action_id, None, Some("Arthur Tang"))?;
        assert_eq!(updated.assignee.as_deref(), Some("Arthur Tang"));
        assert_eq!(updated.assignee_key.as_deref(), Some("person:arthur-tang"));

        let updated = db.set_action_assignee(&action_id, None, Some(""))?;
        assert_eq!(updated.assignee, None);
        assert_eq!(updated.assignee_key, None);

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn discord_source_can_be_resolved_and_repaired_from_canonical_action() -> Result<()> {
        let db_path =
            std::env::temp_dir().join(format!("crumb-source-repair-{}.db", uuid::Uuid::new_v4()));
        let db = Db::open(&db_path)?;
        db.insert_running(
            "discord:channel-1",
            "channel-1",
            Some("dev"),
            None,
            None,
            "tester",
        )?;
        db.mark_extracted(
            "discord:channel-1",
            Some("message-1"),
            Some("message-1"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Review source deep link".into(),
                assignee_key: None,
                assignee: None,
                due: None,
                url: None,
                message_ids: vec!["message-1".into()],
                dedupe_key: Some("review-source-deep-link".into()),
                merge_with: None,
            }],
        )?;

        let action_id = db.list_open_action_items()?[0].id.clone();
        let source = db
            .discord_source_for_action(&action_id)?
            .expect("discord source");
        assert_eq!(source.channel_id, "channel-1");
        assert_eq!(source.guild_id, None);
        assert_eq!(
            db.latest_discord_message_id_for_action(&action_id, "channel-1")?
                .as_deref(),
            Some("message-1")
        );

        let repaired = db
            .update_discord_source_metadata(
                "channel-1",
                Some("dev"),
                Some("guild-1"),
                Some("Crumb"),
            )?
            .expect("repaired source");
        assert_eq!(repaired.guild_id.as_deref(), Some("guild-1"));
        assert_eq!(
            db.list_open_action_items()?[0].source_label.as_deref(),
            Some("Crumb · dev")
        );

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn latest_discord_message_id_uses_newest_evidence_and_highest_id() -> Result<()> {
        let db_path = std::env::temp_dir().join(format!(
            "crumb-action-message-id-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Db::open(&db_path)?;
        db.insert_running(
            "discord:channel-1",
            "channel-1",
            Some("dev"),
            Some("guild-1"),
            Some("Crumb"),
            "tester",
        )?;
        db.mark_extracted(
            "discord:channel-1",
            Some("100"),
            Some("100"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Review source deep link".into(),
                assignee_key: None,
                assignee: None,
                due: None,
                url: None,
                message_ids: vec!["100".into()],
                dedupe_key: Some("review-source-deep-link".into()),
                merge_with: None,
            }],
        )?;
        db.mark_extracted(
            "discord:channel-1",
            Some("120"),
            Some("125"),
            2,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Review source deep link".into(),
                assignee_key: None,
                assignee: None,
                due: None,
                url: None,
                message_ids: vec!["120".into(), "125".into()],
                dedupe_key: Some("review-source-deep-link".into()),
                merge_with: None,
            }],
        )?;

        let action_id = db.list_open_action_items()?[0].id.clone();
        assert_eq!(
            db.latest_discord_message_id_for_action(&action_id, "channel-1")?
                .as_deref(),
            Some("125")
        );

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn repeated_scrapes_expand_message_range() -> Result<()> {
        let db_path =
            std::env::temp_dir().join(format!("crumb-scrape-range-{}.db", uuid::Uuid::new_v4()));
        let db = Db::open(&db_path)?;
        db.insert_running(
            "discord:channel-1",
            "channel-1",
            Some("dev"),
            Some("guild-1"),
            Some("Crumb"),
            "tester",
        )?;
        db.mark_extracted(
            "discord:channel-1",
            Some("100"),
            Some("200"),
            1,
            "summary",
            &[],
            &[],
        )?;
        db.mark_extracted(
            "discord:channel-1",
            Some("50"),
            Some("250"),
            1,
            "summary",
            &[],
            &[],
        )?;

        let range = db.scraped_message_range("discord:channel-1")?;
        assert_eq!(range.first_message_id.as_deref(), Some("50"));
        assert_eq!(range.last_message_id.as_deref(), Some("250"));

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn watched_channels_can_be_persisted_and_unwatched() -> Result<()> {
        let db_path =
            std::env::temp_dir().join(format!("crumb-watch-channel-{}.db", uuid::Uuid::new_v4()));
        let db = Db::open(&db_path)?;

        db.watch_channel(
            "channel-1",
            Some("dev"),
            Some("guild-1"),
            Some("Crumb"),
            "tester",
            Some("100"),
        )?;
        let watched = db.list_watched_channels()?;
        assert_eq!(watched.len(), 1);
        assert_eq!(watched[0].channel_id, "channel-1");
        assert_eq!(watched[0].last_seen_message_id.as_deref(), Some("100"));

        db.update_watch_cursor("channel-1", Some("150"))?;
        let watched = db.list_watched_channels()?;
        assert_eq!(watched[0].last_seen_message_id.as_deref(), Some("150"));

        assert!(db.unwatch_channel("channel-1")?);
        assert!(db.list_watched_channels()?.is_empty());

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn deleting_source_removes_its_actions() -> Result<()> {
        let db_path =
            std::env::temp_dir().join(format!("crumb-delete-source-{}.db", uuid::Uuid::new_v4()));
        let db = Db::open(&db_path)?;
        db.insert_running(
            "discord:channel-1",
            "channel-1",
            Some("dev"),
            Some("guild-1"),
            Some("Crumb"),
            "tester",
        )?;
        db.mark_extracted(
            "discord:channel-1",
            Some("100"),
            Some("100"),
            1,
            "summary",
            &[],
            &[ActionCandidate {
                text: "Retest source scraping".into(),
                assignee_key: Some("discord:user:fox".into()),
                assignee: Some("fox".into()),
                due: Some("today".into()),
                url: None,
                message_ids: vec!["100".into()],
                dedupe_key: Some("retest-source-scraping".into()),
                merge_with: None,
            }],
        )?;

        assert_eq!(db.list_scrapes()?.len(), 1);
        assert_eq!(
            db.scraped_message_range("discord:channel-1")?
                .first_message_id
                .as_deref(),
            Some("100")
        );
        assert_eq!(
            db.scraped_message_range("discord:channel-1")?
                .last_message_id
                .as_deref(),
            Some("100")
        );
        assert_eq!(db.list_open_action_items()?.len(), 1);

        db.delete_source("discord:channel-1")?;

        assert!(db.list_scrapes()?.is_empty());
        assert!(db.get_scrape("discord:channel-1")?.is_none());
        assert!(db.list_action_items("all")?.is_empty());

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[test]
    fn repeated_source_scrapes_accumulate_items_without_source_duplicates() -> Result<()> {
        let db_path =
            std::env::temp_dir().join(format!("crumb-source-upsert-{}.db", uuid::Uuid::new_v4()));
        let db = Db::open(&db_path)?;
        db.insert_running(
            "discord:channel-1",
            "channel-1",
            Some("dev"),
            Some("guild-1"),
            Some("Crumb"),
            "tester",
        )?;
        db.mark_extracted(
            "discord:channel-1",
            Some("message-1"),
            Some("message-2"),
            1,
            "first summary",
            &[DecisionCandidate {
                text: "Ship the menu bar first.".into(),
                context: Some("ship it".into()),
                message_ids: vec!["message-1".into()],
                dedupe_key: Some("ship-menu-bar".into()),
                merge_with: None,
            }],
            &[ActionCandidate {
                text: "Provide Lew with partner user IDs for experiment whitelisting".into(),
                assignee_key: Some("discord:user:arthur".into()),
                assignee: Some("Arthur Tang".into()),
                due: Some("this week".into()),
                url: None,
                message_ids: vec!["message-2".into()],
                dedupe_key: Some("partner-user-ids-for-whitelisting".into()),
                merge_with: None,
            }],
        )?;

        db.insert_running(
            "discord:channel-1",
            "channel-1",
            Some("dev"),
            Some("guild-1"),
            Some("Crumb"),
            "tester",
        )?;
        db.mark_extracted(
            "discord:channel-1",
            Some("message-1"),
            Some("message-3"),
            2,
            "second summary",
            &[DecisionCandidate {
                text: "Ship the menu bar first.".into(),
                context: Some("ship it".into()),
                message_ids: vec!["message-1".into()],
                dedupe_key: Some("ship-menu-bar".into()),
                merge_with: None,
            }],
            &[
                ActionCandidate {
                    text: "Provide Lew with the partner user IDs and application IDs to add to the experiment.".into(),
                    assignee_key: Some("discord:user:arthur".into()),
                    assignee: Some("Arthur Tang".into()),
                    due: Some("this week".into()),
                    url: None,
                    message_ids: vec!["message-2".into()],
                    dedupe_key: Some("partner-user-ids-for-experiment".into()),
                    merge_with: None,
                },
                ActionCandidate {
                    text: "Add screenshots to partner docs".into(),
                    assignee_key: Some("discord:user:anthony".into()),
                    assignee: Some("Anthony".into()),
                    due: Some("today".into()),
                    url: None,
                    message_ids: vec!["message-3".into()],
                    dedupe_key: Some("add-screenshots-to-partner-docs".into()),
                    merge_with: None,
                },
            ],
        )?;

        let sources = db.list_scrapes()?;
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, "discord:channel-1");

        let detail = db.get_scrape("discord:channel-1")?.expect("source exists");
        assert_eq!(detail.decisions.len(), 1);
        assert_eq!(detail.action_items.len(), 2);

        let actions = db.list_open_action_items()?;
        assert_eq!(actions.len(), 2);

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    fn applied_migrations(conn: &rusqlite::Connection) -> Result<Vec<String>> {
        let mut stmt = conn.prepare("SELECT id FROM schema_migrations ORDER BY applied_at, id")?;
        let applied = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(applied)
    }
}

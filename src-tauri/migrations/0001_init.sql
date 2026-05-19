CREATE TABLE IF NOT EXISTS scrapes (
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

CREATE TABLE IF NOT EXISTS decisions (
  id          TEXT PRIMARY KEY,
  scrape_id   TEXT NOT NULL REFERENCES scrapes(id) ON DELETE CASCADE,
  text        TEXT NOT NULL,
  context     TEXT,
  message_ids TEXT,
  created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS action_items (
  id          TEXT PRIMARY KEY,
  scrape_id   TEXT NOT NULL REFERENCES scrapes(id) ON DELETE CASCADE,
  text        TEXT NOT NULL,
  assignee    TEXT,
  due         TEXT,
  message_ids TEXT,
  created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_action_items (
  id              TEXT PRIMARY KEY,
  title           TEXT NOT NULL,
  status          TEXT NOT NULL CHECK(status IN ('inbox','active','snoozed','done','archived')) DEFAULT 'inbox',
  source_kind     TEXT NOT NULL CHECK(source_kind IN ('discord','asana','manual','mixed')),
  source_scope    TEXT NOT NULL,
  source_label    TEXT,
  dedupe_key      TEXT NOT NULL,
  assignee        TEXT,
  due             TEXT,
  priority        INTEGER NOT NULL DEFAULT 0,
  relevance_score REAL NOT NULL DEFAULT 0,
  first_seen_at   INTEGER NOT NULL,
  last_seen_at    INTEGER NOT NULL,
  completed_at    INTEGER,
  snoozed_until   INTEGER,
  latest_context  TEXT,
  UNIQUE(source_kind, source_scope, dedupe_key)
);

CREATE TABLE IF NOT EXISTS action_item_evidence (
  id                 TEXT PRIMARY KEY,
  action_item_id     TEXT NOT NULL REFERENCES canonical_action_items(id) ON DELETE CASCADE,
  source_kind        TEXT NOT NULL CHECK(source_kind IN ('discord','asana','manual')),
  source_id          TEXT NOT NULL,
  source_label       TEXT,
  scrape_id          TEXT REFERENCES scrapes(id) ON DELETE SET NULL,
  extracted_text     TEXT NOT NULL,
  context            TEXT,
  message_ids        TEXT,
  evidence_key       TEXT NOT NULL,
  created_at         INTEGER NOT NULL,
  UNIQUE(action_item_id, evidence_key)
);

CREATE INDEX IF NOT EXISTS idx_scrapes_triggered_at ON scrapes(triggered_at DESC);
CREATE INDEX IF NOT EXISTS idx_decisions_scrape    ON decisions(scrape_id);
CREATE INDEX IF NOT EXISTS idx_actions_scrape      ON action_items(scrape_id);
CREATE INDEX IF NOT EXISTS idx_canonical_actions_main
  ON canonical_action_items(status, snoozed_until, last_seen_at DESC);
CREATE INDEX IF NOT EXISTS idx_action_evidence_action
  ON action_item_evidence(action_item_id, created_at DESC);

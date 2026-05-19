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

CREATE INDEX IF NOT EXISTS idx_scrapes_triggered_at ON scrapes(triggered_at DESC);
CREATE INDEX IF NOT EXISTS idx_decisions_scrape    ON decisions(scrape_id);
CREATE INDEX IF NOT EXISTS idx_actions_scrape      ON action_items(scrape_id);

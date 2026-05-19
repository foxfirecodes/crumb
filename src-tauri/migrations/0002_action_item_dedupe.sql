CREATE UNIQUE INDEX IF NOT EXISTS idx_decisions_source_key
  ON decisions(scrape_id, dedupe_key);

CREATE UNIQUE INDEX IF NOT EXISTS idx_action_items_source_key
  ON action_items(scrape_id, dedupe_key);

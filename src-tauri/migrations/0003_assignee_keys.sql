CREATE INDEX IF NOT EXISTS idx_canonical_actions_assignee
  ON canonical_action_items(assignee_key, status, last_seen_at DESC);

CREATE INDEX IF NOT EXISTS idx_action_items_assignee
  ON action_items(scrape_id, assignee_key);

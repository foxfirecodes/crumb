CREATE INDEX IF NOT EXISTS idx_scrapes_last_message
  ON scrapes(source, channel_id, last_message_id);

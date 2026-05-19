CREATE INDEX IF NOT EXISTS idx_scrapes_message_range
  ON scrapes(source, channel_id, first_message_id, last_message_id);

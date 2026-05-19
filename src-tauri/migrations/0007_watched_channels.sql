CREATE INDEX IF NOT EXISTS idx_watched_channels_poll
  ON watched_channels(source, watched_at);

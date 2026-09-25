ALTER TABLE devices ADD COLUMN whitelisted INTEGER NOT NULL DEFAULT 1;
CREATE INDEX IF NOT EXISTS devices_whitelist_idx ON devices(whitelisted, disabled, created_at DESC);

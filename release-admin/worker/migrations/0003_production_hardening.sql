ALTER TABLE publish_batches ADD COLUMN request_hash TEXT;
CREATE INDEX IF NOT EXISTS publish_batches_request_hash_idx ON publish_batches(requested_by, idempotency_key, request_hash);

CREATE TABLE IF NOT EXISTS rate_limits (
  scope TEXT PRIMARY KEY,
  count INTEGER NOT NULL,
  window_started_at INTEGER NOT NULL
);

-- 新登记设备必须由管理员明确加入灰度白名单，避免公开登记入口被滥用。
UPDATE devices SET whitelisted=0;

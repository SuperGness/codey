PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS admins (
  id TEXT PRIMARY KEY,
  username TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('admin','operator','viewer')) DEFAULT 'operator',
  disabled INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  admin_id TEXT NOT NULL REFERENCES admins(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE,
  expires_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_expiry_idx ON sessions(expires_at);

CREATE TABLE IF NOT EXISTS versions (
  id TEXT PRIMARY KEY,
  version TEXT NOT NULL UNIQUE,
  release_notes TEXT,
  manifest_url TEXT,
  artifact_url TEXT,
  artifact_sha256 TEXT,
  artifact_size INTEGER,
  status TEXT NOT NULL CHECK (status IN ('draft','pending','gray','full','paused','withdrawn','archived')) DEFAULT 'draft',
  created_by TEXT NOT NULL REFERENCES admins(id),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  active_publish_id TEXT
);
CREATE INDEX IF NOT EXISTS versions_status_idx ON versions(status, created_at DESC);

CREATE TABLE IF NOT EXISTS devices (
  id TEXT PRIMARY KEY,
  machine_no TEXT NOT NULL UNIQUE,
  install_key_hash TEXT UNIQUE,
  user_ref TEXT,
  platform TEXT,
  arch TEXT,
  current_version TEXT,
  last_seen_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  disabled INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS devices_user_ref_idx ON devices(user_ref);
CREATE INDEX IF NOT EXISTS devices_last_seen_idx ON devices(last_seen_at);

CREATE TABLE IF NOT EXISTS publish_batches (
  id TEXT PRIMARY KEY,
  version_id TEXT NOT NULL REFERENCES versions(id),
  mode TEXT NOT NULL CHECK (mode IN ('full','gray','targeted')),
  status TEXT NOT NULL CHECK (status IN ('scheduled','running','paused','completed','withdrawn','failed')) DEFAULT 'scheduled',
  percentage INTEGER,
  device_limit INTEGER,
  scheduled_at INTEGER,
  notes TEXT,
  requested_by TEXT NOT NULL REFERENCES admins(id),
  idempotency_key TEXT NOT NULL,
  target_count INTEGER NOT NULL DEFAULT 0,
  success_count INTEGER NOT NULL DEFAULT 0,
  failure_count INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(requested_by, idempotency_key)
);
CREATE INDEX IF NOT EXISTS publish_batches_version_idx ON publish_batches(version_id, created_at DESC);
CREATE INDEX IF NOT EXISTS publish_batches_status_idx ON publish_batches(status, scheduled_at);

CREATE TABLE IF NOT EXISTS publish_targets (
  batch_id TEXT NOT NULL REFERENCES publish_batches(id) ON DELETE CASCADE,
  device_id TEXT NOT NULL REFERENCES devices(id),
  status TEXT NOT NULL CHECK (status IN ('pending','offered','downloaded','installed','failed','withdrawn')) DEFAULT 'pending',
  error_code TEXT,
  error_message TEXT,
  offered_at INTEGER,
  downloaded_at INTEGER,
  installed_at INTEGER,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (batch_id, device_id)
);
CREATE INDEX IF NOT EXISTS publish_targets_device_idx ON publish_targets(device_id, status);
CREATE INDEX IF NOT EXISTS publish_targets_batch_idx ON publish_targets(batch_id, status);

CREATE TABLE IF NOT EXISTS audit_logs (
  id TEXT PRIMARY KEY,
  admin_id TEXT REFERENCES admins(id),
  action TEXT NOT NULL,
  entity_type TEXT NOT NULL,
  entity_id TEXT,
  metadata_json TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS audit_logs_created_idx ON audit_logs(created_at DESC);
CREATE INDEX IF NOT EXISTS audit_logs_entity_idx ON audit_logs(entity_type, entity_id, created_at DESC);

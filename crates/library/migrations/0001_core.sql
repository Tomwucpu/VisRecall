CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at INTEGER NOT NULL
);

CREATE TABLE items (
  rowid INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL CHECK (kind IN ('image','text','link')),
  status TEXT NOT NULL CHECK (status IN ('parsing','ready','pending','failed')),
  captured_at INTEGER NOT NULL,
  source_app TEXT,
  source_title TEXT,
  source_url TEXT,
  width INTEGER,
  height INTEGER,
  char_count INTEGER,
  original_path TEXT,
  thumb_path TEXT,
  note TEXT NOT NULL DEFAULT '',
  content_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_items_captured ON items(captured_at DESC);
CREATE INDEX idx_items_status ON items(status);
CREATE INDEX idx_items_hash ON items(content_hash);

CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE jobs (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL,
  stage TEXT NOT NULL,
  revision INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed','cancelled')),
  priority INTEGER NOT NULL DEFAULT 1,
  attempts INTEGER NOT NULL DEFAULT 0,
  next_run_at INTEGER,
  last_error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (item_id, stage, revision)
);
CREATE INDEX idx_jobs_due ON jobs(state, next_run_at, priority);

-- 主动导入任务与只读目录引用；旧素材默认仍为托管模式。
ALTER TABLE items ADD COLUMN storage_mode TEXT NOT NULL DEFAULT 'managed'
  CHECK (storage_mode IN ('managed', 'reference'));

CREATE TABLE import_sources (
  id TEXT PRIMARY KEY,
  root_path TEXT NOT NULL,
  path_key TEXT NOT NULL UNIQUE,
  include_subdirectories INTEGER NOT NULL DEFAULT 1,
  include_images INTEGER NOT NULL DEFAULT 1,
  include_text INTEGER NOT NULL DEFAULT 1,
  ai_allowed INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'available'
    CHECK (status IN ('available','offline','removed')),
  last_scanned_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE external_assets (
  item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  source_id TEXT NOT NULL REFERENCES import_sources(id) ON DELETE RESTRICT,
  relative_path TEXT NOT NULL,
  path_key TEXT NOT NULL,
  size INTEGER NOT NULL,
  modified_at INTEGER NOT NULL,
  file_identity TEXT,
  indexed_hash TEXT,
  indexed_at INTEGER,
  source_status TEXT NOT NULL DEFAULT 'available'
    CHECK (source_status IN ('available','missing','permission_denied','changed')),
  UNIQUE (source_id, path_key)
);
CREATE INDEX idx_external_assets_source ON external_assets(source_id, source_status);

CREATE TABLE import_tasks (
  id TEXT PRIMARY KEY,
  source_id TEXT REFERENCES import_sources(id) ON DELETE SET NULL,
  kind TEXT NOT NULL CHECK (kind IN ('files','directory_scan','directory_import','refresh')),
  status TEXT NOT NULL CHECK (status IN
    ('pending','running','scanned','completed','completed_with_errors','cancelled','interrupted','failed')),
  total_count INTEGER NOT NULL DEFAULT 0,
  processed_count INTEGER NOT NULL DEFAULT 0,
  succeeded_count INTEGER NOT NULL DEFAULT 0,
  skipped_count INTEGER NOT NULL DEFAULT 0,
  failed_count INTEGER NOT NULL DEFAULT 0,
  original_bytes INTEGER NOT NULL DEFAULT 0,
  cancel_requested INTEGER NOT NULL DEFAULT 0,
  options_json TEXT NOT NULL DEFAULT '{}',
  error_summary TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_import_tasks_updated ON import_tasks(updated_at DESC);

CREATE TABLE import_task_entries (
  rowid INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL REFERENCES import_tasks(id) ON DELETE CASCADE,
  entry_key TEXT NOT NULL,
  relative_path TEXT NOT NULL,
  display_name TEXT NOT NULL,
  size INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN
    ('pending','ready','imported','skipped','failed')),
  error_code TEXT,
  error_message TEXT,
  item_id TEXT,
  UNIQUE (task_id, entry_key)
);
CREATE INDEX idx_import_task_entries_page ON import_task_entries(task_id, rowid);

CREATE TABLE source_exclusions (
  source_id TEXT NOT NULL REFERENCES import_sources(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL,
  path_key TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (source_id, path_key)
);

-- 新库格式由支持引用生命周期的版本读取，避免旧删除语义被误认为兼容。
INSERT OR REPLACE INTO settings(key, value_json) VALUES
  ('minimum_reader_version', '"0.2.0"'),
  ('library_format', '4');

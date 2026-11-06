-- 迁移 0002：标签、标签关联与抑制记录（阶段 02，design §4.1）。
--
-- 1. tags.name 的唯一性用 NOCASE 索引表达，避免 "Work"/"work" 分裂成两个标签。
-- 2. item_tags 记录「这次关联」的来源（auto=阶段 03 自动标注，manual=用户手动）。
-- 3. tag_suppressions 记录用户手动移除过的自动标签，阶段 03 重新解析时不得贴回。
-- 4. item_tags / tag_suppressions 对 items(id) 建外键并 ON DELETE CASCADE；
--    开库时 PRAGMA foreign_keys=ON，删除素材时由 SQLite 完成级联清理。

CREATE TABLE tags (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('auto','manual')),
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_tags_name_nocase ON tags(name COLLATE NOCASE);

CREATE TABLE item_tags (
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  origin TEXT NOT NULL DEFAULT 'manual' CHECK (origin IN ('auto','manual')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (item_id, tag_id)
);
CREATE INDEX idx_item_tags_tag ON item_tags(tag_id);

CREATE TABLE tag_suppressions (
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  tag_name TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (item_id, tag_name)
);

-- 阶段 03：AI 解析结果、全文索引、向量表与检索记录（design §4.1）。

-- 元数据 revision：jobs.revision 的真相来源（D3-03）。
ALTER TABLE items ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;

-- 1) AI 解析结果。
CREATE TABLE insights (
  item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  summary TEXT,
  summary_source TEXT NOT NULL DEFAULT 'none'
    CHECK (summary_source IN ('none','vlm','ocr','extractive','user')),
  content_type TEXT, primary_color TEXT, chart_type TEXT, ui_type TEXT,
  entities_json TEXT,
  ocr_text TEXT, ocr_boxes_json TEXT, ocr_engine TEXT, ocr_confidence REAL,
  latex TEXT, has_formula INTEGER NOT NULL DEFAULT 0,
  model_provider TEXT, model_name TEXT, confidence REAL,
  summary_user_edited INTEGER NOT NULL DEFAULT 0,
  tags_user_edited INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_insights_color ON insights(primary_color);
CREATE INDEX idx_insights_chart ON insights(chart_type);
CREATE INDEX idx_insights_type  ON insights(content_type);

-- 2) 全文索引：contentful（保留分词后正文以支持 snippet()/highlight()）。
--    写入前由应用层 jieba 分词、空格连接；rowid 与 items.rowid 对齐
--    （items 为 INTEGER PRIMARY KEY AUTOINCREMENT，rowid 稳定不复用）。
CREATE VIRTUAL TABLE items_fts USING fts5(
  title, summary, ocr_text, note, tags, entities, original_text,
  tokenize = 'unicode61 remove_diacritics 2'
);

-- 3) 向量主表（int8 暴力路径；蓝绿重建时建 *_v2 后切 vector.active_table 指针）。
--    主键即 items.rowid：删除素材时靠外键级联清理（items 有 rowid，见 0001）。
CREATE TABLE item_vectors_int8 (
  item_rowid INTEGER PRIMARY KEY REFERENCES items(rowid) ON DELETE CASCADE,
  embedding  BLOB NOT NULL,
  dim        INTEGER NOT NULL,
  model_id   TEXT NOT NULL
);

-- 4) 检索记录（评测与反馈数据；无历史 UI）。
CREATE TABLE queries (
  id TEXT PRIMARY KEY,
  raw TEXT NOT NULL,
  parsed_json TEXT,
  result_ids_json TEXT,
  feedback TEXT,
  created_at INTEGER NOT NULL
);

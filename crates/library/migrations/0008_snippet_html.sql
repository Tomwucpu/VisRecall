-- 富文本拖拽留下的 HTML 片段（复制时写回 CF_HTML，让粘贴保留图片与基本格式）。
--
-- 单独建表而不是写 texts/<id>.html：孤儿扫描的白名单是 originals/、texts/，
-- 侧车文件会被当成孤儿统计甚至清理；这里靠 ON DELETE CASCADE 随素材一起消失。
CREATE TABLE snippet_html (
  item_id    TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  html       TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

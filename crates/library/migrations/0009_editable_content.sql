-- 用户手工编辑 OCR 文本的保护位：不对外展示，只阻止重新识别覆盖。
-- 摘要的对应列 summary_user_edited 已在 0003 建好。
ALTER TABLE insights ADD COLUMN ocr_user_edited INTEGER NOT NULL DEFAULT 0;

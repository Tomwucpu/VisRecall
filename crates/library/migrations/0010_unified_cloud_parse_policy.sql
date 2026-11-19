-- 云端解析改由「设置 → 模型」的全局开关统一决定。
-- 来源级授权不再参与判定；列暂时保留并统一归一为允许，避免 SQLite DROP COLUMN 的兼容风险。
UPDATE import_sources SET ai_allowed = 1;
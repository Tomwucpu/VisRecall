-- 目录来源统一记录不带 `\\?\` 前缀的完整路径。
--
-- `fs::canonicalize` 在 Windows 上返回扩展长度前缀，旧版本把这种写法原样存进
-- `import_sources`，导致同一目录用另一种写法再选一次会被当成新来源。
-- 只改写「盘符 + 冒号」形态（`\\?\C:\...`）：UNC、卷 GUID 与超长路径简化后无法保证
-- 等价，与 `normalize_root_path`（dunce）保持一致，保持原样。
UPDATE import_sources
SET root_path = CASE
      WHEN root_path LIKE '\\?\_:%' THEN substr(root_path, 5)
      ELSE root_path
    END,
    path_key = CASE
      WHEN path_key LIKE '\\?\_:%' THEN substr(path_key, 5)
      ELSE path_key
    END;

-- 旧版本删除引用素材时写入排除记录，重新扫描同一目录会把整批文件静默过滤掉。
-- 删除语义已改为「清掉该路径的排除记录」，这里清理历史残留，让重新导入能正常进行。
DELETE FROM source_exclusions;

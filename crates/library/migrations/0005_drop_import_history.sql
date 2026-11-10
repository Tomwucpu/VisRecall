-- 导入任务不再保留历史：运行期仍用 import_tasks / import_task_entries 承载进度与
-- 扫描预览，任务一进终态就删行。这里清掉升级前遗留的历史记录，
-- 条目由 `ON DELETE CASCADE` 一并删除。
DELETE FROM import_tasks;

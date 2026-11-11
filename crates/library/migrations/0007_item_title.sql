-- 用户可编辑标题：NULL 表示未改名，读路径回落到 source_title。
-- 刻意不回填：个人库规模下次迁移不写全表，旧数据语义完全不变。
ALTER TABLE items ADD COLUMN title TEXT;

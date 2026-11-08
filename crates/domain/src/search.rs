//! 检索侧的共享常量与类型。
//!
//! 分词器版本必须同时被「索引写入侧」(`library`) 与「查询侧」(`search`) 读到，
//! 因此常量放在 domain：`crates/search` 只是 re-export，保证只有一处真相。

/// 分词器版本（写入 `settings['fts.tokenizer_version']`，与依赖版本同步维护）。
pub const TOKENIZER_VERSION: &str = "jieba-rs-0.11";

/// `settings` 键：分词器版本。
pub const TOKENIZER_VERSION_KEY: &str = "fts.tokenizer_version";

/// `settings` 键：RRF 参数（`{"k":60,"weights":{"fts":1.0,"semantic":1.0}}`）。
pub const RRF_SETTINGS_KEY: &str = "search.rrf";

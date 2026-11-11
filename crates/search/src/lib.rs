//! VisRecall 检索层：分词、MATCH 构造、search document、查询解析、三路召回与融合。
//!
//! 依赖方向（design §3）：`search` 只依赖 `domain` 与 `library`；**不依赖 `ai`**——
//! 查询嵌入通过 `app-core` 注入的 `Arc<dyn EmbeddingProvider>` 传入。
//!
//! jieba 只在本 crate 出现（`tokenizer.rs`），`pipeline` 通过它构造索引文本，
//! 保证写入侧与查询侧词形一致。

pub mod document;
pub mod execute;
pub mod filters;
pub mod fts_query;
pub mod parser;
pub mod rank;
pub mod tokenizer;

pub use document::{build_search_document, DocumentFields, DOC_CHAR_BUDGET};
pub use execute::{search, SearchDeps, SearchOutcome, SearchRequest, CHANNEL_LIMIT, MAX_LIMIT};
pub use filters::{
    matched_labels, matched_labels_with_filters, passes, passes_with_filters, to_sql,
    to_sql_with_filters, SqlFilter,
};
pub use fts_query::{build_match_expr, escape_phrase};
pub use parser::{parse_query, parse_query_at};
pub use rank::{rrf, RrfConfig, DEFAULT_RRF_K, DEFAULT_WEIGHTS};
pub use tokenizer::{terms_for_query, tokenize_for_fts, words, TOKENIZER_VERSION};

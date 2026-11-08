//! VisRecall 检索层：分词、MATCH 构造、search document、查询解析、三路召回与融合。
//!
//! 依赖方向（design §3）：`search` 只依赖 `domain` 与 `library`；**不依赖 `ai`**——
//! 查询嵌入通过 `app-core` 注入的 `Arc<dyn EmbeddingProvider>` 传入。
//!
//! jieba 只在本 crate 出现（`tokenizer.rs`），`pipeline` 通过它构造索引文本，
//! 保证写入侧与查询侧词形一致。

pub mod document;
pub mod fts_query;
pub mod tokenizer;

pub use document::{build_search_document, DocumentFields, DOC_CHAR_BUDGET};
pub use fts_query::{build_match_expr, escape_phrase};
pub use tokenizer::{terms_for_query, tokenize_for_fts, words, TOKENIZER_VERSION};

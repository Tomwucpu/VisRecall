//! `VectorIndex` 的暴力 int8 实现（design A2 / §4.1）。
//!
//! V0.1 不做 ANN：每条查询把候选行读出来在 Rust 侧算 int8 余弦，20 条规模下单次
//! 检索在毫秒级；设计上限约 10 万条，超出再换 `sqlite-vec`/HNSW（trait 已留位）。
//!
//! 数据一致性：
//! - `add` 校验 `model_id` 与库内固定的嵌入空间一致，防止把两个模型的向量混进同一张表；
//! - `search` 跳过 `dim` 与查询不一致的行并 `warn`（脏数据不毒化排序）；
//! - 读的是 `vector.active_table` 指针，蓝绿重建期间检索永不读半成品表。

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;

use visrecall_domain::providers::VectorIndex;
use visrecall_domain::vector_math::{cosine_i8, quantize_i8};
use visrecall_domain::AppError;

use crate::db::Library;

/// 暴力 int8 向量索引（共享 `Library` 的单连接）。
pub struct BruteForceInt8 {
    library: Arc<Library>,
}

impl BruteForceInt8 {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }

    pub fn library(&self) -> &Arc<Library> {
        &self.library
    }

    fn pinned_space(&self) -> Result<(String, u32), AppError> {
        self.library.embedding_space().ok_or_else(|| {
            AppError::InvalidInput(
                "embedding space is not pinned yet (settings['embedding.model_id'] missing)".into(),
            )
        })
    }

    /// 带显式模型 id 的写入（pipeline 用）：与库内固定的嵌入空间不一致时直接报错，
    /// 避免把两个模型的向量混进同一张表。
    pub fn add_with_model(&self, rowid: i64, vec8: &[u8], model_id: &str) -> Result<(), AppError> {
        if vec8.is_empty() {
            return Err(AppError::InvalidInput("empty vector".into()));
        }
        let (pinned_id, pinned_dim) = self.pinned_space()?;
        if pinned_id != model_id {
            return Err(AppError::InvalidInput(format!(
                "embedding model mismatch: library pinned {pinned_id}, index uses {model_id}"
            )));
        }
        if pinned_dim as usize != vec8.len() {
            return Err(AppError::InvalidInput(format!(
                "vector dim mismatch: library {pinned_dim}, got {}",
                vec8.len()
            )));
        }
        self.library
            .vector_upsert(rowid, vec8, pinned_dim, model_id)
    }
}

/// 一条打分结果。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Scored {
    score: f32,
    rowid: i64,
}

impl Eq for Scored {}

impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Scored {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            // 分数相同时按 rowid 兜底，保证结果稳定可回放
            .then_with(|| self.rowid.cmp(&other.rowid))
    }
}

/// Top-K 选择：用 `BinaryHeap<Reverse<Scored>>`（最小堆），堆顶是当前最差的一条。
///
/// 自然序（分数大者大）+ `Reverse` 比手写反向 `Ord` 更不容易出错——第一版就是因为
/// 误把 `BinaryHeap` 当最小堆用（`peek()` 其实返回最大值）而把结果整个搞反。
pub fn select_top_k(scored: impl IntoIterator<Item = (i64, f32)>, top_k: usize) -> Vec<(i64, f32)> {
    if top_k == 0 {
        return Vec::new();
    }
    let mut heap: BinaryHeap<Reverse<Scored>> = BinaryHeap::with_capacity(top_k + 1);
    for (rowid, score) in scored {
        let candidate = Scored { score, rowid };
        if heap.len() < top_k {
            heap.push(Reverse(candidate));
        } else if let Some(Reverse(worst)) = heap.peek() {
            if candidate > *worst {
                heap.pop();
                heap.push(Reverse(candidate));
            }
        }
    }
    let mut out: Vec<(i64, f32)> = heap
        .into_iter()
        .map(|Reverse(item)| (item.rowid, item.score))
        .collect();
    // 相似度降序；同分按 rowid 升序（稳定可回放）
    out.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

impl VectorIndex for BruteForceInt8 {
    fn add(&self, rowid: i64, vec8: &[u8]) -> Result<(), AppError> {
        let (model_id, _) = self.pinned_space()?;
        self.add_with_model(rowid, vec8, &model_id)
    }

    fn remove(&self, rowid: i64) -> Result<(), AppError> {
        self.library.vector_delete(rowid)
    }

    fn search(
        &self,
        query: &[f32],
        top_k: usize,
        candidates: Option<&[i64]>,
    ) -> Result<Vec<(i64, f32)>, AppError> {
        if query.is_empty() || top_k == 0 {
            return Ok(Vec::new());
        }
        if matches!(candidates, Some(ids) if ids.is_empty()) {
            return Ok(Vec::new());
        }
        let query_i8 = quantize_i8(query);
        let rows = self.library.vector_rows(candidates)?;

        let scored = rows.into_iter().filter_map(|row| {
            if row.embedding.len() != query_i8.len() {
                tracing::warn!(
                    rowid = row.rowid,
                    dim = row.dim,
                    query_dim = query_i8.len(),
                    "skipping vector row with mismatched dimension"
                );
                return None;
            }
            Some((row.rowid, cosine_i8(&query_i8, &row.embedding)))
        });
        Ok(select_top_k(scored, top_k))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_k_keeps_the_best_scores_in_descending_order() {
        let out = select_top_k([(1, 0.1), (2, 0.9), (3, 0.5)], 2);
        assert_eq!(out, vec![(2, 0.9), (3, 0.5)]);
    }

    #[test]
    fn top_k_ties_break_by_rowid() {
        let out = select_top_k([(5, 0.5), (2, 0.5), (9, 0.5)], 3);
        assert_eq!(
            out.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![2, 5, 9]
        );
    }

    #[test]
    fn top_k_with_more_slots_than_items() {
        let out = select_top_k([(1, 0.2), (2, 0.4)], 10);
        assert_eq!(out, vec![(2, 0.4), (1, 0.2)]);
        assert!(select_top_k([(1, 0.2)], 0).is_empty());
    }
}

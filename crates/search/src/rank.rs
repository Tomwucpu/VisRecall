//! RRF 融合（Reciprocal Rank Fusion，Cormack 2009；design §6.3）。
//!
//! `score = Σ_ch w_ch / (k + rank_ch)`，`rank` 从 1 起，`k` 默认 60。
//! 只在「出现在某通道 Top-N」的素材上累加：某通道没命中就是 0 贡献，不是 0/rank。
//!
//! 平局由调用方传入的排序键决定（`captured_at DESC` → `rowid DESC`），
//! 保证同分结果的顺序**可回放**（两次调用必须一致）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 默认 `k`（Cormack 2009 的经验值）。
pub const DEFAULT_RRF_K: u32 = 60;
/// 默认通道权重：`[fts, semantic, facet]`。
///
/// 软命中（facet）通道只带「有条目但没被字面/语义召回」的素材，权重刻意小于 1.0：
/// 它是加分项，不该压过真正的字面命中。
pub const DEFAULT_WEIGHTS: &[f32] = &[1.0, 1.0, 0.6];
/// 软命中通道的权重（旧设置补齐用）。
pub const FACET_WEIGHT: f32 = 0.6;

/// RRF 参数（`settings['search.rrf']`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RrfConfig {
    pub k: u32,
    /// `[fts, semantic, facet]`，权重 0 = 关闭该通道。
    ///
    /// 用 `Vec` 而不是定长数组：存量设置是 2 元素数组（facet 通道引入前写入），
    /// 必须能反序列化成功再补齐，而不是整条设置被当成非法值丢掉。
    pub weights: Vec<f32>,
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self {
            k: DEFAULT_RRF_K,
            weights: DEFAULT_WEIGHTS.to_vec(),
        }
    }
}

impl RrfConfig {
    /// 从 `settings['search.rrf']` 读（缺失/非法 → 默认值）。
    ///
    /// 旧记录（`{"k":60,"weights":[1.0,1.0]}`）缺第三路权重：补 [`FACET_WEIGHT`]，
    /// 不能沿用 `unwrap_or(1.0)`——那会让软命中通道和字面通道等权（见 R7）。
    pub fn from_settings(library: &visrecall_library::Library) -> Self {
        match library.setting_get::<RrfConfig>(visrecall_domain::search::RRF_SETTINGS_KEY) {
            Some(mut config) if config.k > 0 => {
                if config.weights.len() < DEFAULT_WEIGHTS.len() {
                    config.weights.resize(DEFAULT_WEIGHTS.len(), FACET_WEIGHT);
                }
                config
            }
            _ => Self::default(),
        }
    }
}

/// 一条通道结果：`(rowid, 原始分数)`，**必须已按相关度降序**（rank 由位置决定）。
pub type Channel<'a> = &'a [(i64, f32)];

/// 融合多路召回。
///
/// `tiebreak(rowid) -> (captured_at, rowid)` 用于同分排序（越大越靠前）。
pub fn rrf(
    channels: &[Channel<'_>],
    config: RrfConfig,
    tiebreak: &dyn Fn(i64) -> (i64, i64),
) -> Vec<(i64, f32)> {
    let k = config.k.max(1);
    let mut scores: HashMap<i64, f32> = HashMap::new();

    for (channel_index, channel) in channels.iter().enumerate() {
        let weight = config.weights.get(channel_index).copied().unwrap_or(1.0);
        if weight <= 0.0 {
            continue;
        }
        for (position, (rowid, _score)) in channel.iter().enumerate() {
            // rank 从 1 起
            let rank = position + 1;
            #[allow(clippy::cast_precision_loss)]
            let contribution = weight / (f64::from(k) + rank as f64) as f32;
            *scores.entry(*rowid).or_insert(0.0) += contribution;
        }
    }

    let mut fused: Vec<(i64, f32)> = scores.into_iter().collect();
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let (a_time, a_row) = tiebreak(a.0);
                let (b_time, b_row) = tiebreak(b.0);
                b_time.cmp(&a_time).then_with(|| b_row.cmp(&a_row))
            })
    });
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tie_by_rowid(rowid: i64) -> (i64, i64) {
        (0, rowid)
    }

    #[test]
    fn intersection_gets_both_contributions() {
        let fts = vec![(1, 5.0), (2, 3.0)];
        let semantic = vec![(2, 0.9), (3, 0.5)];
        let fused = rrf(&[&fts, &semantic], RrfConfig::default(), &tie_by_rowid);
        // rowid 2 出现在两通道（rank2 + rank1）
        assert_eq!(fused[0].0, 2);
        let expected = 1.0 / 62.0 + 1.0 / 61.0;
        assert!((fused[0].1 - expected).abs() < 1e-6);
        // 其余并列：rowid 大的在前（tiebreak rowid DESC）
        assert_eq!(fused.len(), 3);
    }

    #[test]
    fn single_channel_keeps_original_order() {
        let fts = vec![(10, 9.0), (11, 8.0), (12, 7.0)];
        let fused = rrf(&[&fts], RrfConfig::default(), &tie_by_rowid);
        assert_eq!(
            fused.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![10, 11, 12]
        );
    }

    #[test]
    fn zero_weight_disables_a_channel() {
        let fts = vec![(1, 1.0)];
        let semantic = vec![(2, 1.0), (3, 1.0)];
        let config = RrfConfig {
            k: 60,
            weights: vec![1.0, 0.0],
        };
        let fused = rrf(&[&fts, &semantic], config, &tie_by_rowid);
        assert_eq!(fused.len(), 1, "权重 0 的通道不应贡献任何素材");
        assert_eq!(fused[0].0, 1);

        let both_zero = RrfConfig {
            k: 60,
            weights: vec![0.0, 0.0],
        };
        assert!(rrf(&[&fts, &semantic], both_zero, &tie_by_rowid).is_empty());
    }

    #[test]
    fn facet_channel_uses_the_third_weight() {
        let fts = vec![(1, 1.0)];
        let semantic: Vec<(i64, f32)> = Vec::new();
        let facet = vec![(2, 0.0)];
        let fused = rrf(
            &[&fts, &semantic, &facet],
            RrfConfig::default(),
            &tie_by_rowid,
        );
        assert_eq!(fused.len(), 2);
        // 字面命中（权重 1.0）排在软命中的素材（权重 0.6）前面
        assert_eq!(fused[0].0, 1);
        let expected_facet = 0.6 / 61.0;
        assert!((fused[1].1 - expected_facet).abs() < 1e-6);
    }

    #[test]
    fn k_boundary_is_clamped_to_one() {
        let channel = vec![(1, 1.0)];
        let config = RrfConfig {
            k: 0,
            weights: vec![1.0, 1.0],
        };
        let fused = rrf(&[&channel], config, &tie_by_rowid);
        assert!((fused[0].1 - 0.5).abs() < 1e-6, "k 最小按 1 计算");
    }

    #[test]
    fn empty_channels_yield_empty_result() {
        let empty: Vec<(i64, f32)> = Vec::new();
        assert!(rrf(&[&empty, &empty], RrfConfig::default(), &tie_by_rowid).is_empty());
        assert!(rrf(&[], RrfConfig::default(), &tie_by_rowid).is_empty());
    }

    #[test]
    fn ties_are_broken_by_captured_at_then_rowid() {
        let channel = [(1, 0.0), (2, 0.0)];
        // 两条通道各自只有一条，分数相同 → 按 captured_at DESC
        let fused = rrf(
            &[&channel[..1], &channel[1..]],
            RrfConfig::default(),
            &|rowid| match rowid {
                1 => (100, 1),
                _ => (200, 2),
            },
        );
        assert_eq!(fused[0].0, 2, "captured_at 更大的在前");

        let fused_same_time = rrf(
            &[&channel[..1], &channel[1..]],
            RrfConfig::default(),
            &|rowid| (100, rowid),
        );
        assert_eq!(fused_same_time[0].0, 2, "同 captured_at 时 rowid 大的在前");
    }

    #[test]
    fn result_is_deterministic() {
        let fts = vec![(1, 1.0), (2, 1.0), (3, 1.0)];
        let semantic = vec![(3, 1.0), (1, 1.0)];
        let first = rrf(&[&fts, &semantic], RrfConfig::default(), &tie_by_rowid);
        let second = rrf(&[&fts, &semantic], RrfConfig::default(), &tie_by_rowid);
        assert_eq!(first, second);
    }
}

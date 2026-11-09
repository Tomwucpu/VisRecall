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
/// 默认通道权重：`[fts, semantic]`。
pub const DEFAULT_WEIGHTS: [f32; 2] = [1.0, 1.0];

/// RRF 参数（`settings['search.rrf']`）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RrfConfig {
    pub k: u32,
    /// `[fts, semantic]`，权重 0 = 关闭该通道。
    pub weights: [f32; 2],
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self {
            k: DEFAULT_RRF_K,
            weights: DEFAULT_WEIGHTS,
        }
    }
}

impl RrfConfig {
    /// 从 `settings['search.rrf']` 读（缺失/非法 → 默认值）。
    pub fn from_settings(library: &visrecall_library::Library) -> Self {
        match library.setting_get::<RrfConfig>(visrecall_domain::search::RRF_SETTINGS_KEY) {
            Some(config) if config.k > 0 => config,
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
            weights: [1.0, 0.0],
        };
        let fused = rrf(&[&fts, &semantic], config, &tie_by_rowid);
        assert_eq!(fused.len(), 1, "权重 0 的通道不应贡献任何素材");
        assert_eq!(fused[0].0, 1);

        let both_zero = RrfConfig {
            k: 60,
            weights: [0.0, 0.0],
        };
        assert!(rrf(&[&fts, &semantic], both_zero, &tie_by_rowid).is_empty());
    }

    #[test]
    fn k_boundary_is_clamped_to_one() {
        let channel = vec![(1, 1.0)];
        let config = RrfConfig {
            k: 0,
            weights: [1.0, 1.0],
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

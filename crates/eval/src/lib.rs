//! 检索评测：Top-3 命中率、Recall@10、mAP、P95 延迟与通道分布（requirements P3-R14）。
//!
//! 设计边界：
//! - 只测**检索**，不重跑 VLM/OCR（评测库是已经索引好的真实库）；
//! - 嵌入用真实模型（与线上同一 `EmbeddingProvider`），所以延迟数字可比；
//! - 每条查询都会写一行 `queries`（`execute` 的副作用），因此**评测库要与个人库分开**。
//!
//! 指标口径（写进报告，避免「数字好看但定义含糊」）：
//! - `Top-3 命中率`：前 3 条里至少有 1 条人工标注的相关素材；
//! - `Recall@10`：相关素材被前 10 条召回的比例（每条查询取平均）；
//! - `mAP`：对每条查询算 AP（命中相关的位次上的 precision 均值 / 相关总数），再取平均；
//! - `P95 took_ms`：`search::execute` 的端到端耗时（含嵌入与 SQL），按 95 分位。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use visrecall_domain::providers::{EmbeddingProvider, VectorIndex};
use visrecall_domain::AppError;
use visrecall_library::Library;
use visrecall_search::execute::{search, SearchDeps, SearchRequest};
use visrecall_search::rank::RrfConfig;

/// 一条评测查询。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalQuery {
    pub query: String,
    /// 人工标注的相关素材 id（空数组表示「预期无结果」）。
    #[serde(default)]
    pub relevant: Vec<String>,
    /// `screenshot` / `scene` / `literal`。
    #[serde(default)]
    pub bucket: String,
}

/// 单条查询的执行结果。
#[derive(Debug, Clone, PartialEq)]
pub struct QueryOutcome {
    pub query: String,
    pub bucket: String,
    pub took_ms: u64,
    pub semantic_used: bool,
    /// Top-10 的素材 id（顺序即排序）。
    pub top: Vec<String>,
    /// 这条查询的解释通道分布：`fts` / `filter` / `semantic`。
    pub channels: Vec<&'static str>,
    pub top3_hit: bool,
    pub recall_at_10: f64,
    pub ap: f64,
}

/// 汇总指标。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Metrics {
    pub queries: usize,
    pub top3_hit_rate: f64,
    pub recall_at_10: f64,
    pub map: f64,
    pub p95_ms: u64,
    pub mean_ms: f64,
    /// 通道分布计数（按命中条数）。
    pub channel_counts: BTreeMap<String, usize>,
    /// 语义通道被调用的查询数。
    pub semantic_queries: usize,
}

impl Metrics {
    /// 门禁（requirements P3-R14：Top-3 ≥80%）。
    pub fn passes_gate(&self) -> bool {
        self.top3_hit_rate >= 0.80
    }
}

/// 读取 JSONL 评测集（`#` 开头的行为注释，空行跳过）。
pub fn load_queries(path: &Path) -> Result<Vec<EvalQuery>, AppError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|_| AppError::FileMissing(path.display().to_string()))?;
    let mut out = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let query: EvalQuery = serde_json::from_str(trimmed).map_err(|err| {
            AppError::InvalidInput(format!("{}:{}: {err}", path.display(), index + 1))
        })?;
        if query.query.trim().is_empty() {
            return Err(AppError::InvalidInput(format!(
                "{}:{}: empty query",
                path.display(),
                index + 1
            )));
        }
        out.push(query);
    }
    Ok(out)
}

/// 跑一遍评测集。
pub fn evaluate(
    library: &Library,
    vectors: &dyn VectorIndex,
    embed: &dyn EmbeddingProvider,
    runtime: &tokio::runtime::Handle,
    queries: &[EvalQuery],
    limit: u32,
) -> Result<Vec<QueryOutcome>, AppError> {
    let rrf = RrfConfig::from_settings(library);
    let mut outcomes = Vec::with_capacity(queries.len());
    for query in queries {
        let result = search(
            SearchDeps {
                library,
                vectors,
                embed: Some(embed),
                runtime: Some(runtime),
                rrf: rrf.clone(),
            },
            SearchRequest::new(query.query.clone(), limit),
        )?;

        let top: Vec<String> = result.hits.iter().map(|hit| hit.item.id.clone()).collect();
        let channels: Vec<&'static str> = result
            .hits
            .iter()
            .map(|hit| {
                if hit.explanation.semantic {
                    "semantic"
                } else if !hit.explanation.terms.is_empty() {
                    "fts"
                } else {
                    "filter"
                }
            })
            .collect();

        let relevant: std::collections::HashSet<&String> = query.relevant.iter().collect();
        let hits: Vec<bool> = top
            .iter()
            .take(10)
            .map(|id| relevant.contains(id))
            .collect();
        // 「预期无结果」的查询：正确地什么都不返回才算命中（否则它只贡献噪声）
        let top3_hit = if relevant.is_empty() {
            top.is_empty()
        } else {
            hits.iter().take(3).any(|hit| *hit)
        };
        let recall_at_10 = if relevant.is_empty() {
            // 「预期无结果」的查询：召回无意义，按「没有误召回」计 1
            if top.is_empty() {
                1.0
            } else {
                0.0
            }
        } else {
            let found = hits.iter().filter(|hit| **hit).count();
            #[allow(clippy::cast_precision_loss)]
            {
                found as f64 / relevant.len() as f64
            }
        };
        let ap = average_precision(&hits, relevant.len());

        outcomes.push(QueryOutcome {
            query: query.query.clone(),
            bucket: if query.bucket.is_empty() {
                "unspecified".to_string()
            } else {
                query.bucket.clone()
            },
            took_ms: result.took_ms,
            semantic_used: result.semantic_used,
            top,
            channels,
            top3_hit,
            recall_at_10,
            ap,
        });
    }
    Ok(outcomes)
}

/// AP：命中位次上的 precision 之和 / 相关总数（`hits` 是 Top-10 的命中标记）。
fn average_precision(hits: &[bool], relevant_total: usize) -> f64 {
    if relevant_total == 0 {
        return if hits.iter().any(|hit| *hit) {
            0.0
        } else {
            1.0
        };
    }
    let mut sum = 0.0;
    let mut found = 0usize;
    for (index, hit) in hits.iter().enumerate() {
        if !hit {
            continue;
        }
        found += 1;
        #[allow(clippy::cast_precision_loss)]
        {
            sum += found as f64 / (index + 1) as f64;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    {
        sum / relevant_total as f64
    }
}

/// 汇总（全部查询）。
pub fn summarize(outcomes: &[QueryOutcome]) -> Metrics {
    if outcomes.is_empty() {
        return Metrics::default();
    }
    let mut latencies: Vec<u64> = outcomes.iter().map(|outcome| outcome.took_ms).collect();
    latencies.sort_unstable();

    let mut channel_counts: BTreeMap<String, usize> = BTreeMap::new();
    for outcome in outcomes {
        for channel in &outcome.channels {
            *channel_counts.entry((*channel).to_string()).or_insert(0) += 1;
        }
    }

    #[allow(clippy::cast_precision_loss)]
    let n = outcomes.len() as f64;
    Metrics {
        queries: outcomes.len(),
        top3_hit_rate: outcomes.iter().filter(|o| o.top3_hit).count() as f64 / n,
        recall_at_10: outcomes.iter().map(|o| o.recall_at_10).sum::<f64>() / n,
        map: outcomes.iter().map(|o| o.ap).sum::<f64>() / n,
        p95_ms: percentile(&latencies, 0.95),
        mean_ms: outcomes.iter().map(|o| o.took_ms as f64).sum::<f64>() / n,
        channel_counts,
        semantic_queries: outcomes.iter().filter(|o| o.semantic_used).count(),
    }
}

/// 分桶汇总（报告里的分桶表）。
pub fn summarize_by_bucket(outcomes: &[QueryOutcome]) -> BTreeMap<String, Metrics> {
    let mut buckets: BTreeMap<String, Vec<QueryOutcome>> = BTreeMap::new();
    for outcome in outcomes {
        buckets
            .entry(outcome.bucket.clone())
            .or_default()
            .push(outcome.clone());
    }
    buckets
        .into_iter()
        .map(|(bucket, list)| (bucket, summarize(&list)))
        .collect()
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let index = ((sorted.len() as f64 * p).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[index]
}

/// 报告渲染所需的上下文。
#[derive(Debug, Clone, Default)]
pub struct ReportContext {
    pub database: String,
    pub item_count: u64,
    pub model_id: String,
    pub model_dim: usize,
    pub rrf: String,
    pub queries_file: String,
}

/// 生成 Markdown 报告。
pub fn render_report(
    context: &ReportContext,
    metrics: &Metrics,
    by_bucket: &BTreeMap<String, Metrics>,
    outcomes: &[QueryOutcome],
    title: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {title}\n\n"));
    out.push_str("## 环境\n\n");
    out.push_str(&format!(
        "- 评测库：`{}`（素材 {} 条）\n",
        context.database, context.item_count
    ));
    out.push_str(&format!(
        "- 嵌入模型：`{}`（{} 维）\n",
        context.model_id, context.model_dim
    ));
    out.push_str(&format!("- RRF：{}\n", context.rrf));
    out.push_str(&format!("- 评测集：`{}`\n", context.queries_file));
    out.push_str(&format!(
        "- 平台：{} {}\n\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));

    out.push_str("## 总览\n\n");
    out.push_str("| 指标 | 值 | 门禁 |\n|---|---|---|\n");
    out.push_str(&format!("| 查询数 | {} | — |\n", metrics.queries));
    out.push_str(&format!(
        "| Top-3 命中率 | {:.1}% | ≥80% {} |\n",
        metrics.top3_hit_rate * 100.0,
        if metrics.passes_gate() {
            "通过"
        } else {
            "未达标"
        }
    ));
    out.push_str(&format!(
        "| Recall@10 | {:.3} | — |\n",
        metrics.recall_at_10
    ));
    out.push_str(&format!("| mAP | {:.3} | — |\n", metrics.map));
    out.push_str(&format!(
        "| 延迟 P95 | {} ms | 目标 ≤200ms |\n",
        metrics.p95_ms
    ));
    out.push_str(&format!("| 延迟均值 | {:.1} ms | — |\n", metrics.mean_ms));
    out.push_str(&format!(
        "| 语义通道启用 | {}/{} 条查询 | — |\n\n",
        metrics.semantic_queries, metrics.queries
    ));

    out.push_str("## 分桶\n\n");
    out.push_str(
        "| 桶 | 查询数 | Top-3 | Recall@10 | mAP | P95(ms) |\n|---|---|---|---|---|---|\n",
    );
    for (bucket, bucket_metrics) in by_bucket {
        out.push_str(&format!(
            "| {bucket} | {} | {:.1}% | {:.3} | {:.3} | {} |\n",
            bucket_metrics.queries,
            bucket_metrics.top3_hit_rate * 100.0,
            bucket_metrics.recall_at_10,
            bucket_metrics.map,
            bucket_metrics.p95_ms
        ));
    }

    out.push_str("\n## 通道分布（按命中条数）\n\n");
    out.push_str("| 通道 | 命中条数 |\n|---|---|\n");
    for (channel, count) in &metrics.channel_counts {
        out.push_str(&format!("| {channel} | {count} |\n"));
    }

    out.push_str("\n## Top-3 未命中的查询\n\n");
    let misses: Vec<&QueryOutcome> = outcomes.iter().filter(|o| !o.top3_hit).collect();
    if misses.is_empty() {
        out.push_str("无。\n");
    } else {
        out.push_str("| 查询 | 桶 | Top-3 |\n|---|---|---|\n");
        for miss in misses {
            let top3: Vec<&str> = miss.top.iter().take(3).map(String::as_str).collect();
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                miss.query,
                miss.bucket,
                top3.join(", ")
            ));
        }
    }

    out.push_str("\n## 调参回路\n\n");
    out.push_str(
        "首跑作为基线。若 Top-3 <80%，按以下顺序尝试并各出一份对比报告（含 diff 摘要）：\n\
         1. jieba 词典（补领域词，见 `crates/search/src/tokenizer.rs` 的 `DOMAIN_WORDS`）；\n\
         2. RRF 权重（`settings['search.rrf']`）与 `execute::SEMANTIC_RELATIVE_FLOOR`；\n\
         3. 记录「是否需要升级 bge-base-zh」的评审结论（不擅自升级）。\n",
    );
    out
}

/// 一次性跑完所需的一切（CLI 用；测试直接调 `evaluate` + `render_report`）。
pub struct EvaluationRun<'a> {
    pub library: &'a Library,
    pub vectors: &'a dyn VectorIndex,
    pub embed: &'a dyn EmbeddingProvider,
    pub runtime: &'a tokio::runtime::Handle,
    pub queries: &'a [EvalQuery],
    pub limit: u32,
    pub context: ReportContext,
    pub title: String,
}

/// 便捷入口：一次跑完并返回报告文本与指标。
pub fn run_evaluation(run: EvaluationRun<'_>) -> Result<(String, Metrics), AppError> {
    let outcomes = evaluate(
        run.library,
        run.vectors,
        run.embed,
        run.runtime,
        run.queries,
        run.limit,
    )?;
    let metrics = summarize(&outcomes);
    let by_bucket = summarize_by_bucket(&outcomes);
    let report = render_report(&run.context, &metrics, &by_bucket, &outcomes, &run.title);
    Ok((report, metrics))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(
        top3_hit: bool,
        recall: f64,
        ap: f64,
        took: u64,
        channel: &'static str,
    ) -> QueryOutcome {
        QueryOutcome {
            query: "q".into(),
            bucket: "screenshot".into(),
            took_ms: took,
            semantic_used: channel == "semantic",
            top: Vec::new(),
            channels: vec![channel],
            top3_hit,
            recall_at_10: recall,
            ap,
        }
    }

    #[test]
    fn average_precision_counts_positions_not_just_hits() {
        // 相关在第 1、3 位：AP = (1/1 + 2/3) / 2
        let ap = average_precision(&[true, false, true], 2);
        assert!((ap - (1.0 + 2.0 / 3.0) / 2.0).abs() < 1e-9);

        // 全命中
        assert!((average_precision(&[true, true], 2) - 1.0).abs() < 1e-9);
        // 全不命中
        assert!(average_precision(&[false, false], 1).abs() < 1e-9);
        // 「预期无结果」且确实没结果 → 满分；误召回 → 0
        assert!((average_precision(&[], 0) - 1.0).abs() < 1e-9);
        assert!(average_precision(&[true], 0).abs() < 1e-9);
    }

    #[test]
    fn summarize_aggregates_and_computes_p95() {
        let outcomes = vec![
            outcome(true, 1.0, 1.0, 10, "fts"),
            outcome(false, 0.5, 0.3, 20, "semantic"),
            outcome(true, 1.0, 0.8, 30, "fts"),
            outcome(true, 0.0, 0.0, 400, "filter"),
        ];
        let metrics = summarize(&outcomes);
        assert_eq!(metrics.queries, 4);
        assert!((metrics.top3_hit_rate - 0.75).abs() < 1e-9);
        assert!((metrics.recall_at_10 - 0.625).abs() < 1e-9);
        assert_eq!(metrics.p95_ms, 400);
        assert_eq!(metrics.semantic_queries, 1);
        assert_eq!(metrics.channel_counts.get("fts"), Some(&2));
        assert!(!metrics.passes_gate(), "75% 未达 80% 门禁");
    }

    #[test]
    fn empty_outcomes_do_not_divide_by_zero() {
        let metrics = summarize(&[]);
        assert_eq!(metrics.queries, 0);
        assert!(!metrics.passes_gate());
    }

    #[test]
    fn bucket_summaries_split_by_bucket() {
        let mut a = outcome(true, 1.0, 1.0, 5, "fts");
        a.bucket = "literal".into();
        let mut b = outcome(false, 0.0, 0.0, 7, "filter");
        b.bucket = "scene".into();
        let buckets = summarize_by_bucket(&[a, b]);
        assert_eq!(buckets.len(), 2);
        assert!((buckets["literal"].top3_hit_rate - 1.0).abs() < 1e-9);
        assert!((buckets["scene"].top3_hit_rate - 0.0).abs() < 1e-9);
    }

    #[test]
    fn render_report_contains_gate_verdict_and_misses() {
        let outcomes = vec![outcome(false, 0.0, 0.0, 12, "fts")];
        let metrics = summarize(&outcomes);
        let report = render_report(
            &ReportContext {
                database: "D:/eval-lib".into(),
                item_count: 20,
                model_id: "bge-small-zh-v1.5-int8".into(),
                model_dim: 512,
                rrf: "k=60 weights=[1.0, 1.0]".into(),
                queries_file: "queries.jsonl".into(),
            },
            &metrics,
            &summarize_by_bucket(&outcomes),
            &outcomes,
            "测试报告",
        );
        assert!(report.contains("未达标"));
        assert!(report.contains("bge-small-zh-v1.5-int8"));
        assert!(report.contains("Top-3 未命中的查询"));
    }
}

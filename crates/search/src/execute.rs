//! 三路召回编排（design §6.3）：parse → embed → 候选 SQL → FTS → 向量 → 软命中 → RRF
//! → 装配解释。
//!
//! 与 `app-core` 的分工：本模块是**同步纯编排**，嵌入与向量索引都从外部注入
//! （`&dyn EmbeddingProvider` + `&dyn VectorIndex`），因此 `search` 不依赖 `ai`，
//! 也便于用假实现做确定性测试。查询嵌入的「就绪判定 + 降级事件」在 `app-core`。
//!
//! 降级语义：`embed` 为 `None` 或未就绪 → 跳过向量通道，`semantic_used=false`，
//! FTS/过滤器结果照常返回（P3-R09「语义模型未就绪检索仍可用」）。
//!
//! 软命中（颜色 / 图表 / 素材类型 / 公式）走**第三路纯 SQL 通道**：主色与图表类型不在
//! FTS 索引里（`document.rs` 只有 6 个文本字段），塞进索引要全库重建且污染 BM25；
//! RRF 只用名次，因此这一路只需要有序 rowid（[`filters::facet_rowids`]）。

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use visrecall_domain::insight::OcrBox;
use visrecall_domain::providers::{EmbeddingProvider, VectorIndex};
use visrecall_domain::search::{Explanation, ItemFilters, ParsedQuery, ScoredItem};
use visrecall_domain::{now_millis, AppError, Insight, Item};
use visrecall_library::{FtsHit, Library, QueryLog};

use crate::filters::{self, to_sql_with_filters, FacetMode};
use crate::fts_query::build_match_expr;
use crate::parser::parse_query_at;
use crate::rank::{self, RrfConfig};

/// 每路召回的候选上限（design §6.3 的 200）。
pub const CHANNEL_LIMIT: u32 = 200;
/// 返回条数上限。
pub const MAX_LIMIT: u32 = 200;

/// 语义通道的相对截断：低于「最高分 × 该系数」的向量命中不进入融合。
///
/// 为什么需要它：`vector_index.search` 在没有过滤时会返回**全库**（20 条规模下
/// `top_k=200` 等于全部），RRF 按名次给分就会把整个库都塞进结果列表——语义检索看起来
/// 「什么都搜得到」。绝对阈值对 int8 量化与不同模型都不可移植，因此用相对阈值：
/// 只有与最佳命中同一量级的素材才算「语义相关」。
/// 该系数属 V0.1 经验值，由 `crates/eval` 的 20 条评测集调（P3-R14）。
pub const SEMANTIC_RELATIVE_FLOOR: f32 = 0.5;

/// 检索请求。
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub raw: String,
    pub limit: u32,
    /// 界面筛选条的结构化条件；与自然语言解析结果取交集。
    pub filters: ItemFilters,
}

impl SearchRequest {
    pub fn new(raw: impl Into<String>, limit: u32) -> Self {
        Self {
            raw: raw.into(),
            limit,
            filters: ItemFilters::default(),
        }
    }

    /// 附加界面筛选条件（默认空，便于既有调用点不改）。
    pub fn with_filters(mut self, filters: ItemFilters) -> Self {
        self.filters = filters;
        self
    }
}

/// 检索结果（`search_items` IPC 的后端载荷）。
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    /// 写入 `queries.id`，用于「不是这个」反馈。
    pub query_id: String,
    pub parsed: ParsedQuery,
    pub semantic_used: bool,
    pub took_ms: u64,
    pub hits: Vec<ScoredItem>,
    /// `limit` 截断前的命中数。
    pub total: usize,
}

/// 编排依赖（注入而非构造，测试可完全替换）。
pub struct SearchDeps<'a> {
    pub library: &'a Library,
    pub vectors: &'a dyn VectorIndex,
    pub embed: Option<&'a dyn EmbeddingProvider>,
    pub runtime: Option<&'a tokio::runtime::Handle>,
    pub rrf: RrfConfig,
}

/// 执行一次检索（同步）。
///
/// 软条件默认是「加分项」；只有孤词（[`ParsedQuery::facets_only`]）才退回硬过滤，
/// 且硬过滤 0 命中时自动放宽重跑一次（D3-Q07）。
pub fn search(deps: SearchDeps<'_>, request: SearchRequest) -> Result<SearchOutcome, AppError> {
    let started = Instant::now();
    let now = now_millis();
    let mut parsed = parse_query_at(&request.raw, now);
    let limit = request.limit.clamp(1, MAX_LIMIT) as usize;

    let facet_mode = FacetMode::for_query(&parsed);
    let mut attempt = run_once(&deps, &parsed, &request.filters, facet_mode, limit, false)?;

    // 孤词硬过滤 0 命中 → 清空软条件重跑（保留界面筛选与 `app:X`）
    if facet_mode == FacetMode::Hard && attempt.hits.is_empty() && parsed.has_facets() {
        parsed.color = None;
        parsed.chart = None;
        parsed.content_kind = None;
        parsed.has_formula = None;
        parsed.facet_words.clear();
        attempt = run_once(
            &deps,
            &parsed,
            &request.filters,
            FacetMode::Soft,
            limit,
            true,
        )?;
    }

    // 记录查询（评测与反馈闭环；放宽后的条件才是真正生效的条件）
    let query_id = visrecall_domain::new_item_id();
    let parsed_json = serde_json::to_string(&parsed).ok();
    let result_ids: Vec<&str> = attempt
        .hits
        .iter()
        .map(|hit| hit.item.id.as_str())
        .collect();
    let result_ids_json = serde_json::to_string(&result_ids).ok();
    if let Err(err) = deps.library.query_log_insert(&QueryLog {
        id: query_id.clone(),
        raw: request.raw.clone(),
        parsed_json,
        result_ids_json,
        feedback: None,
        created_at: now,
    }) {
        // 记录失败不该让检索失败（评测数据，不是用户可见结果）
        tracing::warn!(%err, "failed to write query log");
    }

    let took_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(SearchOutcome {
        query_id,
        parsed,
        semantic_used: attempt.semantic_used,
        took_ms,
        total: attempt.total,
        hits: attempt.hits,
    })
}

/// 单次检索的内部产物（放宽重跑时会被替换）。
struct Attempt {
    hits: Vec<ScoredItem>,
    /// `limit` 截断前的命中数。
    total: usize,
    semantic_used: bool,
}

/// 跑一遍完整的三路召回。`relaxed` 只影响解释文案（「条件已放宽」）。
fn run_once(
    deps: &SearchDeps<'_>,
    parsed: &ParsedQuery,
    filters_in: &ItemFilters,
    facet_mode: FacetMode,
    limit: usize,
    relaxed: bool,
) -> Result<Attempt, AppError> {
    // 1) 查询向量（未就绪/未注入 → None，语义通道跳过）
    let query_vector = query_embedding(deps, parsed);
    let semantic_used = query_vector.is_some();

    // 2) 候选集（无过滤器 = 全集，`None`）：自然语言条件与界面筛选合并成同一批 rowid
    let candidates = candidate_rowids(deps.library, parsed, filters_in, facet_mode)?;

    // 3) FTS 路
    let expr = build_match_expr(&parsed.lexical_terms, &parsed.quoted_terms);
    let fts_hits: Vec<FtsHit> = if expr.is_empty() {
        Vec::new()
    } else {
        deps.library
            .fts_search(&expr, candidates.as_deref(), CHANNEL_LIMIT)?
    };
    let fts_channel: Vec<(i64, f32)> = fts_hits.iter().map(|hit| (hit.rowid, hit.score)).collect();
    let fts_rowids: HashSet<i64> = fts_channel.iter().map(|(rowid, _)| *rowid).collect();

    // 4) 向量路（输出已按相似度降序）
    let vec_channel: Vec<(i64, f32)> = match &query_vector {
        Some(vector) => trim_semantic_channel(deps.vectors.search(
            vector,
            CHANNEL_LIMIT as usize,
            candidates.as_deref(),
        )?),
        None => Vec::new(),
    };
    let vec_rowids: HashSet<i64> = vec_channel.iter().map(|(rowid, _)| *rowid).collect();

    // 5) 软命中路（只在 Soft 模式；Hard 模式已进 WHERE，不重复计分）
    let facet_channel: Vec<(i64, f32)> = if facet_mode == FacetMode::Soft && parsed.has_facets() {
        filters::facet_rowids(deps.library, parsed, CHANNEL_LIMIT)?
            .into_iter()
            .map(|rowid| (rowid, 0.0))
            .collect()
    } else {
        Vec::new()
    };
    let facet_rowids: HashSet<i64> = facet_channel.iter().map(|(rowid, _)| *rowid).collect();

    // 6) 取回素材（平局键、装配与过滤都要用）
    let mut rowids: Vec<i64> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    for rowid in fts_rowids
        .iter()
        .chain(vec_rowids.iter())
        .chain(facet_rowids.iter())
    {
        if seen.insert(*rowid) {
            rowids.push(*rowid);
        }
    }
    if let Some(all) = &candidates {
        for rowid in all {
            if seen.insert(*rowid) {
                rowids.push(*rowid);
            }
        }
    }
    let items = deps.library.items_by_rowids(&rowids)?;
    let by_rowid: HashMap<i64, Item> = items.into_iter().collect();
    let item_ids: Vec<String> = by_rowid.values().map(|item| item.id.clone()).collect();
    let insights = deps.library.insights_for(&item_ids)?;

    let tie_break = |rowid: i64| -> (i64, i64) {
        by_rowid
            .get(&rowid)
            .map(|item| (item.captured_at, rowid))
            .unwrap_or((0, rowid))
    };

    // 7) 融合 + filters-only 尾部（score = 0，按 captured_at DESC）
    let mut fused = rank::rrf(
        &[&fts_channel[..], &vec_channel[..], &facet_channel[..]],
        deps.rrf.clone(),
        &tie_break,
    );
    let mut only_filters: Vec<(i64, f32)> = candidates
        .as_ref()
        .map(|all| {
            all.iter()
                .filter(|rowid| {
                    !fts_rowids.contains(rowid)
                        && !vec_rowids.contains(rowid)
                        && !facet_rowids.contains(rowid)
                })
                .map(|rowid| (*rowid, 0.0))
                .collect()
        })
        .unwrap_or_default();
    only_filters.sort_by(|a, b| {
        let (a_time, a_row) = tie_break(a.0);
        let (b_time, b_row) = tie_break(b.0);
        b_time.cmp(&a_time).then_with(|| b_row.cmp(&a_row))
    });
    fused.extend(only_filters);

    // 8) 装配解释 + 内存侧过滤（failed 素材不过滤，与列表口径一致）
    let snippet_map = snippet_terms(&fts_hits);
    let mut hits: Vec<ScoredItem> = Vec::with_capacity(fused.len());
    for (rowid, score) in fused {
        let Some(item) = by_rowid.get(&rowid) else {
            continue;
        };
        let insight = insights.get(&item.id);
        if !filters::passes_with_filters(item, insight, parsed, filters_in) {
            continue;
        }
        let terms = matched_terms(item, insight, parsed, snippet_map.get(&rowid));
        let mut facets = filters::matched_facets(item, insight, parsed);
        if relaxed {
            facets.push("条件已放宽".to_string());
        }
        let explanation = Explanation {
            filters: filters::matched_labels_with_filters(item, insight, parsed, filters_in),
            facets,
            terms: terms.clone(),
            // 仅语义命中：非 FTS + 无硬条件 + 无软命中 + 无界面筛选
            semantic: !fts_rowids.contains(&rowid)
                && !parsed.has_hard_filters()
                && !parsed.has_facets()
                && filters_in.is_empty(),
            highlight_boxes: highlight_boxes(insight, &terms),
        };
        hits.push(ScoredItem {
            item: item.clone(),
            score,
            explanation,
        });
    }

    let total = hits.len();
    hits.truncate(limit);
    Ok(Attempt {
        hits,
        total,
        semantic_used,
    })
}

/// 语义通道截断：丢掉非正相似度，再按「最高分 × [`SEMANTIC_RELATIVE_FLOOR`]」过滤。
fn trim_semantic_channel(mut channel: Vec<(i64, f32)>) -> Vec<(i64, f32)> {
    channel.retain(|(_, score)| *score > 0.0);
    let best = channel.first().map(|(_, score)| *score).unwrap_or(0.0);
    let floor = best * SEMANTIC_RELATIVE_FLOOR;
    channel.retain(|(_, score)| *score >= floor);
    channel
}

fn query_embedding(deps: &SearchDeps<'_>, parsed: &ParsedQuery) -> Option<Vec<f32>> {
    let embed = deps.embed?;
    let runtime = deps.runtime?;
    if !embed.ready() {
        return None;
    }
    let text = parsed.semantic_text.trim();
    if text.is_empty() {
        // 没语义文本就不必花 30–80ms（纯过滤查询）
        return None;
    }
    match runtime.block_on(embed.embed(&[text.to_string()])) {
        Ok(mut vectors) => vectors.pop(),
        Err(err) => {
            tracing::warn!(%err, "query embedding failed; semantic channel skipped");
            None
        }
    }
}

fn candidate_rowids(
    library: &Library,
    parsed: &ParsedQuery,
    extra: &ItemFilters,
    facet_mode: FacetMode,
) -> Result<Option<Vec<i64>>, AppError> {
    let filter = to_sql_with_filters(parsed, extra, facet_mode)?;
    if filter.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        library.candidate_rowids(&filter.clause, &filter.params)?,
    ))
}

/// 从 FTS snippet 的 `«»` 标记里取实际命中的词。
fn snippet_terms(hits: &[FtsHit]) -> HashMap<i64, Vec<String>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    for hit in hits {
        let mut terms: Vec<String> = Vec::new();
        let mut rest = hit.snippet.as_str();
        while let Some(start) = rest.find('\u{ab}') {
            let after = &rest[start + '\u{ab}'.len_utf8()..];
            let Some(end) = after.find('\u{bb}') else {
                break;
            };
            let term = after[..end].trim();
            if !term.is_empty() && !terms.iter().any(|existing| existing == term) {
                terms.push(term.to_string());
            }
            rest = &after[end + '\u{bb}'.len_utf8()..];
        }
        out.insert(hit.rowid, terms);
    }
    out
}

/// 命中词 = 查询词 ∩ 素材文本（FTS snippet 优先，缺失时退回文本包含判断）。
fn matched_terms(
    item: &Item,
    insight: Option<&Insight>,
    parsed: &ParsedQuery,
    snippet: Option<&Vec<String>>,
) -> Vec<String> {
    if let Some(terms) = snippet.filter(|terms| !terms.is_empty()) {
        return terms.clone();
    }
    let haystack = format!(
        "{} {} {} {}",
        item.display_title().unwrap_or(""),
        item.note,
        insight
            .and_then(|value| value.summary.as_deref())
            .unwrap_or(""),
        insight
            .and_then(|value| value.ocr_text.as_deref())
            .unwrap_or(""),
    );
    parsed
        .lexical_terms
        .iter()
        .chain(parsed.quoted_terms.iter())
        .filter(|term| haystack.contains(term.as_str()))
        .cloned()
        .collect()
}

/// OCR 框高亮：命中词与框文本的包含匹配（失败退化空 → 前端只做文字高亮）。
fn highlight_boxes(insight: Option<&Insight>, terms: &[String]) -> Vec<OcrBox> {
    let Some(insight) = insight else {
        return Vec::new();
    };
    insight
        .ocr_boxes
        .iter()
        .filter(|obox| terms.iter().any(|term| obox.contains(term)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_terms_extracts_marked_terms() {
        let hits = vec![FtsHit {
            rowid: 1,
            snippet: "季度营收«漏斗图»…转化率".to_string(),
            score: 1.0,
        }];
        let map = snippet_terms(&hits);
        assert_eq!(map.get(&1).unwrap(), &vec!["漏斗图".to_string()]);
    }

    #[test]
    fn snippet_terms_handles_multiple_and_missing_marks() {
        let hits = vec![
            FtsHit {
                rowid: 1,
                snippet: "«甲»与«乙»".to_string(),
                score: 1.0,
            },
            FtsHit {
                rowid: 2,
                snippet: "没有标记".to_string(),
                score: 1.0,
            },
        ];
        let map = snippet_terms(&hits);
        assert_eq!(map.get(&1).unwrap().len(), 2);
        assert!(map.get(&2).unwrap().is_empty());
    }

    #[test]
    fn highlight_boxes_matches_only_containing_terms() {
        let insight = Insight {
            ocr_boxes: vec![
                OcrBox::new("发票号码", 0.0, 0.0, 0.1, 0.1),
                OcrBox::new("金额", 0.0, 0.2, 0.1, 0.1),
            ],
            ..Insight::empty("x", 0)
        };
        let boxes = highlight_boxes(Some(&insight), &["发票".to_string()]);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].text, "发票号码");
        assert!(highlight_boxes(None, &["发票".to_string()]).is_empty());
    }
}

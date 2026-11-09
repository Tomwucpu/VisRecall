/**
 * 检索相关的类型别名：组件只从这里取类型，避免各处重复写 `*_Serialize` 后缀。
 */
import type {
  ExplanationDto,
  ItemDto,
  OcrBoxDto,
  ParsedQueryDto,
  ProbeOutcomeDto,
  SearchHitDto,
  SearchResponseDto,
} from "./bindings";

export type { ExplanationDto, OcrBoxDto, ParsedQueryDto, ProbeOutcomeDto };

/** 一条命中：素材 DTO + 分数 + 解释。 */
export type SearchHit = SearchHitDto;

/** 检索响应（`queryId` 用于「不是这个」反馈）。 */
export type SearchResponse = SearchResponseDto;

/** 检索状态：`idle` 空查询、`ready` 有结果、`error` 失败。 */
export type SearchStatus = "idle" | "ready" | "error";

/** 命中来源（ExplainBar 文案与卡片徽标共用）。 */
export type HitSource = "fts" | "semantic" | "filter";

/** 命中来源：语义 > 字面 > 仅过滤。 */
export function hitSource(explanation: ExplanationDto): HitSource {
  if (explanation.semantic) {
    return "semantic";
  }
  if (explanation.terms.length > 0) {
    return "fts";
  }
  return "filter";
}

/** 命中来源的中文短标签（卡片徽标）。 */
export function hitSourceLabel(source: HitSource): string {
  switch (source) {
    case "semantic":
      return "语义命中";
    case "fts":
      return "字面命中";
    default:
      return "条件命中";
  }
}

/** 命中来源的长文案（ExplainBar 的一句话解释）。 */
export function hitSourceSentence(source: HitSource, terms: string[]): string {
  switch (source) {
    case "semantic":
      return "按语义相关性匹配";
    case "fts":
      return terms.length > 0 ? `命中文字：${terms.join("、")}` : "命中文字";
    default:
      return "仅满足筛选条件";
  }
}

export type { ItemDto };

/**
 * 把 chip 对应的字样从查询串里删掉（保留其余词与空白规整）。
 *
 * chip 文本是展示用的中文（「橙色」「来自 chrome」），与原文可能不完全一致，
 * 所以按「chip 内容里出现过的原词」逐个尝试删除；一个都删不掉时原样返回，
 * 由调用方决定是否重搜（避免一个失误的删除把整条查询清空）。
 */
export function stripChip(raw: string, chip: string): string {
  const text = chip.trim();
  if (text.length === 0) {
    return raw;
  }

  // chip 是展示文案，尝试几种「原文里可能的写法」：整体、去前缀（来自/#）、带空格
  const candidates = new Set<string>([text]);
  if (text.startsWith("来自 ")) {
    const app = text.slice(3).trim();
    candidates.add(app);
    candidates.add(`来自${app}`);
  }
  if (text.startsWith("#")) {
    candidates.add(text.slice(1));
    candidates.add(`tag:${text.slice(1)}`);
  }

  for (const candidate of candidates) {
    if (candidate.length === 0) {
      continue;
    }
    const next = raw.replace(candidate, " ");
    if (next !== raw) {
      return next.replace(/\s{2,}/g, " ").trim();
    }
  }
  // 一个都删不掉（例如「时间」chip 对应的词已被解析器吞掉）：保持原样
  return raw;
}

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

/** 检索响应（`queryId` 对应 `queries` 表里的评测记录）。 */
export type SearchResponse = SearchResponseDto;

/** 检索状态：`idle` 空查询、`ready` 有结果、`error` 失败。 */
export type SearchStatus = "idle" | "ready" | "error";

/** 命中来源（ExplainBar 文案与卡片徽标共用）。 */
export type HitSource = "fts" | "semantic" | "facet" | "filter";

/** 命中来源：语义 > 字面 > 软命中 > 仅硬条件。 */
export function hitSource(explanation: ExplanationDto): HitSource {
  if (explanation.semantic) {
    return "semantic";
  }
  if (explanation.terms.length > 0) {
    return "fts";
  }
  if ((explanation.facets?.length ?? 0) > 0) {
    // 软命中（颜色/图表/类型/公式）：只加分不排除，所以文案不能写「仅满足筛选条件」
    return "facet";
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
    case "facet":
      return "软命中";
    default:
      return "条件命中";
  }
}

/** 命中来源的长文案（ExplainBar 的一句话解释）。 */
export function hitSourceSentence(
  source: HitSource,
  terms: string[],
  facets: string[] = [],
): string {
  switch (source) {
    case "semantic":
      return "按语义相关性匹配";
    case "fts":
      return terms.length > 0 ? `命中文字：${terms.join("、")}` : "命中文字";
    case "facet":
      return facets.length > 0 ? `参考条件命中：${facets.join("、")}` : "参考条件命中";
    default:
      return "仅满足筛选条件";
  }
}

export type { ItemDto };

/** 软命中 chip 的后缀（后端 `soft_chips` 统一加，用于区分「参考条件」）。 */
export const SOFT_CHIP_SUFFIX = "(参考)";

/** 软 chip 是否「只加分不排除」的参考条件。 */
export function isSoftChip(chip: string): boolean {
  return chip.trim().endsWith(SOFT_CHIP_SUFFIX);
}

/** 去掉软 chip 的「(参考)」后缀，得到原文里的词。 */
export function softWordOf(chip: string): string {
  const text = chip.trim();
  return isSoftChip(text) ? text.slice(0, -SOFT_CHIP_SUFFIX.length).trim() : text;
}

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

  // chip 是展示文案，尝试几种「原文里可能的写法」：整体、去前缀（来自/#）
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

  // 长候选优先：`tag:工作` 必须整体删除，否则先删 `工作` 会留下 `tag:` 残渣
  const ordered = [...candidates].sort((a, b) => b.length - a.length);
  for (const candidate of ordered) {
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

/**
 * 把软词改写成引号字面量：`红色预警` → `"红色" 预警`。
 *
 * **暂未接入 UI**：R2 决定不把软命中做成 chip（只加分不排除，用户不需要「取消」），
 * 所以没有入口调用它。留在这里是因为「想搜这两个字但不想按参考条件排序」是真实需求，
 * 将来若在结果区或搜索框旁加开关，直接复用即可（引号通道是既有语法，后端零改动）。
 * 词不在原文里时原样返回，由调用方决定是否重搜。
 */
export function literalizeChip(raw: string, chip: string): string {
  const word = softWordOf(chip);
  if (word.length === 0 || !raw.includes(word)) {
    return raw;
  }
  const literal = `"${word}"`;
  if (raw.includes(literal)) {
    return raw;
  }
  return raw.replace(word, literal);
}

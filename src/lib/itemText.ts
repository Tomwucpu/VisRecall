/**
 * 素材标题的展示规则与长度上限。
 *
 * 单独成文件（而不是挂在卡片组件上）：详情面板、重命名弹窗与卡片都要用同一条
 * 「用户标题优先、否则来源标题」的规则，放在组件文件里会让 Fast Refresh 失效。
 */

/** 与后端 `MAX_TITLE_CHARS` 一致；超出直接拒绝，不静默截断。 */
export const TITLE_LIMIT = 200;

/** 标题展示：用户改名优先，否则来源标题，再否则给中性兜底。 */
export function displayTitle(item: {
  title?: string | null;
  sourceTitle: string | null;
}): string {
  const title = item.title?.trim();
  if (title !== undefined && title.length > 0) {
    return title;
  }
  const source = item.sourceTitle?.trim();
  return source !== undefined && source.length > 0 ? source : "未命名素材";
}

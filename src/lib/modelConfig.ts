/** 模型 pane 的纯函数与类型（单独放，避免组件文件导出非组件值触发 react-refresh 规则）。 */

/** 单个槽位的可编辑草稿：`apiKey` 只进不出，保存后立刻清空。 */
export type SlotDraft = {
  baseUrl: string;
  model: string;
  apiKey: string;
  /** 后端是否已存密钥（只回布尔，不回明文）。 */
  hasApiKey: boolean;
};

/** base URL 合法性：与后端 `ProviderEntry::validate` 同口径。 */
export function isValidBaseUrl(value: string): boolean {
  const trimmed = value.trim();
  return trimmed.startsWith("http://") || trimmed.startsWith("https://");
}

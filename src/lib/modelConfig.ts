/**
 * 模型 pane 的纯函数与类型（单独放，避免组件文件导出非组件值触发 react-refresh 规则）。
 *
 * 数据契约与后端 `ModelEntry` / `ModelEntryInputDto` 一一对应：
 * 密钥字段「只写不读」——读取视图只有 `hasApiKey` 布尔值。
 */

import type {
  ModelConfigDto,
  ModelEntryDto,
  ModelEntryInputDto,
} from "./bindings";

export type ProtocolKey = "openai" | "anthropic" | "ollama" | "rest";

export const PROTOCOLS: ReadonlyArray<{
  key: ProtocolKey;
  label: string;
  baseUrl: string;
}> = [
  { key: "openai", label: "OpenAI 兼容", baseUrl: "https://api.openai.com/v1" },
  { key: "anthropic", label: "Anthropic Messages", baseUrl: "https://api.anthropic.com/v1" },
  { key: "ollama", label: "Ollama", baseUrl: "http://localhost:11434/v1" },
  { key: "rest", label: "自定义 REST", baseUrl: "" },
];

export function protocolLabel(key: string): string {
  return PROTOCOLS.find((item) => item.key === key)?.label ?? key;
}

export function protocolBaseUrl(key: string): string {
  return PROTOCOLS.find((item) => item.key === key)?.baseUrl ?? "";
}

/** 单个模型的可编辑草稿；`apiKey` 只进不出，保存成功后由后端视图重建。 */
export type ModelDraft = {
  id: string;
  name: string;
  model: string;
  baseUrl: string;
  protocol: string;
  protocolId: string;
  apiKey: string;
  /** 请求清除已保存的密钥（后端把空串解释为删除）。 */
  clearApiKey: boolean;
  /** 后端是否已存密钥（只回布尔，不回明文）。 */
  hasApiKey: boolean;
};

export type ModelSnapshot = {
  entries: ModelDraft[];
  activeId: string | null;
};

export function emptyDraft(): ModelDraft {
  return {
    id: "",
    name: "",
    model: "",
    baseUrl: "",
    protocol: "openai",
    protocolId: "",
    apiKey: "",
    clearApiKey: false,
    hasApiKey: false,
  };
}

/** 与原型同构的本地 id；只含安全字符，会直接决定凭据 account 名。 */
export function newModelId(): string {
  return `cm-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;
}

export function toDraft(entry: ModelEntryDto): ModelDraft {
  return {
    id: entry.id,
    name: entry.name,
    model: entry.model,
    baseUrl: entry.baseUrl,
    protocol: entry.protocol,
    protocolId: entry.protocolId,
    apiKey: "",
    clearApiKey: false,
    hasApiKey: entry.hasApiKey,
  };
}

export function fromConfig(config: ModelConfigDto | undefined): ModelSnapshot {
  return {
    entries: (config?.entries ?? []).map(toDraft),
    activeId: config?.activeId ?? null,
  };
}

/** 整表提交用；留空 = 不改密钥，勾选清除 = 发空串。 */
export function toInput(draft: ModelDraft): ModelEntryInputDto {
  return {
    id: draft.id,
    name: draft.name.trim(),
    model: draft.model.trim(),
    baseUrl: draft.baseUrl.trim(),
    protocol: draft.protocol,
    protocolId: draft.protocolId.trim(),
    apiKey: draft.clearApiKey ? "" : draft.apiKey.trim() === "" ? null : draft.apiKey,
  };
}

/** base URL 合法性：与后端 `ModelEntry::validate` 同口径。 */
export function isValidBaseUrl(value: string): boolean {
  const trimmed = value.trim();
  return trimmed.startsWith("http://") || trimmed.startsWith("https://");
}

/** 表单校验：返回第一条错误，或 `null`。 */
export function validateDraft(draft: ModelDraft): string | null {
  if (draft.name.trim().length === 0) {
    return "请填写显示名称";
  }
  if (draft.model.trim().length === 0) {
    return "请填写模型 ID";
  }
  if (!isValidBaseUrl(draft.baseUrl)) {
    return "地址必须以 http:// 或 https:// 开头";
  }
  if (!PROTOCOLS.some((item) => item.key === draft.protocol)) {
    return "请选择通信协议";
  }
  if (draft.protocol === "rest" && draft.protocolId.trim().length === 0) {
    return "自定义 REST 需填写协议标识";
  }
  return null;
}

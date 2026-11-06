/**
 * 前端唯一数据出口：真实命令与 stub 都从这里进。
 *
 * 阶段 02 重写 spec 后，只需把本文件里的 stub 逐个换成 `commands.*`，
 * 组件侧零改动 —— 因此每个 stub 都必须与将来真实命令**同签名、同返回类型**。
 *
 * stub 的两条硬规则：
 *  1. **读**：返回显式的「还没有」空值（`null` / `[]`），绝不编造内容；
 *  2. **写**：抛与真实命令同形的 `ApiError`（`code = "UNSUPPORTED"`），
 *     `message` 直接面向用户，由调用方原样提示。
 *
 * 真实命令现状（2026-11-06，阶段 01）：
 *  - `get_app_status` / `list_items` / `open_library_folder` / `get_item` /
 *    `get_settings` / `update_settings` 已注册；
 *  - 但 `update_settings` 改键一律 `INVALID_INPUT(restart required)`、
 *    `ball_size` 需重启生效 —— 故设置面板的写入仍按 stub 处理（见 settings 组件）。
 */
import {
  commands,
  type ApiError,
  type AppStatus,
  type ItemDto,
  type SettingsDto,
} from "./bindings";

export type { ApiError, AppStatus, ItemDto, SettingsDto };

/** stub 专用错误码；UI 只按它判断「能力未接入」，不解析 message。 */
export const UNSUPPORTED_CODE = "UNSUPPORTED";

function unsupported(message: string): ApiError {
  return { code: UNSUPPORTED_CODE, message };
}

export function isUnsupported(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    (error as { code?: unknown }).code === UNSUPPORTED_CODE
  );
}

/** 标签 chip：`auto` 为自动标注（左侧「·」与淡色底），`hue` 为令牌名。 */
export type TagChip = {
  id: string;
  name: string;
  auto: boolean;
  hue: string | null;
};

/**
 * 详情增强字段（契约草案 §2）。
 * 都是「按 id 读取」的结果，缺能力时整块为空，界面据此显示占位而不是空白。
 */
export type DetailExtras = {
  /** AI 描述（阶段 03，`SUMMARY_UNAVAILABLE` 时也为 null）。 */
  summary: string | null;
  /** 文本原文前 2 万字符（阶段 02）。 */
  bodyText: string | null;
  /** 原图地址 `visrecall://original/<id>`（阶段 02；缩略图协议已存在）。 */
  originalUrl: string | null;
  /** 标签（阶段 02）。 */
  tags: TagChip[];
};

export type ItemDetail = {
  /** 真实：`get_item` 的完整 DTO（含 `note`）。 */
  item: ItemDto;
  /** stub：阶段 02/03 才有的字段。 */
  extras: DetailExtras;
};

/** 存储用量（阶段 02）。 */
export type StorageUsage = {
  usedBytes: number;
  capacityBytes: number;
  itemCount: number;
};

const PHASE_02 = "阶段 02";
const PHASE_03 = "阶段 03";

/**
 * STUB(阶段 02)：详情增强字段。
 * 正文/原图/标签需要新命令，摘要要等阶段 03；在此之前一律返回空值。
 */
async function stubDetailExtras(_id: string): Promise<DetailExtras> {
  return { summary: null, bodyText: null, originalUrl: null, tags: [] };
}

export const api = {
  // —— 真实命令（阶段 01 既有）——

  getStatus: (): Promise<AppStatus> => commands.getAppStatus(),

  listItems: (cursor: number | null, limit: number): Promise<ItemDto[]> =>
    commands.listItems(cursor, limit),

  openLibraryFolder: (): Promise<null> => commands.openLibraryFolder(),

  getSettings: (): Promise<SettingsDto> => commands.getSettings(),

  /** 真实 `get_item` + stub 增强字段；两者并行，缺失部分不影响基础信息展示。 */
  async loadDetail(id: string): Promise<ItemDetail> {
    const [item, extras] = await Promise.all([commands.getItem(id), stubDetailExtras(id)]);
    return { item, extras };
  },

  // —— STUB：阶段 02 / 03 提供；换实现时签名与返回类型不变 ——

  /** STUB(阶段 02)：`note_set` + `item_note_updated { id }`，300ms 防抖、10000 字上限由前端保证。 */
  async saveNote(_id: string, _text: string): Promise<void> {
    throw unsupported(`备注保存将在${PHASE_02} 提供`);
  },

  /** STUB(阶段 02)：标签云数据。当前返回空数组 —— 「没有标签」是真实状态。 */
  async listTags(): Promise<TagChip[]> {
    return [];
  },

  /** STUB(阶段 02)：`tag_add`，同名复用、自动标签与手动标签语义由后端定。 */
  async addTag(_itemId: string, _name: string): Promise<TagChip> {
    throw unsupported(`标签编辑将在${PHASE_02} 提供`);
  },

  /** STUB(阶段 02)：`CLIPBOARD_BUSY` / `CLIPBOARD_FAILED` / `FILE_MISSING` 由后端给出。 */
  async copyOriginal(_itemId: string, kind: string): Promise<void> {
    const label = kind === "image" ? "复制原图" : kind === "link" ? "复制链接" : "复制原文";
    throw unsupported(`${label}将在${PHASE_02} 提供`);
  },

  /** STUB(阶段 02)：`item_delete`（彻底删除，原图/缩略图/索引一并移除）。 */
  async deleteItem(_id: string): Promise<void> {
    throw unsupported(`删除将在${PHASE_02} 提供`);
  },

  /** STUB(阶段 02)：用量查询。`null` 表示「暂时读不到」，界面显示占位而非 0。 */
  async getStorageUsage(): Promise<StorageUsage | null> {
    return null;
  },

  /** STUB(阶段 02)：容量设置（1–4096 GB）。 */
  async setCapacity(_gigabytes: number): Promise<void> {
    throw unsupported(`容量设置将在${PHASE_02} 提供`);
  },

  /** STUB(阶段 02)：保留策略，`null` 为永久保留。 */
  async setRetention(_days: number | null): Promise<void> {
    throw unsupported(`保留策略将在${PHASE_02} 提供`);
  },
};

/** 详情面板需要的 AI 描述占位文案（阶段 03 前唯一允许的显示内容）。 */
export const SUMMARY_PLACEHOLDER = `待解析（${PHASE_03} 提供）`;

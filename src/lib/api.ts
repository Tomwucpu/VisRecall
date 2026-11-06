/**
 * 前端唯一数据出口：所有 IPC 都从这里进，组件不直接调 `commands.*`。
 *
 * 阶段 02 起本文件不再有 stub：`api.ts` 里的每个方法都对应一条真实命令，
 * 类型直接来自 `bindings.ts`（由 `tauri-specta` 生成，不手写）。
 *
 * 两条约定：
 *  1. 读接口失败走错误码（`ApiError.code`），不把「读不到」伪装成空数据；
 *  2. 不可恢复操作（删除/清空/清理）的确认词判断在 Rust 侧，前端只负责收集输入。
 */
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  commands,
  type ApiError,
  type AppStatus,
  type ItemDetailDto,
  type ItemDto,
  type OrphanStatsDto,
  type PruneReportDto,
  type PurgePreview,
  type RelocateResultDto,
  type RetentionPreviewDto,
  type SettingsDto,
  type StorageUsageDto,
  type TagChipDto,
} from "./bindings";

export type {
  ApiError,
  AppStatus,
  ItemDetailDto,
  ItemDto,
  OrphanStatsDto,
  PruneReportDto,
  PurgePreview,
  RelocateResultDto,
  RetentionPreviewDto,
  SettingsDto,
  StorageUsageDto,
  TagChipDto,
};

/** 标签 chip：`auto` 为自动标注（左侧「·」与淡色底），`hue` 为色令牌名。 */
export type TagChip = TagChipDto;

/** 详情增强字段（契约 §2）；缺能力时整块为空，界面据此显示占位而不是空白。 */
export type DetailExtras = ItemDetailDto["extras"];

export type ItemDetail = ItemDetailDto;

/** 存储用量；读取失败走错误码，界面显示「—」而不是 0。 */
export type StorageUsage = StorageUsageDto;

/** 迁移进度（`phase ∈ prepare | copy | verify | done`）。 */
export type MigrationProgress = {
  phase: string;
  done: number;
  total: number;
};

const PHASE_03 = "阶段 03";

export const api = {
  getStatus: (): Promise<AppStatus> => commands.getAppStatus(),

  listItems: (cursor: number | null, limit: number): Promise<ItemDto[]> =>
    commands.listItems(cursor, limit),

  openLibraryFolder: (): Promise<null> => commands.openLibraryFolder(),

  getSettings: (): Promise<SettingsDto> => commands.getSettings(),

  /** 详情：一次 IPC 拿到素材本体 + 原文/原图 URL + 标签。 */
  loadDetail: (id: string): Promise<ItemDetail> => commands.getItemExt(id),

  /** 备注：300ms 防抖与 10000 字上限由前端保证，后端再独立校验一次。 */
  saveNote: async (id: string, text: string): Promise<void> => {
    await commands.noteSet(id, text);
  },

  /** 标签云（含每个标签的素材数量）。 */
  listTags: (): Promise<TagChip[]> => commands.tagsList(),

  addTag: (itemId: string, name: string): Promise<TagChip> =>
    commands.tagAdd(itemId, name),

  /** 解除某条素材与标签的关联（自动标签会写入抑制记录）。 */
  removeTag: async (itemId: string, tagId: string): Promise<void> => {
    await commands.tagDetach(itemId, tagId);
  },

  renameTag: async (tagId: string, name: string): Promise<void> => {
    await commands.tagRename(tagId, name);
  },

  /** 删除标签并解除其与所有素材的关联，返回受影响的素材数。 */
  deleteTag: (tagId: string): Promise<number> => commands.tagDelete(tagId),

  /**
   * 复制原始内容：图片写 `CF_DIB` + `PNG`，文本/链接写 `CF_UNICODETEXT`。
   *
   * `kind` 仅为兼容既有调用点的可选参数，后端按库里的 `kind` 自己分支。
   */
  copyOriginal: async (itemId: string, _kind?: string): Promise<void> => {
    await commands.copyItemOriginal(itemId);
  },

  /** 单条彻底删除（原图/缩略图/标签关联/任务一并移除）。 */
  deleteItem: async (id: string): Promise<void> => {
    await commands.itemDelete(id);
  },

  /** 用量：已用 / 容量 / 条数。 */
  getStorageUsage: (): Promise<StorageUsage> => commands.storageUsageGet(),

  /** 容量（1–4096 GB，软配额）。 */
  setCapacity: async (gigabytes: number): Promise<void> => {
    await commands.storageCapacitySet(gigabytes);
  },

  /** 保留策略；`null` = 永久保留。 */
  setRetention: async (days: number | null): Promise<void> => {
    await commands.retentionSet(days);
  },

  /** 清理预估：会删掉多少条、释放多少字节（保存前必须让用户看到）。 */
  getRetentionPreview: (days: number | null): Promise<RetentionPreviewDto> =>
    commands.retentionPreview(days),

  /** 按已保存的策略立即清理，返回成功/失败条数。 */
  applyRetention: (): Promise<PruneReportDto> => commands.retentionApply(),

  /** 全库清空所需的确认信息（条数 + 确认词都由后端给）。 */
  getPurgePreview: (): Promise<PurgePreview> => commands.libraryPurgePreview(),

  /** 全库清空；确认词不匹配时后端返回 `INVALID_INPUT`。 */
  purgeLibrary: async (confirm: string): Promise<void> => {
    await commands.libraryPurge(confirm);
  },

  /** 孤儿文件统计（只统计 `originals/`、`texts/`）。 */
  getOrphans: (): Promise<OrphanStatsDto> => commands.storageOrphansGet(),

  /** 清理孤儿文件，返回实际删除数与字节数。 */
  cleanupOrphans: (): Promise<OrphanStatsDto> => commands.storageOrphansCleanup(),

  /** 迁移库位置；成功后必须重启才生效。 */
  relocateStorage: (path: string): Promise<RelocateResultDto> =>
    commands.storageRelocate(path),

  /** 订阅迁移进度；返回退订函数。 */
  subscribeMigration(onProgress: (progress: MigrationProgress) => void): () => void {
    let disposed = false;
    let unlisten: UnlistenFn | null = null;

    listen<MigrationProgress>("storage_migration_progress", (event) => {
      if (!disposed) {
        onProgress(event.payload);
      }
    })
      .then((fn) => {
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch((error: unknown) => {
        console.error("listen storage_migration_progress failed", error);
      });

    return () => {
      disposed = true;
      unlisten?.();
      unlisten = null;
    };
  },
};

/** 详情面板需要的 AI 描述占位文案（阶段 03 前唯一允许的显示内容）。 */
export const SUMMARY_PLACEHOLDER = `待解析（${PHASE_03} 提供）`;

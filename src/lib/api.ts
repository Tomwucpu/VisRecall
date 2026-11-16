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
  type BatchDeleteReportDto,
  type EmbeddingRebuildDto,
  type DirectoryScanOptionsDto,
  type DirectorySelectionDto,
  type DropTargetDto,
  type FileSelectionDto,
  type ImportSourceDto,
  type ImportTaskDto,
  type ImportTaskEntriesPageDto,
  type ImportTaskEntryDto,
  type ItemDetailDto,
  type ItemDto,
  type ItemFiltersDto_Deserialize,
  type ModelConfigDto,
  type ModelConfigPatchDto,
  type ModelEntryDto,
  type ModelEntryInputDto,
  type OrphanStatsDto,
  type ProbeOutcomeDto,
  type PruneReportDto,
  type PurgePreview,
  type RelocateResultDto,
  type RetentionPreviewDto,
  type SettingsDto,
  type SettingsPatch,
  type StorageUsageDto,
  type TagChipDto,
} from "./bindings";
import type { SearchResponse } from "./searchTypes";

export type {
  ApiError,
  AppStatus,
  BatchDeleteReportDto,
  EmbeddingRebuildDto,
  DirectoryScanOptionsDto,
  DirectorySelectionDto,
  DropTargetDto,
  FileSelectionDto,
  ImportSourceDto,
  ImportTaskDto,
  ImportTaskEntriesPageDto,
  ImportTaskEntryDto,
  ItemDetailDto,
  ItemDto,
  ModelConfigDto,
  ModelConfigPatchDto,
  ModelEntryDto,
  ModelEntryInputDto,
  OrphanStatsDto,
  ProbeOutcomeDto,
  PruneReportDto,
  PurgePreview,
  RelocateResultDto,
  RetentionPreviewDto,
  SearchResponse,
  SettingsDto,
  SettingsPatch,
  StorageUsageDto,
  TagChipDto,
};

/** 标签 chip：`auto` 为自动标注（左侧「·」与淡色底），`hue` 为色令牌名。 */
export type TagChip = TagChipDto;

/** 界面筛选条件；组内 OR、组间 AND，空数组表示该维度不限制。 */
export type ItemFilters = ItemFiltersDto_Deserialize;

/** 批量删除结果：成功/失败条数。 */
export type BatchDeleteReport = BatchDeleteReportDto;

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

export const api = {
  getStatus: (): Promise<AppStatus> => commands.getAppStatus(),

  /** 原生选择只返回短期令牌与摘要，路径保留在 Rust 侧。 */
  pickImportFiles: (): Promise<FileSelectionDto | null> => commands.pickImportFiles(),

  pickImportDirectory: (): Promise<DirectorySelectionDto | null> => commands.pickImportDirectory(),

  discardImportSelection: async (): Promise<void> => {
    await commands.discardImportSelection();
  },

  startFileImport: (token: string, entryIds: string[]): Promise<ImportTaskDto> =>
    commands.startFileImport(token, entryIds),

  createTextItem: (title: string | null, body: string, tags: string[]): Promise<string> =>
    commands.createTextItem(title, body, tags),

  scanImportDirectory: (
    token: string,
    options: DirectoryScanOptionsDto,
  ): Promise<ImportTaskDto> => commands.scanImportDirectory(token, options),

  startDirectoryImport: (scanTaskId: string): Promise<ImportTaskDto> =>
    commands.startDirectoryImport(scanTaskId),

  getImportTask: (id: string): Promise<ImportTaskDto> => commands.getImportTask(id),

  listImportTasks: (limit = 20): Promise<ImportTaskDto[]> => commands.listImportTasks(limit),

  listImportTaskEntries: (
    taskId: string,
    cursor: number | null,
    limit = 100,
  ): Promise<ImportTaskEntriesPageDto> => commands.listImportTaskEntries(taskId, cursor, limit),

  cancelImportTask: (id: string): Promise<ImportTaskDto> => commands.cancelImportTask(id),

  listImportSources: (): Promise<ImportSourceDto[]> => commands.listImportSources(),

  refreshImportSource: (id: string): Promise<ImportTaskDto> => commands.refreshImportSource(id),

  relocateImportSource: (id: string, token: string): Promise<ImportSourceDto> =>
    commands.relocateImportSource(id, token),

  removeImportSource: (id: string, removeItems: boolean): Promise<number> =>
    commands.removeImportSource(id, removeItems),

  openReferenceFile: async (itemId: string, reveal = false): Promise<void> => {
    await commands.openReferenceFile(itemId, reveal);
  },

  setImportDropTarget: async (target: DropTargetDto | null): Promise<void> => {
    await commands.setImportDropTarget(target);
  },

  /** 订阅原生拖放结果；监听器在弹窗卸载时全部释放。 */
  subscribeImportDrop(handlers: {
    onReady: (selection: FileSelectionDto) => void;
    onHover: (overTarget: boolean) => void;
  }): () => void {
    let disposed = false;
    const unlisteners: UnlistenFn[] = [];
    const subscriptions = [
      listen<FileSelectionDto>("import_drop_ready", (event) => {
        if (!disposed) handlers.onReady(event.payload);
      }),
      listen<{ overTarget: boolean }>("import_drop_hover", (event) => {
        if (!disposed) handlers.onHover(event.payload.overTarget);
      }),
    ];
    for (const pending of subscriptions) {
      void pending
        .then((unlisten) => {
          if (disposed) unlisten();
          else unlisteners.push(unlisten);
        })
        .catch((error: unknown) => console.error("监听导入拖放失败", error));
    }
    return () => {
      disposed = true;
      for (const unlisten of unlisteners.splice(0)) unlisten();
    };
  },

  listItems: (cursor: number | null, limit: number): Promise<ItemDto[]> =>
    commands.listItems(cursor, limit),

  /** 带结构化筛选的列表分页；无关键词时用它替代检索链路。 */
  listItemsFiltered: (
    filters: ItemFilters,
    cursor: number | null,
    limit: number,
  ): Promise<ItemDto[]> => commands.listItemsFiltered(filters, cursor, limit),

  /** 当前筛选下的全部 id（后端上限 2000），供「全选当前筛选」。 */
  listItemIdsFiltered: (filters: ItemFilters, limit: number | null = null): Promise<string[]> =>
    commands.listItemIdsFiltered(filters, limit),

  openLibraryFolder: (): Promise<null> => commands.openLibraryFolder(),

  getSettings: (): Promise<SettingsDto> => commands.getSettings(),

  /** 设置补丁：只有显式给出的字段会被修改（`null` 表示不改）。 */
  updateSettings: (patch: SettingsPatch): Promise<SettingsDto> =>
    commands.updateSettings(patch),

  /** 目录选择器：对话框在 Rust 侧弹出，前端只拿到路径。取消返回 `null`。 */
  pickDirectory: (): Promise<string | null> => commands.pickDirectory(),

  /** 详情：一次 IPC 拿到素材本体 + 原文/原图 URL + 标签。 */
  loadDetail: (id: string): Promise<ItemDetail> => commands.getItemExt(id),

  /** 备注：300ms 防抖与 10000 字上限由前端保证，后端再独立校验一次。 */
  saveNote: async (id: string, text: string): Promise<void> => {
    await commands.noteSet(id, text);
  },

  /** 保存用户编辑的 AI 描述；`summary_source` 保持不变。 */
  setSummary: (id: string, text: string): Promise<null> => commands.itemSummarySet(id, text),

  /** 保存图片素材的 OCR 文本（不重算文字框）。 */
  setOcrText: (id: string, text: string): Promise<null> => commands.itemOcrTextSet(id, text),

  /** 保存文本素材正文（写回库内文件；外部引用素材只读）。 */
  setBodyText: (id: string, text: string): Promise<null> => commands.itemBodyTextSet(id, text),

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

  /** 批量彻底删除；返回成功/失败条数（`NOT_FOUND` 计入成功，重试幂等）。 */
  deleteItems: (ids: string[]): Promise<BatchDeleteReportDto> => commands.itemDeleteMany(ids),

  /** 保存用户标题；`null` / 空白 = 恢复显示来源标题。 */
  renameItem: (id: string, title: string | null): Promise<ItemDto> =>
    commands.itemRename(id, title),

  /** 重新解析：强制重跑该素材的完整解析链（当前选用模型）。 */
  reparseItem: async (id: string): Promise<void> => {
    await commands.itemReparse(id);
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

  /** 文本/片段是否允许上传云端解析（库内策略，默认关闭）。 */
  setCloudTextAi: async (enabled: boolean): Promise<void> => {
    await commands.cloudTextAiSet(enabled);
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

  /** 检索：解析 → 三路召回 → RRF。`limit` 由后端 clamp 到 1..=200。 */
  search: (
    raw: string,
    limit: number = 50,
    filters: ItemFilters | null = null,
  ): Promise<SearchResponse> => commands.searchItems(raw, limit, filters),

  /** 按模型 id 探测：连通性 + 视觉能力 + 延迟（使用已保存配置）。 */
  probeModel: (id: string): Promise<ProbeOutcomeDto> => commands.modelProbe(id),

  /** 读取模型列表（只回「是否已设置密钥」，永不回传明文）。 */
  getModelConfig: (): Promise<ModelConfigDto> => commands.getModelConfig(),

  /** 整表保存模型列表：密钥写凭据管理器，保存即热生效。 */
  setModelConfig: (patch: ModelConfigPatchDto): Promise<ModelConfigDto> =>
    commands.setModelConfig(patch),

  /** 触发向量蓝绿重建（重建期间检索不中断）。 */
  rebuildEmbeddings: (): Promise<EmbeddingRebuildDto> => commands.embeddingRebuild(),

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
export const SUMMARY_PLACEHOLDER = `待解析`;

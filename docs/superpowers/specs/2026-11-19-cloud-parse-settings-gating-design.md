# VisRecall 云端解析开关统一规划

> 日期：2026-11-19  
> 状态：已实现（自动化测试通过；实机网络验收待做）  
> 范围：把图片/文本是否进行云端解析的决策，从「上传时按来源选择」统一收敛到「设置 → 模型」中的全局开关  
> 原则：默认不上传；开关只决定是否发起云端请求；本地 OCR、FTS、向量索引始终执行；不自动重跑历史素材

## 1. 背景与目标

当前实现存在三个问题：

1. **图片没有开关**。只要配置了云端模型，图片在 OCR 完成后必然接力 `vlm` 上传原图；用户无法只使用本地 OCR 而拒绝图片出网。
2. **上传时还有逐来源授权**。目录导入弹窗提供「允许此来源按全局隐私设置使用云端 AI」，并把 `ai_allowed` 持久化到 `import_sources`，这与「由设置决定」的目标冲突。
3. **判定逻辑分散**。是否上云分别散落在入队函数、`ocr` / `vlm` / `index` handler、重新解析命令中，容易新增入口时绕过开关。

本次目标：

- 在「设置 → 模型」中增加 **「图片上传云端解析」** 开关，与现有 **「文本上传云端解析」** 并列。
- 主窗口「添加」入口的任何内容（选择文件、拖拽文件、新建文字、目录导入），以及截图捕获，都只依据全局开关决定是否云端解析。
- 移除上传时的来源级 AI 授权 UI 与判定。
- 开关关闭时，图片仍完成本地 OCR、FTS 与向量索引并进入 `ready`；文本仍完成本地索引。
- 已入库素材不自动重跑；用户可通过详情页「重新解析」按当前设置处理。

非目标：

- 不实现本地 VLM / 本地模型模式（仍保留 V1.0 占位）。
- 不改变 OCR、嵌入、FTS、向量检索的实现。
- 不自动批量重解析历史素材。
- 不为链接新增云端开关；链接仍不参与自动云端解析。

## 2. 现状核查

| 环节 | 当前行为 | 位置 |
| --- | --- | --- |
| 文本开关 | `ai.cloud_text_ai`，默认关闭；只约束文本素材 | `crates/library/src/ai_policy.rs` |
| 图片上云 | 配置模型后固定走 `ocr → index → vlm`，不受任何开关约束 | `crates/pipeline/src/handlers/ocr.rs`、`vlm.rs` |
| 入队判定 | `cloud_text_ai_pending` 只判断 `Text`；图片恒为 `false`，由 OCR handler 接力 | `crates/app-core/src/pipeline_host.rs` |
| 来源级授权 | 目录导入弹窗复选框；`import_sources.ai_allowed` 默认 `false` | `src/features/import/DirectoryImportPanel.tsx`、`crates/library/src/import_sources.rs` |
| 来源判定 | `cloud_ai_allowed`：托管素材恒 `true`，引用素材要求来源 `ai_allowed && available` | `crates/library/src/import_sources.rs` |
| 阶段一收口 | 仅在来源不允许云端时把素材置 `ready` | `crates/pipeline/src/handlers/index.rs` |
| 阶段二守卫 | 来源允许且文本开关开启才执行；图片直接放行 | `crates/pipeline/src/handlers/vlm.rs` |
| 重新解析 | 文本开关关闭时直接报错；图片强制重跑并自动上云 | `crates/app-core/src/app.rs` |
| 设置界面 | 只有「文本上传云端解析」一个开关；文案明确写着「图片不受此开关影响」 | `src/features/settings/ModelSettingsPane.tsx` |
| 设置 IPC | `SettingsDto.cloud_text_ai` + `cloud_text_ai_set` | `src-tauri/src/ipc.rs`、`src/lib/api.ts` |

关键调用链：

```text
主窗口添加 / 拖拽 / 截图 / 目录导入
        ↓
ImportFlow / CaptureFlow / DirectoryImportService
        ↓
enqueue_initial_indexing / enqueue_bulk_indexing
        ↓
ocr（图片） / index（文本、链接）
        ↓
index 收口 → vlm（当前无图片开关）
```

## 3. 目标行为

### 3.1 决策矩阵

| 素材类型 | 对应开关 | 开关状态 | 实际行为 |
| --- | --- | --- | --- |
| 图片 | 图片上传云端解析 | 开启 | OCR → 本地索引 → 云端 VLM 摘要/标签 |
| 图片 | 图片上传云端解析 | 关闭 | OCR → 本地索引 → `ready`，不发起云端请求 |
| 文本 | 文本上传云端解析 | 开启 | 本地索引 → 云端文本摘要/标签 |
| 文本 | 文本上传云端解析 | 关闭 | 本地索引 → `ready`，不发起云端请求 |
| 链接 | 无 | — | 仅本地索引；不自动进入云端阶段 |
| 引用素材 | 对应开关 | 开关开启 | 还需来源目录当前可用；来源离线时不上云 |
| 引用素材 | 对应开关 | 开关关闭 | 只做本地处理 |

补充规则：

- 开关是「允许上传」的意愿，不等于一定成功。未配置模型、无 Key 或模型不可用时，云端阶段跳过，素材保持 `ready`，不进入 `pending`。
- 开关只影响**之后新增**的素材。切换开关不会回溯处理已经入库的素材。
- 开关状态在执行阶段会再次校验。若任务排队期间用户关闭开关，`vlm` handler 会跳过并把素材置 `ready`。

### 3.2 文案与默认值

- 两个开关默认均为关闭，延续「默认不上传」的隐私策略。
- 开启图片开关时的提示：`已开启：新添加的图片会生成云端摘要与标签`。
- 关闭图片开关时的提示：`已关闭：图片只在本地 OCR 与索引，不再上传`。
- 设置页说明更新为：`图片与文本是否上传云端，分别由下方开关决定；关闭时只做本地 OCR 与索引。`

## 4. 方案设计

### 4.1 使用两个独立开关，而不是一个总开关

推荐保留「图片」「文本」两个独立开关：

- 图片（尤其截图）通常比文本更敏感，用户可能希望文本生成摘要、但不允许原图出网。
- 现有文本开关已经落库并被测试覆盖，新增图片开关是增量改动。
- 一个总开关实现更简单，但会丢失「只允许文本上云」的能力。

如果产品最终希望一个总开关，可在本方案上合并为单一 key；但默认建议保持两个独立开关。

### 4.2 统一授权判定入口

在库层提供唯一的判定函数，所有入队、执行、重新解析路径都调用它，避免散落判断。

```rust
// crates/library/src/ai_policy.rs
pub const KEY_CLOUD_IMAGE_AI: &str = "ai.cloud_image_ai";
pub const KEY_CLOUD_TEXT_AI: &str = "ai.cloud_text_ai";

impl Library {
    pub fn cloud_image_ai_enabled(&self) -> bool { /* 缺失/损坏按 false */ }
    pub fn set_cloud_image_ai(&self, enabled: bool) -> Result<(), AppError> { /* 写库 */ }

    /// 该素材当前是否允许进入云端阶段。
    pub fn cloud_parse_allowed(&self, item: &Item) -> Result<bool, AppError> {
        if !self.cloud_source_available(&item.id)? {
            return Ok(false);
        }
        Ok(match item.kind {
            ItemKind::Image => self.cloud_image_ai_enabled(),
            ItemKind::Text => self.cloud_text_ai_enabled(),
            ItemKind::Link => false,
        })
    }
}
```

同时把 `import_sources.rs` 中的 `cloud_ai_allowed` 改名为 `cloud_source_available`，只负责「素材是否可读」：

```rust
pub fn cloud_source_available(&self, item_id: &str) -> Result<bool, AppError> {
    let Some(asset) = self.external_asset_for_item(item_id)? else {
        return Ok(true);
    };
    Ok(self
        .get_import_source(&asset.source_id)?
        .is_some_and(|source| source.status == SourceStatus::Available))
}
```

改名后，函数名不再暗示「已经包含全部授权」，避免后续调用方误用。

### 4.3 入队与执行链路

| 位置 | 修改 |
| --- | --- |
| `pipeline_host::cloud_text_ai_pending` | 替换为 `cloud_parse_pending(library, item)`，内部调用 `cloud_parse_allowed(...).unwrap_or(false)` |
| `enqueue_indexing_with` | 图片仍只入队 `ocr`；文本在 `cloud_parse_pending` 为真时直接入队 `vlm`；链接不入队 `vlm` |
| `ocr` handler | OCR 完成后入队 `index`；`cloud_parse_pending` 为真时再入队 `vlm`。非图片防御分支同样使用新判定 |
| `index` handler | 本地索引完成后，若 `!cloud_parse_allowed(item)` 则直接置 `ready`；否则保持 `parsing` 等待 `vlm` |
| `vlm` handler | 入口重新校验 `cloud_parse_allowed`；为假时置 `ready` 并返回。图片再检查 `vision.ready()`，文本检查 `text.ready()`，不可用则跳过而不是标 `pending` |
| `capture_flow` / `import_flow` / `directory_import` | 无需为每个入口单独加开关判断；它们最终都经过上述入队函数 |

为图片增加 Provider 就绪判断：

```rust
// crates/domain/src/providers.rs
pub trait VisionProvider: Send + Sync {
    fn id(&self) -> &str;

    /// 与 TextProvider::ready 同构：未配置模型/无 Key 时返回 false。
    fn ready(&self) -> bool { true }

    async fn analyze(&self, png: &[u8]) -> Result<VisionInsight, AppError>;
}
```

`RoutedVision` 覆写为 `self.registry.active().is_some()`。测试中的假实现沿用默认值，不需要逐个修改。

这样即使图片开关开启但尚未配置模型，也不会把新图片批量标成「待重试」。

### 4.4 移除上传时的来源级授权

前端：

- 删除 `DirectoryImportPanel` 中的「允许此来源按全局隐私设置使用云端 AI」复选框。
- 删除 `DEFAULT_DIRECTORY_OPTIONS.aiAllowed` 与恢复扫描时的 `aiAllowed` 传递。
- 在目录面板保留一行静态说明：`云端解析由「设置 → 模型」中的开关统一决定。`

类型与后端：

- 删除 `DirectoryScanOptionsDto.aiAllowed`（TS）与 `DirectoryScanOptions.ai_allowed`（Rust）。
- 删除 `ImportSourceDto.ai_allowed`、`ImportSource.ai_allowed`、`NewImportSource.ai_allowed`。
- `options_from_source` 不再复制该字段。
- `create_import_source` 的 SQL 中 `ai_allowed` 列仍存在，写入字面量 `1`；`ON CONFLICT` 分支也置 `1`。
- `SOURCE_SELECT` 与 `source_from_row` 删除该列。
- 旧任务 `options_json` 中的 `aiAllowed` 会被 serde 忽略；增加一个反序列化测试确认不会导致旧扫描无法恢复。

数据库：

- 新增迁移 `0010_unified_cloud_parse_policy.sql`：`UPDATE import_sources SET ai_allowed = 1;`
- 推荐暂时保留列、删除全部代码引用，避免在 SQLite 上直接 `DROP COLUMN` 带来的兼容风险；后续版本可单独清理。
- 迁移后旧来源的 `false` 不再具有任何效果；新策略完全由全局开关决定。

### 4.5 重新解析

`reparse_item` 改为「本地链路始终重跑，云端阶段按开关决定」：

| 素材 | 开关关闭 | 开关开启 |
| --- | --- | --- |
| 图片 | `ocr → index → ready`，不调模型 | `ocr → index → vlm` |
| 文本 | `index → ready`，不调模型 | `index → vlm` |
| 链接 | `index → ready` | `index → ready`（链接不参与云端） |

同时移除文本重新解析前「开关未开启就报错」的硬拦截：

- 现有拦截会阻止用户单纯重跑本地 OCR/索引，且与图片行为不一致。
- 前端按钮仍显示「重新解析中…」，完成后再由事件刷新状态。
- 可选优化：详情页在开关关闭时给出一行说明「云端解析未开启，本次只重跑本地解析」。

如果产品更希望「重新解析」明确表示云端重解析，可保留硬拦截，但需要同时覆盖图片，并接受无法只重跑本地 OCR。推荐采用「本地始终执行」的方案。

### 4.6 状态语义

- `parsing`：本地或云端阶段仍有工作待完成。
- `ready`：本地索引已完成，且不会再有云端任务执行。
- `pending`：云端任务已经实际执行并失败（如 401/403、最终重试失败），保持现有语义。
- 开关关闭：`index` 完成后直接 `ready`。
- 开关开启但模型不可用：`vlm` 跳过并置 `ready`，不进入 `pending`。
- 云端失败：仍按现有逻辑进入 `pending`，由用户「重新解析」重试。
- 切换开关不会批量改变历史素材状态。

### 4.7 前端设置界面

在 `ModelSettingsPane` 的「当前模型」行下方，沿用现有 `.row` + `.switch` 结构增加两行：

```text
云端解析
  图片上传云端解析    [开关]
    开启后，新添加的图片会把原图发送给当前模型生成摘要与自动标签；
    关闭时只做本地 OCR 与索引。
  文本上传云端解析    [开关]
    开启后，新添加的文本/片段会把原文发送给当前模型生成摘要与自动标签；
    关闭时只在本地解析。
```

交互约定沿用文本开关：

- 不做乐观更新，写库成功后重新读取 `settings`。
- `settings.isPending` 或写入中禁用开关。
- 失败时提示错误并保持原状态。
- 未配置模型时允许切换开关（保留用户意愿），并在说明中提示「未配置模型时不会发起请求」。

`ModelSettingsPane` 顶部说明删除「图片不受此开关影响」，改为「图片与文本分别受下方开关控制」。

## 5. 数据与接口变更

### 5.1 设置项与 IPC

| 项目 | 变更 |
| --- | --- |
| `SettingsDto` | 新增 `cloud_image_ai: bool` |
| `SettingsDto::from_config` | 读取 `library.cloud_image_ai_enabled()`；库不可用时默认 `false` |
| Tauri 命令 | 新增 `cloud_image_ai_set(enabled)`，调用 `AppCore::set_cloud_image_ai` |
| `AppCore` | 新增 `set_cloud_image_ai` 透传库层 |
| `src/lib/api.ts` | 新增 `api.setCloudImageAi(enabled)` |
| `src/lib/bindings.ts` | 通过 Specta 生成，不手改 |

现有 `cloud_text_ai_set` 保持不变。

### 5.2 目录导入 DTO

| 类型 | 删除字段 |
| --- | --- |
| `DirectoryScanOptionsDto`（TS / Rust） | `aiAllowed` / `ai_allowed` |
| `ImportSourceDto` | `aiAllowed` |
| `ImportSource`（库层） | `ai_allowed` |
| `NewImportSource` | `ai_allowed` |

生成绑定后，前端 `ImportDialog`、`DirectoryImportPanel`、`ImportSourcesPane` 及相关测试同步删除该字段。

### 5.3 文件清单

后端：

- `crates/library/src/ai_policy.rs`：新增图片开关与统一判定。
- `crates/library/src/import_sources.rs`：删除来源级授权字段与判定。
- `crates/library/migrations/0010_unified_cloud_parse_policy.sql` 与 `migrations.rs`。
- `crates/domain/src/providers.rs`：`VisionProvider::ready`。
- `crates/app-core/src/pipeline_host.rs`：统一入队判定；`RoutedVision::ready`。
- `crates/app-core/src/app.rs`：`set_cloud_image_ai`；调整 `reparse_item`。
- `crates/app-core/src/directory_import.rs`：移除 `ai_allowed` 选项。
- `crates/app-core/src/import_flow.rs`：注释与测试更新（逻辑走统一入队）。
- `crates/pipeline/src/handlers/ocr.rs`、`index.rs`、`vlm.rs`：统一使用 `cloud_parse_allowed`。
- `src-tauri/src/ipc.rs`：`SettingsDto` 与 `cloud_image_ai_set`。
- `src-tauri/src/import_ipc.rs`：DTO 字段删除。

前端：

- `src/lib/api.ts`、`src/lib/bindings.ts`（生成）。
- `src/features/settings/ModelSettingsPane.tsx` 及其测试。
- `src/features/import/DirectoryImportPanel.tsx`。
- `src/features/import/ImportDialog.tsx` 及其测试。
- `src/features/settings/ImportSourcesPane.test.tsx` 等夹具。

文档：

- 本文件。
- `docs/specs/v0.1/03-ai-search/requirements.md` 的 P3-R03：补充「受设置开关控制」。
- `docs/VisRecall-阶段三-AI与检索架构设计.md` 中「云端 VLM 仅入库侧」的表述可补充「需开关开启」。

## 6. 实施步骤

### 任务 1：库层策略与迁移

- [ ] 在 `ai_policy.rs` 增加 `ai.cloud_image_ai` 读写与 `cloud_parse_allowed`。
- [ ] 将 `cloud_ai_allowed` 重命名为 `cloud_source_available`，去掉 `ai_allowed` 判断。
- [ ] 删除 `import_sources` 相关字段与 `SOURCE_SELECT` 列。
- [ ] 新增迁移 0010，将现有 `import_sources.ai_allowed` 统一置 `1`。
- [ ] 增加库层测试：图片开关默认关闭、持久化、重开库保持；来源离线时统一判定为 `false`。

### 任务 2：入队与 handler

- [ ] `pipeline_host` 用 `cloud_parse_pending` 替换 `cloud_text_ai_pending`。
- [ ] `ocr` handler 使用统一判定决定是否接力 `vlm`。
- [ ] `vlm` handler 执行前重新校验统一判定与 Provider 就绪。
- [ ] `index` handler 在不会进入云端阶段时直接置 `ready`。
- [ ] `VisionProvider` 增加 `ready()`，`RoutedVision` 覆写。
- [ ] 增加 pipeline 测试：图片开关关闭不排 `vlm`；开启但无模型时跳过并 `ready`；文本行为保持。

### 任务 3：重新解析

- [ ] `reparse_item` 移除文本硬拦截，改为按 `cloud_parse_allowed` 决定是否入队 `vlm`。
- [ ] 图片重解析在开关关闭时只跑 OCR/索引，开启时由 OCR 接力 `vlm`。
- [ ] 更新 `crates/app-core/tests/pipeline_host.rs` 中的重新解析断言。

### 任务 4：设置与 IPC

- [ ] `SettingsDto` 增加 `cloud_image_ai`。
- [ ] 新增 `cloud_image_ai_set` 命令并注册 Specta。
- [ ] `src/lib/api.ts` 增加 `setCloudImageAi`。
- [ ] 重新生成 `src/lib/bindings.ts`。

### 任务 5：前端设置与导入弹窗

- [ ] `ModelSettingsPane` 增加图片开关，更新说明与 Toast 文案。
- [ ] 删除目录导入弹窗中的来源级 AI 复选框，增加静态说明。
- [ ] 删除 `aiAllowed` 相关默认值、恢复逻辑与测试夹具。
- [ ] 增加前端测试：两个开关默认关闭、切换落库、失败保持原状态。

### 任务 6：验证与文档

- [ ] 运行前后端测试与类型检查。
- [ ] 实机验证图片/文本/目录/截图在开关开闭下的网络行为。
- [ ] 更新 P3-R03 与相关架构文档的措辞。

## 7. 测试计划

### 7.1 后端

- `crates/library/tests/ai_policy_db.rs`
  - 图片开关默认 `false`。
  - 开启后重开库仍为 `true`。
  - 坏值/缺失回落 `false`。
- `crates/app-core/tests/ai_policy_flow.rs`
  - `set_cloud_image_ai` 透传并持久化。
- `crates/app-core/src/import_flow.rs`
  - 图片开关关闭：不产生 `STAGE_VLM` 任务；`STAGE_OCR` 正常。
  - 图片开关开启：`ocr` 执行后产生 `STAGE_VLM`。
  - 文本测试保持并补充「图片不受文本开关影响」。
- `crates/app-core/tests/pipeline_host.rs`
  - 图片开关关闭时重新解析不排 `vlm`。
  - 开关开启时重新解析排 `vlm`。
  - 文本开关关闭时重新解析只做本地索引。
  - 链接重新解析不排 `vlm`。
- `crates/pipeline/tests/two_stage.rs`
  - 默认关闭：排队的图片 `vlm` 任务也不得调用云端。
  - 开启：图片 `vlm` 正常执行。
  - 未配置模型：图片 `vlm` 跳过并置 `ready`，不进入 `pending`。
- 目录导入
  - 旧 `options_json` 含 `aiAllowed` 时仍可反序列化。
  - 来源级 `ai_allowed=false` 不再阻止云端解析。
  - 来源离线时仍不进入云端阶段。

### 7.2 前端

- `ModelSettingsPane.test.tsx`
  - 图片开关默认关闭。
  - 点击后调用 `setCloudImageAi(true)` 并重新读取设置。
  - 写入失败保持原状态并提示。
  - 文本开关行为不回归。
- `ImportDialog.test.tsx` / `DirectoryImportPanel` 相关测试
  - 目录选项不再渲染云端 AI 复选框。
  - 扫描请求不再携带 `aiAllowed`。
- `ImportSourcesPane.test.tsx`
  - 夹具删除 `aiAllowed`，列表仍正常渲染。

### 7.3 验证命令

```powershell
pnpm test
pnpm typecheck
pnpm lint
pnpm build
cargo test -p visrecall-library -p visrecall-app-core -p visrecall-pipeline
cargo test --workspace
```

绑定更新：

```powershell
cargo test -p visrecall --bin visrecall -- --ignored refresh_committed_bindings
cargo test -p visrecall --bin visrecall committed_bindings_are_up_to_date
```

### 7.4 实机验收

- 关闭两个开关，配置了模型：添加图片/文本/目录/截图，抓包确认没有云端请求；素材最终为 `ready`，本地检索可用。
- 只开图片开关：图片产生摘要与标签，文本不上云。
- 只开文本开关：文本产生摘要与标签，图片不上云。
- 未配置模型但开关开启：素材不进入 `pending`，保持 `ready`。
- 目录导入不再出现来源级 AI 复选框；已有来源的行为与全局开关一致。
- 重新解析在开关关闭时只重跑本地链路，在开启时执行云端增强。
- 旧的含 `aiAllowed` 扫描任务可以正常恢复。

## 8. 风险与取舍

| 风险 | 影响 | 处理 |
| --- | --- | --- |
| 图片默认不再上云 | 现有用户升级后，新图片不会自动生成云端摘要 | 属于本次目标行为；在设置页与更新说明中明确 |
| 历史素材不回溯 | 已入库图片不会因开启开关自动补摘要 | 保持与现有文本开关一致；用户可逐条「重新解析」 |
| 删除来源级授权 | 旧来源的 `ai_allowed=false` 不再生效 | 迁移统一置 `1`；如产品要求保留逐来源选择，需要重新设计，而不是本方案 |
| 开关开启但无模型 | 若直接入队会产生失败的 `vlm` 任务 | `vlm` handler 检查 `vision.ready()` / `text.ready()` 并跳过 |
| 开关在排队期间切换 | 任务执行时状态可能已变化 | `vlm` 执行前重新校验，关闭则跳过并置 `ready` |
| 保留 `ai_allowed` 列 | 数据结构中仍有死字段 | 先删代码引用并迁移归一，后续版本再评估 `DROP COLUMN` |
| 绑定文件不同步 | 前端类型与后端不一致 | 只通过 Specta 生成，跑 `committed_bindings_are_up_to_date` |

## 9. 待确认决策

| 决策 | 推荐 | 备选 |
| --- | --- | --- |
| 开关粒度 | 图片、文本两个独立开关 | 一个「云端解析」总开关 |
| 重新解析在开关关闭时 | 本地链路照常执行，云端阶段跳过 | 直接报错，要求先开启对应开关 |
| `import_sources.ai_allowed` | 保留列、归一为 1、删除代码引用 | 新增迁移直接 `DROP COLUMN` |
| 未配置模型时开关 | 允许切换并保留意愿，说明不会发请求 | 禁用开关直到配置模型 |

## 10. 验收清单

- [ ] 「设置 → 模型」同时显示图片、文本两个云端解析开关，默认关闭。
- [ ] 两个开关的切换写库成功、失败回滚、重新打开设置后状态一致。
- [ ] 图片开关关闭时，添加图片不产生任何云端请求，OCR 与本地检索正常。
- [ ] 图片开关开启且模型可用时，图片生成云端摘要与自动标签。
- [ ] 文本开关行为与现有功能一致，且不再受图片开关影响。
- [ ] 主窗口选择文件、拖拽、新建文字、目录导入、截图捕获都遵循同一套开关。
- [ ] 目录导入弹窗不再提供来源级云端 AI 复选框。
- [ ] 未配置模型时，开启开关不会把新素材标成「待重试」。
- [ ] 「重新解析」按当前开关执行，不会强制上传或卡在 `parsing`。
- [ ] 链接素材不会自动进入云端阶段。
- [ ] 旧扫描任务与旧来源数据可正常读取，不因字段删除而失败。
- [ ] 前后端测试、类型检查、Lint、构建全部通过。

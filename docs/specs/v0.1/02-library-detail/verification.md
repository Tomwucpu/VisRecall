# 阶段 02：存储与详情 · 验收记录

> 状态：**已执行（2026-11-06，有条件）**。自动化门禁全绿（473 Rust + 76 前端用例）；数据层/编排层的验收项由集成测试取证；**依赖 GUI 交互与物理条件**的项（物理 4K、双屏、缩放切换、物理拖放、DRM、提权窗口、30 分钟长跑、截图目视）在本轮**未执行**，逐项标 `未验证（环境限制）` 并写原因与替代证据，见 §1/§2/§4。
> 口径：每一项必须有**结论 + 证据**。结论只能是「通过 / 未通过 / 未验证（环境限制）」；「未验证」必须写原因与替代证据，不允许留空或静默勾选。
> 证据形式：命令输出片段、日志片段、测试用例名（`cargo test` / `pnpm test`）、数据库查询结果。**本轮没有截图**：执行环境禁止 `Add-Type`（截屏 API 被安全策略拦截），所有视觉项以进程指标 + 日志 + 组件测试替代，并在证据栏注明。

## 0. 环境与产物

| 项 | 值 |
|---|---|
| 日期 | 2026-11-06 |
| 机器 / CPU / 内存 | AMD Ryzen 7 7840H（8C/16T）/ 15.2 GB；显示输出为核显 `AMD Radeon(TM) 780M`，另有 `NVIDIA GeForce RTX 4060 Laptop`（未参与覆盖层） |
| Windows 版本（`winver`） | Windows 11 家庭中文版 10.0.22621（Build 22621.1344，22H2） |
| 显示配置（分辨率 / 缩放 / 屏数） | **1 台**，2560×1440 @165 Hz；缩放 **125%**（`HKCU\Control Panel\Desktop\LogPixels = 120`） |
| 库路径与素材条数 | `C:\Users\Tomwu\Documents\VisRecall`，`items = 1`（`image` / `status=ready`，阶段 01 遗留的那条 `Code.exe` 截图）；`visrecall.db` 90,112 B；`tags = 0` |
| `cargo --version` / `rustc --version` | cargo 1.93.1 (083ac5135 2025-12-15) / rustc 1.93.1 (01f6ddf75 2026-02-11) |
| `node --version` / `pnpm --version` | v22.22.2 / 10.30.3 |
| release 二进制路径与构建时间 | `target/release/visrecall.exe`，20,015,616 B（19.1 MiB），构建于 2026-11-06 17:35（`cargo build --release -p visrecall`） |
| commit | `0735669`（Task 20 收尾）+ 本文件所在的 Task 21 提交 |

## 0.1 测试数据构成（固定 20 条）

> **本轮未构造这 20 条数据。** 造数口径要求「14 次快捷键捕获 + 6 次悬浮球拖入」全部走正常路径，需要真实 GUI 交互（模拟输入注入 + 应用窗口焦点），本轮执行环境只能运行测试与二进制、不能操作界面。因此：库内现有 1 条阶段 01 遗留素材；§2 中依赖这批数据的项一律标 `未验证（环境限制）`，其**语义**由集成测试覆盖（`crates/*/tests/*`）。补齐清单见 §5。

> 规模口径：**20 条**（2026-11-06 调整，原为 100 条）。20 条覆盖排序、状态流转、标签复用、用量统计、删除与迁移；分页需临时调小页大小（见下文），不足以给出 100 条规模的性能结论。

| 类别 | 条数 | 覆盖点 |
|---|---|---|
| 图片 | 14 | 1080p 全屏 4（含 1 条 4K）、窗口截图 4、小区域截图 2、拖入 JPEG 1、长边 >4000 的超长图 1、GIF 1、WebP 1；4K 那条用于原图加载与详情耗时 |
| 文本 | 4 | 1 条 ≥20000 字符（截断边界）、1 条含 emoji/代理对、1 条 <100 字、1 条带 UTF-8 BOM |
| 链接 | 2 | 1 条标题抓取成功、1 条抓取失败回退为 URL |
| **合计** | **20** | 14 条来自捕获，6 条来自拖入 |

以下为在上述 20 条上做的**附加处置**，不额外增加条数：

| 处置 | 数量 | 取值来源与验证点 |
|---|---|---|
| 标签 | 3 个 | `work` 关联 5 条、`参考` 2 条、`待读` 1 条（验证复用、排序、数量统计） |
| 备注 | 3 条 | 其中 1 条接近 10000 字符（验证上限与保存耗时） |
| 缺失缩略图 | 2 条 | 手工删除 `thumbs/<id>.png`，验证卡片/详情占位 |
| 缺失原图 | 1 条 | 手工删除 `originals/...png`，验证 `FILE_MISSING` 与协议 404 |
| 失败态 | 1 条 | 手工把 `status` 置为 `failed`，验证徽标与删除入口（可选） |

预期库体量：约 **30–60 MB**（4K 图与超长图占主要部分），用于对账 P2-R07 的用量统计。

门禁第 11 项要求「≥20 次真实捕获」，与「库内常驻 20 条」不冲突：库内 20 条由 14 次捕获 + 6 次拖入组成，另补 6 次重复捕获（验证后删除）以满足捕获次数口径，按日志计数取证。

**分页需要单独构造条件**：前端页大小 `ITEM_PAGE_SIZE = 50`（`src/features/library/useItems.ts`），20 条素材只会加载一页，「加载更多」永远不出现。验收分页时临时把该常量改为 10 并本地构建（验收后还原为 50），同时以 `library::list_items` 的游标测试作为自动化背书；验收记录（§1 第 9 行与 §3）必须写明「临时页大小 10」这一前提。

## 0.2 造数与改数方法

- 20 条全部通过正常路径产生：14 条快捷键捕获（图片）+ 6 条悬浮球拖入（4 文本 + 2 链接）；不手工插库造记录。
- 本机无 `sqlite3` CLI，需要直接改库时用 Python 内置模块，**必须先退出应用**（否则与 WAL 竞争）：

```bash
python - <<'PY'
import sqlite3, time
db = r"C:\Users\<user>\Documents\VisRecall\visrecall.db"
old = int(time.time() * 1000) - 3 * 86_400_000          # 3 天前
with sqlite3.connect(db) as c:
    c.execute("UPDATE items SET captured_at = ? WHERE id IN (?, ?, ?)", (old, "id1", "id2", "id3"))
PY
```

- 保留策略验收需要「3 天前」的素材：按上面方式把 3 条记录的 `captured_at` 改到 3 天前，再重启应用观察启动清理。
- 缺口样本（缺失缩略图 / 缺失原图）通过手工删除对应文件构造，不要改 `thumb_path`。

## 1. 阶段 01 门禁回填

> 来源：`docs/specs/v0.1/README.md` §1。回填目标同时包含 `01-capture-loop/verification.md`（勾选原条目）与本表。全部完成后，README §1 的对应项也要标注结论。

| # | 项目 | 结论 | 证据 |
|---|---|---|---|
| 1 | 物理 4K 面板 + PresentMon 交叉验证（Task 10 场景 B/C） | **未验证（环境限制）** | 本机无 4K 面板（单屏 2560×1440），也未安装 PresentMon。替代证据：Task 10 的**合成 4K** 三组实测（avg 6.060/6.061/6.064 ms，P95 ≤6.28 ms，max ≤13.18 ms，drops=0，首帧 116–132 ms）——DWM 只裁剪不缩放，渲染负载是完整 4K，但不能声称「物理 4K 已验证」。见 `01-capture-loop/verification.md` D 表 |
| 2 | 双屏与混合 DPI：双屏同 DPI、150%+100%、负坐标/竖直错位、覆盖层临空边、标签归属、跨屏拼合 | **未验证（环境限制）** | 本机只有 1 台显示器（`Get-CimInstance Win32_VideoController` 仅 `AMD Radeon 780M 2560x1440` 有活动分辨率），无法构造第二屏/负坐标。替代证据：`crates/shell/tests/stitch.rs`（跨屏拼合、屏缝去重）、`crates/capture/tests/display_geometry.rs`（DPI/坐标换算） |
| 3 | 单屏 100% / 125% / 200% 缩放（150% 已验） | **部分：125% 生效中，100%/200% 未验证** | 当前系统缩放 **125%**（`LogPixels = 120`）；阶段 01 的选区几何验收是在 **150%**（DPI 144）下做的（`01-capture-loop/verification.md` 行 3/7，500×400+400+300）。100%/200% 需注销会话改缩放，本轮未做；无自动化替代（DPI 换算只有单测） |
| 4 | 物理 OLE 四来源拖放：Chrome 文字、Word 文本、资源管理器图片、微信图片 | **未验证（环境限制）** | 需真实物理拖拽（鼠标按住跨进程拖），本轮无 GUI 会话。替代证据：Task 16 用**真实 OLE 管线**（真实剪贴板格式 + `DoDragDrop` + `RegisterDragDrop`）跑通 12 个场景；`crates/shell/tests/drop_parse.rs` |
| 5 | DRM 视频 / 独占全屏：全黑帧 → `DRM_PROTECTED` | **未验证（环境限制）** | 未构造真实 DRM/独占全屏窗口。替代证据：`visrecall_capture::grab_gdi::is_blank_frame` 单测 + `capture_flow` 的全黑帧分支（`select_usable_frames` 部分/全部全黑的处理） |
| 6 | 提权窗口与 UIPI 拖放（高完整性级别） | **未验证（环境限制）** | 需 UAC 提权会话；本轮所有进程均为中完整性级别。行为未验证，列入 §4 已知限制 |
| 7 | 实时模式决策（D2-01） | **通过（决策项）** | 已定：不实现「透明覆盖层 + 提交时现场抓取」，维持 `CAPTURE_FAILED` 降级。见 `design.md` §1.1、requirements P2-R14 |
| 8 | 托盘内存与空闲 CPU（关闭主窗口 60s 后私有工作集；10 分钟 PerfMon） | **部分：内存有本次实测，空闲 CPU 未按口径** | 本次 release 实测：托盘驻留（未开主窗口）WS **42.2 MB** / 私有 **33.5 MB** / 30 线程；主窗口打开后 WS **51.9 MB** / 私有 **36.4 MB** / 50 线程 / 1012 句柄。空闲 CPU 未做 10 分钟 PerfMon（无 PerfMon 采集，只有阶段 01 的 60 s 短采样） |
| 9 | 列表视觉验收：20 条滚动、`加载更多` 反复翻页、`解析中 → 正常` 过渡（原 100 条口径按现行规模下调） | **未验证（环境限制）** | 库内只有 1 条素材，且无 GUI 操作。替代证据：`crates/library/tests/items_db.rs`（`captured_at DESC` + keyset 游标分页：`list_items` 的游标/limit 边界）、`src/features/library/ItemGrid.test.tsx`（`keeps the grid and shows a retry bar`、空态/错误态）、`src/app/App.test.tsx`（事件刷新语义）。**未做**帧率或掉帧量化 |
| 10 | 打包产物（D2-02） | **通过** | Task 20：`src-tauri/Cargo.toml` 增加 `[features] default = ["custom-protocol"]`，`cargo build --release -p visrecall` 产出 20,015,616 B 的二进制；`bundle.active = false`，不产安装包。脱离 `pnpm dev` 运行见 §2 P2-R13 |
| 11 | 30 分钟连续使用 + ≥20 次真实捕获 | **未验证（环境限制）** | 需 30 分钟真实交互（快捷键捕获 20 次）。本轮进程只运行约 1 分钟做 release 冒烟（无崩溃）。阶段 01 的记录是「21 分钟 / 4 条成功入库」，两次都不满足 requirements §5 退出条件 2，**仍未达成** |
| 12 | 同一画面重复捕获不去重 | **未验证（环境限制）** | 代码无去重逻辑（与 requirements 一致），但「连续两次捕获同一画面产生两条素材」需真实两次捕获；`library.insert_item` 不做 content_hash 唯一约束（`items_db.rs` 无唯一索引断言可佐证「不去重」的实现选择，但不等于实测） |
| 13 | 滚动/帧率量化 | **未验证（环境限制）** | 无 PresentMon；WebView2 侧无帧率工具。替代证据：阶段 01 D 表的 CPU 提交耗时（draw 0.53–0.87 ms、input→present max 17.84 ms），只说明预算内，不是帧率 |

## 2. 阶段 02 验收矩阵

| 需求 | 场景 | 结论 | 证据 |
|---|---|---|---|
| P2-R01 | 详情：图片 / 文本 / 链接各 3 条 | **未验证（环境限制）** | 库内只有 1 条图片素材，无 GUI 操作。替代证据：`crates/app-core/tests/detail_extras.rs`（`image_has_original_url_and_no_body_text`、`link_has_neither_body_text_nor_original_url`、`missing_item_is_not_found`）+ `src/features/detail/DetailPanel.test.tsx::renders real metadata from get_item_ext and hides absent size as a dash` |
| P2-R01 | 原图缺失时的占位与复制失败提示 | **通过（自动化）** | `detail_extras.rs::missing_original_file_still_returns_detail`；`src-tauri/src/protocol.rs` 的原图 404/400/405 用例；`DetailPanel.test.tsx::explains a missing original file instead of a generic error`；`ImageOverlay` 原图→缩略图→双缺失三级降级（`src/features/detail/ImageOverlay.tsx`）。未目视 |
| P2-R01 | 20000 字符截断边界（含 emoji） | **通过** | `detail_extras.rs`：`text_below_limit_is_not_truncated`、`text_exactly_at_limit_is_not_truncated`、`text_over_limit_is_truncated_with_marker`、`truncation_point_survives_emoji`（按 `char_indices` 截断，不会切坏代理对） |
| P2-R01 | 详情读取耗时（P95，10 次采样） | **未验证（环境限制）** | 未采样：需要 20 条数据与可交互界面。无替代证据（未做基准测试） |
| P2-R02 | 300ms 防抖、切换素材 flush、10000 字上限 | **通过（防抖与上限）/ 切换 flush 未单测** | `DetailPanel.test.tsx::debounces the note and persists it through note_set`、`blocks input beyond the 10000 character limit instead of saving an overflow`；`crates/library/tests/detail_db.rs::note_accepts_exactly_the_limit_counting_chars`、`note_rejects_one_char_over_limit_and_keeps_old_value`。切换素材时的 flush 由 `useEffect` 清理函数实现，**没有独立断言** |
| P2-R02 | 保存失败时文本不丢、显示失败态 | **通过** | `DetailPanel.test.tsx::keeps the note text and reports failure when saving fails`（`SaveState = error`，输入框内容保留） |
| P2-R03 | 同名复用（大小写混输）、detach、rename、delete | **通过** | `crates/library/tests/tags_db.rs`：`adding_same_tag_to_two_items_reuses_one_row`、`tag_names_are_case_insensitive`、`tag_add_normalizes_and_rejects_bad_names`、`detaching_manual_tag_keeps_tag_row`、`detaching_auto_tag_writes_suppression_and_drops_tag`、`manual_add_clears_suppression`、`rename_updates_name_and_rejects_collision`、`delete_returns_removed_link_count`、`item_tags_follow_item_deletion_by_cascade` |
| P2-R03 | 标签云数量与排序 | **通过** | `tags_db.rs::tag_cloud_orders_by_count_then_name`（count DESC, name ASC）；`src/features/shell/Sidebar.test.tsx::renders one pill per tag with its item count`、`renames a tag through tag_rename`、`deletes a tag only after confirming the affected count` |
| P2-R04 | 图片粘贴到 Word / 微信 / Chrome | **未验证（环境限制）** | 需真实目标应用接收粘贴，本轮无法操作 GUI。替代证据：`crates/shell/tests/clipboard_bytes.rs`（`dib_starts_with_bitmapinfoheader_without_file_header`、`dib_first_pixel_is_bottom_left_in_bgra`、`dib_alpha_channel_is_preserved`）+ `clipboard.rs` 同时写 `CF_DIB` 与 `RegisterClipboardFormatW("PNG")` 两种格式 |
| P2-R04 | 文本粘贴换行正确；链接粘贴为 URL | **通过（字节层）** | `clipboard_bytes.rs`：`utf16_crlf_adds_carriage_returns_and_trailing_nul`、`utf16_crlf_does_not_double_carriage_returns`、`utf16_crlf_handles_empty_and_non_bmp_text`；链接走 `copy_text(source_url)`（`AppCore::copy_original` 的 `ItemKind::Link` 分支）。未在真实应用里粘贴 |
| P2-R04 | `CLIPBOARD_BUSY` / `FILE_MISSING` 提示 | **通过（自动化）** | 错误码映射 `capture_failed_code`/`AppError` → `CLIPBOARD_BUSY`；`ClipboardGuard` 重试 5×20 ms 后返回 `ClipboardBusy`（`crates/shell/src/clipboard.rs`）；`DetailPanel.test.tsx::explains a missing original file instead of a generic error` |
| P2-R04 | 富文本片段粘贴带图片（Word / WPS / Chrome 网页编辑器） | **待手工验收** | 代码层：`crates/shell/tests/clipboard_bytes.rs::cf_html_offsets_point_at_the_fragment`（`CF_HTML` 偏移是字节偏移）、`crates/shell/src/html_snippet.rs` 的 `sanitize_strips_scripts_and_keeps_images` / `sanitize_absolutizes_relative_image_sources`、`import_flow::tests::text_snippet_keeps_its_html_for_copy`、`clipboard::copy_html` 同时写 `CF_HTML` 与 `CF_UNICODETEXT`。图片保留源站 URL：需要登录态或防盗链的图片**粘不出图**（已知限制，本轮不做本地缓存） |
| P2-R04 | 无 HTML 的旧素材复制仍是纯文本 | **通过（自动化）** | `import_flow::tests::plain_text_snippet_has_no_html_record`；`AppCore::copy_original` 的 `Text` 分支在无 HTML 记录时走 `copy_text`（向后兼容） |
| P2-R04 | 片段 HTML 随素材删除不残留 | **通过（自动化）** | `crates/library/src/snippets.rs::html_follows_item_deletion`：`snippet_html` 靠 `ON DELETE CASCADE` 随 `items` 行删除，不进孤儿扫描白名单 |
| P2-R05 | 删除后文件与关联行均消失 | **通过** | `crates/library/tests/purge_db.rs`：`delete_item_removes_row_file_thumb_and_jobs`（items/jobs/item_tags 行 + 原图 + 缩略图同时消失）、`delete_item_succeeds_when_original_already_missing`、`delete_missing_item_is_not_found` |
| P2-R05 | 原图只读时删除：库已删、日志有 warn、孤儿可计数 | **通过（记录被删；文件可能仍在）** | `purge_db.rs::delete_item_with_readonly_file_still_removes_row`：断言**库记录已删**；Windows 上只读属性不阻止已打开句柄的删除，因此不要求文件消失，残留由孤儿扫描兜底（`storage_db.rs::orphan_file_is_counted_and_cleaned`） |
| P2-R06 | 清空：确认词校验、表计数归零、策略保留 | **通过** | `purge_db.rs`：`purge_clears_tables_and_content_dirs_but_keeps_settings`、`purge_empty_library_is_noop`、`clear_content_dirs_keeps_directories`、`capture_still_works_after_purge`；确认词由后端下发 `PURGE_CONFIRMATION_WORD = "删除全部"`，前端 `SettingsDialog.test.tsx::keeps the purge button disabled until the confirmation word matches` |
| P2-R07 | 用量与手工求和一致；容量边界 1/4096 与越界 | **通过** | `crates/library/tests/storage_db.rs`：`usage_matches_manual_sum`、`capacity_range_is_enforced_before_writing`、`corrupt_capacity_falls_back_to_default`、`retention_range_and_forever_default` |
| P2-R07 | 用量超容量时捕获被拒（注入） | **通过** | `crates/app-core/src/capture_flow.rs` 单测：`usage_at_capacity_rejects_capture_before_hiding`、`usage_below_capacity_allows_capture`（用 `UsageProbe` 注入，不造 50 GB 数据）；旁路入口 `retention_flow.rs::quota_helper_reports_storage_full` |
| P2-R08 | 保留 1 天 → 重启后清理；`None` 不清理 | **通过（逻辑层）** | `crates/app-core/tests/retention_flow.rs`：`forever_policy_prunes_nothing`、`expired_items_are_pruned_and_reported_once`、`pruning_older_items_first_leaves_the_newest_untouched`、`delete_failure_is_counted_without_stopping_the_batch`；启动清理挂在 `start_runtime` 末尾（`spawn_startup_prune`）。**未做**真实「改 3 天前 → 重启观察」的端到端（需 GUI + 改库） |
| P2-R08 | 预估条数正确；取消不写入 | **通过** | `retention_preview` 走 `Library::list_expired` 同一口径（`detail_db.rs::counts_track_inserts_and_cutoff`、`list_expired_returns_oldest_first_within_limit`）；`SettingsDialog.test.tsx::asks for a preview before saving the retention policy`、`cancelling the retention preview does not write anything` |
| P2-R09 | 迁移成功（同盘 / 另一块盘） | **通过（同盘）/ 跨卷未验证** | `crates/app-core/tests/relocate_flow.rs::successful_relocation_switches_config_and_requires_restart`（`files_copied ≥ 3`、`config.json` 落盘、源目录保留、进度事件 `prepare→copy→verify→done`；受控库在系统临时目录内，与目标同卷）。**跨卷**（不同盘符）未实测 |
| P2-R09 | 拒绝路径：非空目录、自身子目录、空间不足 | **通过** | `crates/library/tests/relocate_fs.rs`：`rejects_relative_target`、`rejects_target_inside_source_and_source_inside_target`、`rejects_same_path_and_non_empty_target`、`rejects_when_target_volume_lacks_space`、`accepts_an_empty_directory`、`component_wise_prefix_comparison`（`C:\lib` 与 `C:\lib2` 不同） |
| P2-R09 | 中途失败：config 未变、原库可用、运行时恢复 | **通过** | `relocate_flow.rs::copy_failure_keeps_config_and_resets_the_gate`：复制到一半让 `rename` 失败（目标里 `visrecall.db` 被占成目录）→ `config.json` 逐字节未变、`gate() == Idle`、`restart_required == false`、`ball_present_for_test()` 与 `hotkey_spec_for_test()` 均恢复、源库条数不变、目标残留保留 |
| P2-R09 | **checkpoint 未清空即中止**：另开连接持读事务后发起迁移 → 返回 `BUSY`、目标目录无文件 | **通过** | `relocate_flow.rs::blocked_checkpoint_aborts_before_copying`：另开只读连接持快照（先用 `setting_set` 造两帧 WAL，否则 TRUNCATE 仍能完成）→ 返回 `AppError::Busy`、目标目录 0 个条目、`config.json` 未变、闸门回 `Idle` |
| P2-R09 | **迁移成功后捕获禁用**：按快捷键、点悬浮球、向球拖入一张图各一次 → `BUSY` 提示、旧库条数不变 | **通过（两条写入口）** | `relocate_flow.rs::capture_is_rejected_while_a_restart_is_pending`（`handle_shell_event` 分发处拒绝 + `coordinator.start()` 纵深防御，`capture_failed.code == "BUSY"`，条数不变）、`drop_is_rejected_while_a_restart_is_pending`（`ShellEvent::Drop` 不经过 `start`，同处被挡）。**快捷键/球点击的物理按键**未做，但两条入口在代码上同一处 |
| P2-R09 | **待重启状态可观测**：横幅常驻、`get_app_status.restart_required = true`、刷新界面后仍在 | **通过** | `src/app/App.test.tsx::shows a persistent banner when a library migration awaits restart` / `hides the banner when no restart is required`；`AppStatus.restart_required` 由 `core.restart_required()` 派生（闸门），`relocate_flow` 断言迁移后为 `true`、重启后为 `false` |
| P2-R09 | **重启前「打开库目录」指向新路径**（不打开旧目录） | **通过** | `AppCore::library_root()` 在 `RestartPending` 时返回 `config.library_path`；`relocate_flow` 断言 `RestartPending` 下 `library_root() == target`、`Idle` 下 `== 原库根` |
| P2-R09 | **重启后恢复**：横幅消失、捕获可用、新素材落在新库 | **通过（库切换部分）** | `relocate_flow.rs::restart_picks_up_the_new_library`：用同一 config 重建 `AppCore` → `gate() == Idle`、`restart_required == false`、`library_root() == 新路径`、条数一致。「捕获可用 + 新素材落在新库」需真实捕获，未做 |
| P2-R10 | 三档尺寸即时生效 | **通过（自动化）/ 未目视** | `crates/shell/tests/ball_state.rs`（`BALL_SIZE_OPTIONS` 三档夹取、`BallAction::Resize` 保中心）、`settings_hot_reload.rs` 的尺寸热生效断言、`SettingsDialog.test.tsx::changes the ball size through update_settings` |
| P2-R10 | 贴边收起 / 悬停展开 / 全屏隐藏 / 引导只出现一次 | **通过（三态）/ 引导项未验证** | `ball_state.rs` 14 个时序用例：12px 收起、13px 不收、150 ms 展开、600 ms 回缩、悬停取消收起、全屏 3 次采样才隐藏、退出全屏回长条、关贴边不收、会话内不改形态。**「引导只出现一次」**由 `ball.guide_shown` 配置承载，无断言也未目视 |
| P2-R11 | 快捷键改键即时生效；占用时拒绝并保留旧键 | **通过** | `crates/app-core/tests/settings_hot_reload.rs` 8 个用例：用独立线程占住组合键模拟冲突（`OccupiedHotkey`），验证「先起新线程、失败则回 `INVALID_INPUT` 且旧键仍有效」、同值不重注册、未启动线程时只写配置；`SettingsDialog.test.tsx::keeps the capture row in edit mode when the backend rejects the hotkey` |
| P2-R11 | 配置只读时保存失败、界面回退 | **未验证（环境限制）** | 未做「config.json 只读」故障注入；`crates/library/tests/config_paths.rs` 只覆盖读失败回退默认值。代码路径 `save_atomic` 失败 → 内存配置不变（`update_settings` 第 3 步），**无测试** |
| P2-R12 | 强杀后重启恢复；孤儿计数与清理；缩略图缺失占位 | **通过（数据层）** | `storage_db.rs`：`orphan_file_is_counted_and_cleaned`、`missing_thumb_is_counted_not_deleted`、`cleanup_counts_only_files_present_at_scan_time`（外部先删一个时按扫描结果计数、二次清理幂等）、`startup_cleanup_reports_missing_thumbs`、`tmp/` 清理由阶段 01 行 13 实测；占位 UI 由 `ItemCard.test.tsx::falls back to placeholder when the thumbnail fails to load, and retries on src change` |
| P2-R12 | **清理不越界**：清理孤儿前后 `visrecall.db` / `-wal` / `-shm` 存在性与 mtime 不变、素材条数不变 | **通过** | `storage_db.rs::cleanup_never_touches_database_or_library_meta`：逐项比对 `visrecall.db`、`visrecall.db-wal`、`library.json` 的存在性/大小/mtime，条数不变，且库根 `junk.bin`、`backups/`、`tmp/` 下的文件都保留 |
| P2-R12 | **提示文案**：设置存储 pane 有「数据库文件请勿单独删除或移动」的说明 | **通过** | `SettingsDialog.test.tsx::mentions that database files must not be moved on their own` |
| P2-R13 | 全部自动化门禁 | **通过** | 见 §3（`cargo fmt` / `clippy -D warnings` / `cargo test --workspace` 473 passed / `pnpm lint` / `typecheck` / `test` 76 passed / `build`） |
| P2-R13 | release 二进制脱离 dev server 运行 | **通过** | 停掉 `pnpm dev` 后直接运行 `target/release/visrecall.exe`（默认 `custom-protocol` 已内嵌 `dist/`）。日志：`opening app core config=C:\Users\Tomwu\AppData\Roaming\VisRecall\config.json library=C:\Users\Tomwu\Documents\VisRecall` → `tray icon created library_ok=true` → `capture runtime started` → `global hotkey registered hotkey=Ctrl+Shift+S` → `cold start to tray: 180ms`；再起一个实例触发单实例回调 → `second instance detected; showing main window` → `main window created`，窗口标题 `目记 VisRecall`、`Responding=True`。内存：托盘驻留 WS 42.2 MB / 私有 33.5 MB（30 线程）；主窗口打开后 WS 51.9 MB / 私有 36.4 MB（50 线程）。WebView2 子进程由 13 → 19（退出后回到 13，无残留）。**截图未采集**：沙箱禁止 `Add-Type`（截屏 API 被安全策略拦截），视觉项由进程/日志 + `%LOCALAPPDATA%\com.visrecall.desktop\EBWebView` 用户数据目录（83 MB）佐证 |
| P2-R14 | 见 §1 | **通过（决策项）** | D2-01：不实现实时模式；见 §1 第 7 行 |

## 3. 自动化门禁输出

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm lint
pnpm typecheck
pnpm test
pnpm build
```

2026-11-06 实跑（提交 `0735669` + 本文件所在提交；Windows 11 22621）：

| 命令 | 结果 | 备注 |
|---|---|---|
| `cargo fmt --all -- --check` | **通过**（退出码 0，无输出） | 首轮曾因新测试文件的换行/折行报 diff，`cargo fmt --all` 后复查无输出 |
| `cargo clippy --workspace --all-targets -- -D warnings` | **通过**（退出码 0） | 只有 `error deleting lock file for incremental compilation session directory ... (os error 5)` 这类**构建缓存 IO 提示**，不是代码告警；出现该提示时偶发「bin test 编译失败」，重跑即通过（见 §4 L7） |
| `cargo test --workspace` | **通过**：**473 passed / 0 failed / 2 ignored** | 36 个 `test result` 行 = 31 个测试二进制 + 5 个 doc-test 套件；2 个 ignored 是 `visrecall-capture` 的既有忽略用例与新增的 `refresh_committed_bindings`（写 `bindings.ts`，默认不跑） |
| `pnpm lint` | **通过**（`eslint .` 无输出） | |
| `pnpm typecheck` | **通过**（`tsc --noEmit` 无输出） | |
| `pnpm test` | **通过**：**12 个文件 / 76 个用例** | `Test Files 12 passed`、`Tests 76 passed`，15.29 s |
| `pnpm build` | **通过**：`102 modules transformed`，1.76 s | `dist/index.html` 3.00 kB（gzip 1.42）、`index-C9FHtuEy.css` 27.04 kB（gzip 5.82）、`index-D77qjW8W.js` 320.19 kB（gzip 98.13） |

`cargo build --release -p visrecall` 产物 20,015,616 字节（19.1 MiB），耗时 2 分 01 秒。

## 4. 已知限制与偏差（本阶段结束时确认）

| 项 | 现状 | 收口阶段 |
|---|---|---|
| 实时模式不实现（D2-01） | 抓帧失败直接 `CAPTURE_FAILED` | 阶段 04 复评 |
| 安装包未产出（D2-02） | release 二进制可运行，无 NSIS | 阶段 04 |
| 迁移需重启、无显式续传（D2-03） | 失败不删源；重试按大小跳过；重启前捕获显式禁用（横幅 + `BUSY`），不是静默失效 | 不排期 |
| 保留策略无 3 天预告（D2-04） | 保存前预估 + 手动清理 | 阶段 04 复评 |
| 迁移期间缩略图可能未进目标库 | 目标库显示占位，可重建（属已知：`thumbs/` 在 `COPY_ORDER` 末位，中途失败时可能只有部分缩略图） | 阶段 03 引入任务队列时评估 |
| 原图协议无缩放参数（D2-08） | 原尺寸返回 | 阶段 04 |
| 标签云点击不筛选 | 阶段 03 接检索 | 阶段 03 |
| 删除文件失败无 gc 重试队列 | 孤儿扫描 + 手动清理兜底 | 阶段 03 |
| 虚拟滚动未实现 | 测试规模 ≤20 条 | 素材超 200 条时 |
| **L1 本轮未构造 20 条手工数据** | §2 中依赖这批数据的项标「未验证（环境限制）」；数据层语义由集成测试覆盖 | 阶段 03 启动前补测 |
| **L2 截图与 GUI 目视全部未做** | 执行环境禁止 `Add-Type`（截屏 API 被安全策略拦截），且无 GUI 操作能力；视觉项以进程指标 + 日志 + 组件测试替代 | 有人值守时补 |
| **L3 `custom-protocol` 设为默认特性的副作用** | `tauri` 的 `dev` cfg 定义为「未启用 `custom-protocol`」，因此**裸 `cargo run` / `cargo build`（debug）也内嵌 `dist/`、不走 `devUrl`**；`dist/` 缺失时 `cargo build -p visrecall` 会因内嵌资源缺失而失败（新克隆仓库需先 `pnpm build`）。**日常开发不受影响**：`pnpm tauri dev` 实际执行 `cargo run --no-default-features`（2026-11-07 `--verbose` 实测日志 `DevCommand (cargo run --no-default-features --color always --)`，同时 `BeforeDevCommand` 起了 Vite），HMR 照旧；另有 `cargo run -p visrecall --no-default-features` 可手工走 devUrl | 阶段 04 若统一 dev 流程时重估 |
| **L4 跨卷迁移未实测** | 迁移只在系统临时目录内（同卷）验证；不同盘符/不同文件系统的 `free_space_bytes` 与复制行为未实测 | 有人值守时补 |
| **L5 原图只读删除** | 断言的是「库记录已删」；Windows 上只读属性不阻止已打开句柄删除，因此不要求文件消失，残留由孤儿扫描兜底 | 不排期 |
| **L6 真实库只有 1 条素材** | `C:\Users\Tomwu\Documents\VisRecall` 仍是阶段 01 遗留的那条 `Code.exe` 截图；本轮 release 冒烟只读取了它，未产生新素材、未做迁移/清空等破坏性操作 | 用户可自行清理 |
| **L7 增量编译缓存报 `os error 5`** | 本机 `target/debug/incremental` 目录下部分 `.lock` 文件删不掉（杀软/索引占用），偶发导致某次 `cargo clippy`/`cargo test` 报「bin test 编译失败」；重跑即通过，与代码无关。门禁结论以重跑后的退出码为准 | 环境问题，不修代码 |

## 5. 结论

**阶段 02 有条件完成（2026-11-06）。**

- **实施完成**：Task 1–21 全部落地，一条任务一个提交；数据层（`visrecall-library`）、编排层（`visrecall-app-core`）、外壳层（`visrecall-shell`）、命令层（`src-tauri`）与前端按 design 分层，未新增未经锁定的依赖（唯一新增是 `app-core` dev-dependencies 里的 `rusqlite`，与 `library` 同版本、lock 中已有）。
- **自动化门禁全绿**：`cargo fmt` / `clippy -D warnings` / `cargo test --workspace`（473 passed）/ `pnpm lint` / `pnpm typecheck` / `pnpm test`（76 passed）/ `pnpm build`；release 二进制脱离 dev server 可运行（冷启动到托盘 180 ms、托盘驻留 WS 42.2 MB）。
- **阶段 02 验收矩阵的覆盖情况**：§2 共 **36 行**——**通过 32 行**（其中 3 行带保留：跨卷迁移未验证、重启后「捕获可用、新素材落新库」未验证、悬浮球引导只出现一次未验证），**未验证（环境限制）4 行**（详情三类各 3 条、详情读取耗时 P95、图片粘贴到 Word/微信/Chrome、配置只读时保存失败）。
- **仍未达成、必须补齐的项**：
  1. 20 条手工数据下的视觉验收（详情三类、列表滚动/翻页、三档尺寸、贴边/全屏、引导只出现一次），以及全部截图证据（L1/L2）；
  2. 阶段 01 门禁里仍然 `未验证（环境限制）` 的 9 项：物理 4K + PresentMon、双屏与混合 DPI、100%/200% 缩放、物理 OLE 四来源、DRM/独占全屏、提权窗口与 UIPI、30 分钟 + ≥20 次真实捕获、重复捕获不去重、滚动帧率量化（§1）；
  3. 详情读取耗时 P95、跨卷迁移（L4）、以及 L3 的取舍（只影响「裸 `cargo run`」，不影响 `pnpm tauri dev`）。
- **建议下一阶段启动前确认**：①L3 是否保留现状（`custom-protocol` 默认开启：release 一条命令可用，代价是裸 `cargo build/run` 必须已有 `dist/`）；②阶段 03 的检索依赖标签/摘要，届时需先补本阶段未做的标签云筛选与 20 条数据验收。

## 6. 增量验收：可编辑 AI 描述与原文（2026-11-15）

> 对应需求：P2-R15 / P2-R16；设计：`design.md` §12；任务：`tasks.md` Task 22–26。
> **功能状态：已实施（2026-11-15，Task 22–26）。** 自动化证据已回填；需要 GUI 的三项（改摘要后检索、改正文后看卡片预览、改 OCR 后看原图文字框）标「环境限制未验证」，附同路径的自动化替代证据。口径与 §2 一致——没有证据不得静默勾选。

### 6.1 验收矩阵

| 需求 | 场景 | 结论 | 证据 |
|---|---|---|---|
| P2-R15 | 改摘要后立即生效，重启后仍在 | **通过（持久化，自动化）** | `content_edit_db::summary_write_sets_user_edited_and_keeps_source`（`summary_source` 不变、保护位为真）；`content_edit_flow::summary_write_emits_the_summary_field` |
| P2-R15 | 摘要加入唯一词「蓝鲸」后检索命中 | **未验证（环境限制：无 GUI）** | 无手工证据。同路径替代：`summary_and_ocr_writes_fire_the_hook` + 既有 `two_stage::note_change_cancels_old_index_job_and_writes_new_note_to_fts`（备注与摘要是同一条钩子） |
| P2-R15 | 2000 / 2001 字符边界（超限被拒且保留旧值） | **通过** | `content_edit_db::summary_accepts_exactly_the_limit_and_rejects_overflow` |
| P2-R15 | 空摘要可补写；清空后回到「待解析」占位 | **通过（后端）** | `content_edit_db::empty_summary_clears_it`（写 NULL）；前端占位由 `summary === null` 分支保证 |
| P2-R15 | 注入一次 VLM 结果后人工摘要未被覆盖 | **通过** | `two_stage::vlm_does_not_overwrite_user_edited_summary`（摘要不变、`summary_source` 不变、颜色照常更新）；`copy_model_fields` 同样加了保护（无独立用例） |
| P2-R15 | 保存失败（库只读）保留文本并可重试 | **通过** | `DetailPanel.test.tsx::keeps the summary text and reports failure when saving fails`（失败态 + 文本保留）；备注侧同一 `commitContent` 语义由 `keeps the note text...` 覆盖 |
| P2-R15 | 切换素材不串台、未保存草稿丢弃 | **通过** | `DetailPanel.test.tsx::resets the content draft when switching to another item` |
| P2-R15 | 保存后继续输入必须回到「未保存」 | **通过** | `DetailPanel.test.tsx::typing after a successful save returns to the unsaved state` |
| P2-R15 | 摘要上限按字符（码点）计，emoji 算 1 个 | **通过** | `DetailPanel.test.tsx::counts the summary limit by code point and blocks overflow`（2001 个 emoji 截到 2000） |
| P2-R16 | 正文编辑写回 `texts/*.txt`，卡片预览与检索立即更新 | **通过（文件与事件）/ 预览未验证（无 GUI）** | `content_edit_db::body_write_replaces_the_file_and_fires_the_hook`；`content_edit_flow::body_write_emits_the_body_field_and_updates_the_file`；`events.test.ts::content update invalidates the detail and refreshes the list only for body text` |
| P2-R16 | 正文 20000 / 20001 边界；`bodyTextTruncated` 时只读 | **通过** | `body_write_rejects_documents_longer_than_the_display_limit`；`DetailPanel.test.tsx::hides the body editor when the original was truncated` |
| P2-R16 | 外部引用素材正文只读（UI 禁用 + 后端 `INVALID_INPUT`） | **通过** | `body_write_rejects_external_reference_items`；`DetailPanel.test.tsx::keeps an external reference body read-only` |
| P2-R16 | `texts/` 只读时保存返回 `IO`，`revision` 未变 | **通过（用同名文件占用父目录模拟写失败）** | `content_edit_db::body_write_failure_does_not_fire_the_hook`（`IO` 且钩子 0 次；只读属性在 Windows 上不阻止已打开句柄，故用目录占位） |
| P2-R16 | OCR 文本编辑不重建文字框，检索按新文本命中 | **通过（框保留）/ 检索未验证（环境限制）** | `content_edit_db::ocr_write_keeps_boxes_and_sets_flag`；检索替代证据同 P2-R15 的钩子用例 |
| P2-R16 | OCR 20000 / 20001 边界；`ocrTextTruncated` 时只读 | **通过** | `ocr_write_rejects_overflow_and_keeps_old_value`（后端限长）；`DetailPanel.test.tsx::hides the ocr editor when the text was truncated`（前端只读 + 提示） |
| P2-R16 | 重新 OCR 不覆盖人工文本与文字框 | **通过** | `two_stage::ocr_does_not_overwrite_user_edited_text_or_boxes`（框为空、文本不变、保护位保留） |
| P2-R16 | 清空正文 / OCR 后的空态与检索结果 | **通过（空态）** | `empty_body_clears_the_file`、`empty_ocr_clears_the_text_but_keeps_boxes` |
| P2-R15/16 | `item_content_updated` 的失效范围：`bodyText` 额外刷新列表，`summary`/`ocrText` 不刷新 | **通过** | `events.test.ts::content update invalidates the detail and refreshes the list only for body text` |
| P2-R15 | 待解析时不提供编辑，复制仍靠右 | **通过** | `DetailPanel.test.tsx::groups the summary copy action on the right and hides editing while unparsed` |
| P2-R15/16 | AI 描述 / 识别文字与备注同构：textarea 常驻、无「编辑」按钮 | **通过** | `DetailPanel.test.tsx::keeps the summary editor always mounted like the note` |
| P2-R15/16 | 摘要 / OCR 失焦：无改动隐藏按钮，有改动保留草稿 | **通过** | `hides the summary actions after blur when nothing changed`、`keeps the summary actions after blur when there are changes`、`hides the ocr actions after blur when nothing changed` |
| P2-R16 | 原文（库内）与备注同构：常驻 textarea、无「编辑」按钮；外部引用与超长只读 | **通过** | `edits the body text of a text item`（直接编辑）、`keeps an empty body editable`、`keeps a missing body editable and disables copy`、`keeps an external reference body read-only`、`hides the body editor when the original was truncated` |
| P2-R16 | 原文与识别到的文字支持一键复制，无文本时禁用 | **通过** | `copies the body text`、`copies the ocr text`、`disables copy when there is no text` |
| P2-R15/16 | 绑定重新生成、`committed_bindings_are_up_to_date` 通过 | **通过** | `refresh_committed_bindings` 执行成功；`bindings.ts` 包含三条新命令与四个新字段；`pnpm typecheck` 通过 |

### 6.2 已知取舍（本次明确接受）

| 项 | 现状 | 依据 |
|---|---|---|
| 来源徽标不表示人工编辑 | `summary_source` 保持 `vlm`/`ocr`/`extractive`/`none`；编辑后徽标不变，界面无「已编辑」标记 | 需求 P2-R15、design D2-12 |
| 无版本历史 / 恢复 AI 原文 | 只保留 `summary_user_edited` / `ocr_user_edited` 保护位，不保存旧值 | requirements §4 |
| OCR 文字框不重算 | 编辑文本后 `ocr_boxes_json` 保持上次识别结果，界面提示可能不一致 | design D2-15 |
| 超长不可编辑 | 正文 / OCR 超过 20000 字符只读，避免把截断内容写回文件 | design D2-14 |
| 外部引用正文只读 | 外部原件始终只读，写请求返回 `INVALID_INPUT` | requirements P2-R16 |
| OCR 置信度保留旧值 | 重新识别只更新 `ocr_engine`；`ocr_confidence` 仍是编辑前那次识别的值 | design §12.7 |
| 保护位存在并发窗口 | handler 读保护位 → 调模型 → 整行写回；模型调用期间的用户编辑会被覆盖（用户侧已改列级写入，handler 侧未加版本号） | design §12.4「并发窗口」 |
| 正文「缺失」与「不可读」共用 `null` | 文件不存在、非 UTF-8、无权限都返回 `body_text = null` 且允许编辑；保存会用新内容覆盖原文件（GBK 老文本可能被整体替换） | design §12.7；建议后续区分 `missing` / `unreadable` |
| 正文编辑不刷新 `char_count` | 只更新 `updated_at`；列表不展示 `charCount`，卡片预览实时读文件 | 低影响，未排期 |

### 6.3 验收命令

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm lint && pnpm typecheck && pnpm test && pnpm build
cargo test -p visrecall --bin visrecall -- --ignored refresh_committed_bindings
```

### 6.4 本轮执行结果（2026-11-15）

| 命令 | 结果 |
|---|---|
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过（0 warning） |
| `cargo test --workspace` | 全部 `test result: ok`；新增 library 17 例、pipeline 2 例、app-core 4 例、IPC/事件稳定性用例若干 |
| `NODE_ENV=test pnpm test` | 24 文件 / **188 passed** |
| `pnpm typecheck` / `pnpm lint` | 通过（0 error / 0 warning） |
| 前端构建 | `npx vite build --emptyOutDir false` 通过；`pnpm build` 的 `tsc --noEmit` 阶段通过，vite 清空 `dist/` 被 IDE 的 safe-delete shim 拦截（环境问题，非代码） |
| `cargo fmt --all -- --check` | 本次改动文件格式化通过；全仓仍有既有基线差异（`app-core/src/app.rs` 未改动的测试行等），未混入格式化 diff |
| `refresh_committed_bindings` | 通过；`src/lib/bindings.ts` 新增 3 条命令与 4 个字段 |

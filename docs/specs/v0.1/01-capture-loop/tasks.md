# VisRecall V0.1 阶段 01：捕获闭环 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完成「触发 → 冻结帧 → 窗口识别 → 选区 → 裁剪落盘 → 入库 → 列表可见」的可日常使用闭环，并具备最小可用的悬浮球与 OLE 拖放。

**Architecture:** 单进程多线程：Tauri 主线程、原生悬浮球线程、原生覆盖层线程、捕获工作线程、Tokio 异步运行时。抓帧用 GDI，覆盖层用 D3D11 + Direct2D，存储用 SQLite WAL，前端用 React 19 + TanStack Query。

**Tech Stack:** Rust 1.93（MSVC）、Tauri 2、React 19、TypeScript、Vite、pnpm、SQLite（rusqlite bundled）、`windows` crate、`tauri-specta`、TanStack Query、Vitest。

## Global Constraints

- 仅 Windows 10 22H2（19045）+ / Windows 11；x64。
- 单进程 `visrecall.exe`；UI 线程不做同步 IO/抓帧/落盘。
- 数据全本地、明文；默认离线可用。
- 检索/素材测试规模 ≤100 条；不做万级压测。
- 捕获入口响应 ≤150ms；选区交互 60fps；提交到列表可见 ≤3s（100 条以内）。
- 托盘常驻 ≤150MB（目标 40–70MB）；空闲 CPU <1%。
- 所有几何使用虚拟屏幕物理像素；Per-Monitor V2 DPI 感知。
- API Key 不进入前端；阶段 01 不接云端。
- 依赖锁 `Cargo.lock` / `pnpm-lock.yaml`；每个任务结束提交一次。
- 阶段 01 缩略图用 PNG（480px 长边），不是 `.webp`；阶段 04 再评估。

---

### Task 1: 工作区与 Tauri/React 骨架

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `.gitignore`
- Create: `crates/domain/{Cargo.toml,src/lib.rs}`
- Create: `crates/library/{Cargo.toml,src/lib.rs}`
- Create: `crates/capture/{Cargo.toml,src/lib.rs}`
- Create: `crates/shell/{Cargo.toml,src/lib.rs}`
- Create: `crates/app-core/{Cargo.toml,src/lib.rs}`
- Create: `src-tauri/{Cargo.toml,build.rs,tauri.conf.json,capabilities/default.json,icons/icon.ico,src/main.rs}`（`icon.ico` 为 Windows tauri-build 必需的最小占位图标，阶段 04 替换）
- Create: `package.json`, `pnpm-workspace.yaml`, `vite.config.ts`, `eslint.config.js`, `tsconfig.json`, `index.html`, `src/main.tsx`, `src/styles/globals.css`, `src/test/setup.ts`

**Interfaces:**
- Consumes: 无
- Produces: 可编译的 Cargo workspace、可启动的 Tauri 窗口、可构建的 React 前端。

- [ ] **Step 1: 创建 Cargo workspace**

```toml
# Cargo.toml
[workspace]
resolver = "2"
members = [
  "crates/domain",
  "crates/library",
  "crates/capture",
  "crates/shell",
  "crates/app-core",
  "src-tauri",
]

[workspace.package]
version = "0.1.0"
edition = "2021"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
uuid = { version = "1", features = ["v4", "serde"] }
tracing = "0.1"
tracing-subscriber = "0.3"
```

```toml
# rust-toolchain.toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
targets = ["x86_64-pc-windows-msvc"]
```

各 crate 先用最小 `lib.rs`：

```rust
// crates/domain/src/lib.rs
pub fn version() -> &'static str { env!("CARGO_PKG_VERSION") }
```

其余 crate 同理；`src-tauri` 的 `main.rs` 先只调用 `tauri::Builder::default().run(tauri::generate_context!()).expect("tauri run");`。

- [ ] **Step 2: 创建 Tauri 配置与 React 前端**

`src-tauri/build.rs`：

```rust
fn main() {
    tauri_build::build();
}
```

`src-tauri/Cargo.toml` 至少包含：

```toml
[package]
name = "visrecall"
version.workspace = true
edition.workspace = true

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = [] }
visrecall-app-core = { path = "../crates/app-core" }
visrecall-domain = { path = "../crates/domain" }
serde.workspace = true
serde_json.workspace = true
```

`src-tauri/tauri.conf.json`：

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "VisRecall",
  "version": "0.1.0",
  "identifier": "com.visrecall.desktop",
  "build": {
    "beforeDevCommand": "pnpm dev",
    "devUrl": "http://localhost:5173",
    "beforeBuildCommand": "pnpm build",
    "frontendDist": "../dist"
  },
  "app": {
    "windows": [
      {
        "label": "main",
        "title": "目记 VisRecall",
        "width": 1100,
        "height": 760,
        "minWidth": 860,
        "minHeight": 560,
        "visible": false
      }
    ],
    "security": { "csp": null }
  },
  "bundle": { "active": false }
}
```

`src-tauri/capabilities/default.json`：

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "default capability",
  "windows": ["main"],
  "permissions": ["core:default"]
}
```

`package.json`：

```json
{
  "name": "visrecall",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc --noEmit && vite build",
    "typecheck": "tsc --noEmit",
    "lint": "eslint .",
    "test": "vitest run",
    "tauri": "tauri"
  },
  "dependencies": {
    "@tanstack/react-query": "^5",
    "@tauri-apps/api": "^2",
    "react": "^19",
    "react-dom": "^19"
  },
  "devDependencies": {
    "@eslint/js": "^9",
    "@tauri-apps/cli": "^2",
    "@testing-library/jest-dom": "^6",
    "@testing-library/react": "^16",
    "@types/react": "^19",
    "@types/react-dom": "^19",
    "@vitejs/plugin-react": "^5",
    "eslint": "^9",
    "eslint-plugin-react-hooks": "^5",
    "eslint-plugin-react-refresh": "^0.4",
    "globals": "^16",
    "jsdom": "^26",
    "typescript": "^5.8",
    "typescript-eslint": "^8",
    "vite": "^7",
    "vitest": "^3"
  }
}
```

`index.html`：

```html
<!doctype html>
<html lang="zh-CN">
  <head><meta charset="UTF-8" /><title>目记 VisRecall</title></head>
  <body><div id="root"></div><script type="module" src="/src/main.tsx"></script></body>
</html>
```

`src/main.tsx`：

```tsx
import { createRoot } from "react-dom/client";
import "./styles/globals.css";

createRoot(document.getElementById("root")!).render(
  <div style={{ padding: 24 }}>VisRecall</div>,
);
```

`vite.config.ts` 同时配置 Vitest：

```ts
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: { port: 5173, strictPort: true },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    globals: true,
  },
});
```

`tsconfig.json`：

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "jsx": "react-jsx",
    "strict": true,
    "noEmit": true,
    "skipLibCheck": true,
    "types": ["vitest/globals"]
  },
  "include": ["src"]
}
```

`eslint.config.js`：

```js
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";

export default tseslint.config(
  { ignores: ["dist", "src/lib/bindings.ts"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: { globals: { ...globals.browser } },
    plugins: { "react-hooks": reactHooks, "react-refresh": reactRefresh },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": "warn",
    },
  },
);
```

`src/test/setup.ts`：

```ts
import "@testing-library/jest-dom/vitest";
```

`vite.config.ts` 固定端口 5173，`src/main.tsx` 渲染一个最小 `App`，先显示「VisRecall」。

- [ ] **Step 3: 安装依赖并生成锁文件**

Run:
```bash
pnpm install
cargo fetch
```

Expected: 生成 `pnpm-lock.yaml`、`Cargo.lock`，无网络错误。

- [ ] **Step 4: 验证骨架**

Run:
```bash
cargo check --workspace
pnpm lint
pnpm typecheck
pnpm build
```

Expected: 三条命令均退出码 0；`dist/` 生成。

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml .gitignore crates src-tauri package.json pnpm-lock.yaml pnpm-workspace.yaml vite.config.ts tsconfig.json index.html src
git commit -m "chore: scaffold tauri and cargo workspace"
```

---

### Task 2: Domain 类型与几何

**Files:**
- Create: `crates/domain/src/{lib.rs,ids.rs,geometry.rs,item.rs,capture.rs,drop.rs,time.rs,error.rs}`
- Test: `crates/domain/tests/geometry.rs`

**Interfaces:**
- Consumes: 无
- Produces: `PhysicalPoint`, `PhysicalRect`, `DisplayInfo`, `RawFrame`, `ForegroundInfo`, `CapturedImage`, `DropContent`, `now_millis()`, `ItemKind`, `ItemStatus`, `NewItem`, `Item`, `AppError`（签名见 `design.md` §4）。

- [ ] **Step 1: 写失败测试**

`crates/domain/tests/geometry.rs`：

```rust
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};

#[test]
fn normalizes_bottom_right_drag() {
    let r = PhysicalRect::from_points(PhysicalPoint { x: 110, y: 220 }, PhysicalPoint { x: 10, y: 20 });
    assert_eq!(r, PhysicalRect::new(10, 20, 100, 200));
}

#[test]
fn intersect_returns_none_for_disjoint() {
    let a = PhysicalRect::new(0, 0, 10, 10);
    let b = PhysicalRect::new(20, 20, 5, 5);
    assert_eq!(a.intersect(&b), None);
}

#[test]
fn intersect_clips_to_overlap() {
    let a = PhysicalRect::new(0, 0, 100, 100);
    let b = PhysicalRect::new(80, 90, 40, 40);
    assert_eq!(a.intersect(&b), Some(PhysicalRect::new(80, 90, 20, 10)));
}

#[test]
fn contains_uses_half_open_edges() {
    let r = PhysicalRect::new(0, 0, 10, 10);
    assert!(r.contains(PhysicalPoint { x: 0, y: 0 }));
    assert!(r.contains(PhysicalPoint { x: 9, y: 9 }));
    assert!(!r.contains(PhysicalPoint { x: 10, y: 9 }));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-domain`
Expected: 编译失败，`PhysicalRect` 未定义。

- [ ] **Step 3: 实现 domain 类型**

`crates/domain/Cargo.toml` 加 `serde`、`thiserror`、`uuid`；dev-dependencies 加 `proptest`。

按 `design.md` §4 实现；`PhysicalRect` 统一使用半开区间 `[x, x+width)`。`from_points` 处理负宽高；`intersect` 返回 `None` 当宽或高为 0。

`ids.rs`：

```rust
pub fn new_item_id() -> String { uuid::Uuid::new_v4().to_string() }
```

`time.rs`：

```rust
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
}
```

`item.rs` 的枚举用 `#[serde(rename_all = "snake_case")]`，与数据库 CHECK 值一致。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p visrecall-domain`
Expected: 4 个测试通过。

- [ ] **Step 5: 增加属性测试**

在 `Cargo.toml` 加 `proptest = "1"`，补：

```rust
proptest! {
    #[test]
    fn intersect_is_within_both(a in -1000i32..1000, b in -1000i32..1000, w in 0u32..500, h in 0u32..500) {
        let r1 = PhysicalRect::new(a, b, w, h);
        let r2 = PhysicalRect::new(a + 10, b + 10, w, h);
        if let Some(i) = r1.intersect(&r2) {
            prop_assert!(i.width <= r1.width && i.width <= r2.width);
            prop_assert!(i.height <= r1.height && i.height <= r2.height);
        }
    }
}
```

Run: `cargo test -p visrecall-domain`
Expected: 属性测试通过。

- [ ] **Step 6: 提交**

```bash
git add crates/domain
git commit -m "feat(domain): add geometry and core item types"
```

---

### Task 3: 配置、库路径与 library.json

**Files:**
- Create: `crates/library/src/{lib.rs,config.rs,paths.rs,files.rs}`
- Test: `crates/library/tests/config_paths.rs`

**Interfaces:**
- Consumes: `visrecall-domain::AppError`
- Produces:
  - `AppPaths::resolve() -> Result<AppPaths, AppError>`
  - `ensure_library_root(&Path) -> Result<LibraryMeta, AppError>`
  - `AppConfig::load_or_default(&Path) -> AppConfig`
  - `AppConfig::save_atomic(&self, &Path) -> Result<(), AppError>`

- [ ] **Step 1: 写失败测试**

```rust
use std::fs;
use visrecall_library::config::AppConfig;
use visrecall_library::paths::{ensure_library_root, LibraryMeta};

#[test]
fn creates_library_metadata_once() {
    let dir = tempfile::tempdir().unwrap();
    let meta = ensure_library_root(dir.path()).unwrap();
    assert_eq!(meta.format_version, 1);
    assert!(dir.path().join("library.json").exists());

    let again = ensure_library_root(dir.path()).unwrap();
    assert_eq!(meta.library_id, again.library_id);
}

#[test]
fn rejects_newer_format_version() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("library.json"),
        r#"{"library_id":"x","format_version":99,"created_at":0}"#,
    ).unwrap();
    let err = ensure_library_root(dir.path()).unwrap_err();
    assert!(format!("{err}").contains("format"));
}

#[test]
fn config_roundtrips_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let cfg = AppConfig::default();
    cfg.save_atomic(&path).unwrap();
    let loaded = AppConfig::load_or_default(&path);
    assert_eq!(loaded.hotkeys.capture, "Ctrl+Shift+S");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-library`
Expected: 编译失败，模块不存在。

- [ ] **Step 3: 实现路径、元数据与配置**

`crates/library/Cargo.toml` 加 `visrecall-domain`、`serde`、`serde_json`、`uuid`；dev-dependencies 加 `tempfile`。

`AppPaths::resolve`：

- `config_dir = %APPDATA%/VisRecall`
- `config_file = config_dir/config.json`
- `default_library_root = %USERPROFILE%/Documents/VisRecall`

`ensure_library_root`：

1. 创建 `root`、`originals/images`、`texts`、`thumbs`、`tmp`、`backups`。
2. 读取 `library.json`；不存在则写入默认 `{library_id: uuid, format_version: 1, created_at: now}`。
3. `format_version > 1` → `AppError::InvalidInput("library format newer than app")`。
4. `library_id` 缺失/为空 → 视为损坏，备份后重建（阶段 01 记录日志）。

`AppConfig` 使用 `serde`，字段见 `design.md` §10.3；`save_atomic` 写 `config.json.tmp` → `sync_all` → `rename`。读取失败时把坏文件重命名为 `config.json.bad-<unix>` 并返回默认值。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p visrecall-library`
Expected: 3 个测试通过。

- [ ] **Step 5: 提交**

```bash
git add crates/library
git commit -m "feat(library): add config and library metadata"
```

---

### Task 4: SQLite 库与 items CRUD

**Files:**
- Create: `crates/library/migrations/0001_core.sql`
- Create: `crates/library/src/{db.rs,migrations.rs,items.rs}`
- Test: `crates/library/tests/items_db.rs`

**Interfaces:**
- Consumes: `LibraryMeta`, `NewItem`, `Item`, `ItemStatus`
- Produces:
  - `Library::open(root: PathBuf) -> Result<Library, AppError>`
  - `Library::root(&self) -> &std::path::Path`
  - `Library::insert_item(&self, new: NewItem) -> Result<Item, AppError>`
  - `Library::list_items(&self, cursor: Option<i64>, limit: u32) -> Result<Vec<Item>, AppError>`
  - `Library::get_item(&self, id: &str) -> Result<Option<Item>, AppError>`
  - `Library::update_status(&self, id: &str, status: ItemStatus, thumb: Option<&str>) -> Result<(), AppError>`
  - `Library::recover_jobs(&self) -> Result<u64, AppError>`
  - `Library::enqueue_job(&self, item_id: &str, stage: &str, revision: i64) -> Result<(), AppError>`
  - `Library::job_state(&self, item_id: &str, stage: &str) -> Result<String, AppError>`
  - `Library::relative_to_root(&self, rel: &str) -> Result<PathBuf, AppError>`

- [ ] **Step 1: 写失败测试**

```rust
use visrecall_domain::item::{ItemKind, ItemStatus, NewItem};
use visrecall_library::Library;

fn new_item(n: i64) -> NewItem {
    NewItem {
        kind: ItemKind::Image,
        captured_at: 1_700_000_000_000 + n,
        source_app: Some("chrome.exe".into()),
        source_title: Some("test".into()),
        source_url: None,
        width: Some(100),
        height: Some(50),
        char_count: None,
        original_path: format!("originals/images/2026/11/{n}.png"),
        content_hash: format!("hash-{n}"),
    }
}

#[test]
fn insert_list_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    let a = lib.insert_item(new_item(1)).unwrap();
    let b = lib.insert_item(new_item(2)).unwrap();
    assert_eq!(a.status, ItemStatus::Parsing);

    let rows = lib.list_items(None, 50).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, b.id);
    assert_eq!(rows[1].id, a.id);
}

#[test]
fn keyset_pagination_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    for n in 0..5 { lib.insert_item(new_item(n)).unwrap(); }

    let page1 = lib.list_items(None, 2).unwrap();
    let cursor = page1.last().unwrap().captured_at;
    let page2 = lib.list_items(Some(cursor), 2).unwrap();
    assert_eq!(page2.len(), 2);
    assert!(page2.iter().all(|i| i.captured_at < cursor));
}

#[test]
fn update_status_sets_thumb() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    let item = lib.insert_item(new_item(1)).unwrap();
    lib.update_status(&item.id, ItemStatus::Ready, Some("thumbs/1.png")).unwrap();
    let got = lib.get_item(&item.id).unwrap().unwrap();
    assert_eq!(got.status, ItemStatus::Ready);
    assert_eq!(got.thumb_path.as_deref(), Some("thumbs/1.png"));
}

#[test]
fn startup_recovery_requeues_running_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    let item = lib.insert_item(new_item(1)).unwrap();
    lib.enqueue_job(&item.id, "thumbnail", 1).unwrap();
    lib.mark_job_running_for_test(&item.id, "thumbnail").unwrap();

    assert_eq!(lib.recover_jobs().unwrap(), 1);
    assert_eq!(lib.job_state(&item.id, "thumbnail").unwrap(), "queued");
}
```

`mark_job_running_for_test` 是 `#[doc(hidden)] pub` 辅助（集成测试看不到 `#[cfg(test)]` 符号），直接执行 `UPDATE jobs SET state='running'`；生产路径通过任务状态机修改。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-library`
Expected: 编译失败，`Library`/`insert_item` 未定义。

- [ ] **Step 3: 实现迁移与 CRUD**

`crates/library/Cargo.toml` 加 `rusqlite = { features = ["bundled"] }`、`visrecall-domain`、`serde_json`；dev-dependencies 加 `tempfile`。版本以 `cargo add` 解析为准。

- `Library` 持有 `root: PathBuf`、`write: Mutex<rusqlite::Connection>`。
- `open`：`ensure_library_root` → 打开 `<root>/visrecall.db` → 在连接上执行 `PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;` → 执行 `CREATE TABLE IF NOT EXISTS schema_migrations (...)` → 查询已应用版本 → 执行未应用的迁移 SQL。迁移文件本身不包含 `PRAGMA journal_mode`。
- `insert_item`：生成 UUID、`created_at/updated_at = now_millis()`、`status = parsing`；事务插入后返回完整 `Item`。
- `list_items`：`WHERE (?1 IS NULL OR captured_at < ?1) ORDER BY captured_at DESC, rowid DESC LIMIT ?2`；`captured_at` 使用 Unix 毫秒，cursor 取上一页最后一项的毫秒值。
- `update_status`：更新 `status`、可选 `thumb_path`、`updated_at`。
- `enqueue_job`：`INSERT OR REPLACE INTO jobs (...)`，唯一键 `(item_id, stage, revision)`；阶段 01 只用于缩略图与恢复测试。
- `recover_jobs`：`UPDATE jobs SET state='queued', updated_at=? WHERE state='running'`，返回受影响行数。
- `job_state`：查询 `(item_id, stage)` 的 state。
- `relative_to_root`：拒绝绝对路径与 `..`，拼接后 `canonicalize` 并校验前缀。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p visrecall-library`
Expected: 7 个测试通过（含 Task 3）。

- [ ] **Step 5: 提交**

```bash
git add crates/library
git commit -m "feat(library): add sqlite items store"
```

---

### Task 5: Tauri 启动、托盘、单实例与主窗口生命周期

**Files:**
- Modify: `src-tauri/Cargo.toml`, `src-tauri/src/main.rs`, `src-tauri/tauri.conf.json`
- Create: `src-tauri/src/{tray.rs,window_lifecycle.rs,ipc.rs,protocol.rs}`
- Create: `crates/app-core/src/{lib.rs,app.rs,events.rs}`
- Create: `src/app/{App.tsx,ErrorBoundary.tsx}`（改造 Task 1 的临时 App）

**Interfaces:**
- Consumes: `Library`, `AppConfig`
- Produces:
  - Tauri managed `AppCore { paths: AppPaths, library: Result<Library, AppError>, config: Mutex<AppConfig>, sink: Arc<dyn EventSink> }`
  - `AppCore::open(paths: AppPaths, sink: Arc<dyn EventSink>) -> AppCore`
  - 命令 `get_app_status()`、`open_library_folder()`、`list_items(cursor, limit)`、`get_item(id)`、`get_settings()`、`update_settings(patch)`
  - `src-tauri/src/ipc.rs` 中的 `ItemDto`、`AppStatus`、`SettingsDto`、`SettingsPatch`、`ApiError`（见 `design.md` §11.1）
  - 事件总线 `AppEvent`（阶段 01 先只定义，后续任务接入）

- [ ] **Step 1: 接入 Tauri 插件与托盘**

`src-tauri/Cargo.toml` 增加：

```toml
tauri = { version = "2", features = ["tray-icon"] }
tauri-plugin-single-instance = "2"
tauri-plugin-opener = "2"
tauri-specta = { version = "2", features = ["derive", "typescript"] }
specta = "2"
specta-typescript = "0.0.9"
```

`main.rs` 骨架：

```rust
fn main() {
    let specta_builder = tauri_specta::Builder::<tauri::Wry>::new()
        .commands(tauri_specta::collect_commands![
            ipc::get_app_status,
            ipc::open_library_folder,
            ipc::list_items,
            ipc::get_item,
            ipc::get_settings,
            ipc::update_settings
        ]);

    #[cfg(debug_assertions)]
    specta_builder
        .export(specta_typescript::Typescript::default(), "../src/lib/bindings.ts")
        .expect("export bindings");

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            window_lifecycle::show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(specta_builder.invoke_handler())
        .setup(|app| {
            let sink = std::sync::Arc::new(ipc::TauriEventSink::new(app.handle().clone()));
            let core = app_core::AppCore::open(app_core::AppPaths::resolve()?, sink);
            app.manage(core);
            tray::create(app)?;
            window_lifecycle::install(app);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri run");
}
```

`TauriEventSink` 在 `ipc.rs` 实现 `app_core::EventSink`：`ItemCreated` → `app.emit("item_created", payload)`，`ItemUpdated` → `item_updated`，`CaptureFailed` → `capture_failed`，`AppRecovered` → `app_recovered`。

命令加 `#[tauri::command] #[specta::specta]`。托盘菜单用 `TrayIconBuilder` + `MenuBuilder`；图标用代码生成的 32×32 RGBA 占位（`tauri::image::Image::new_owned`），不引入图片资源。

- [ ] **Step 2: 实现主窗口 60s 销毁生命周期**

`window_lifecycle.rs`：

```rust
use std::time::Duration;

pub fn install(app: &tauri::App) {
    let handle = app.handle().clone();
    if let Some(win) = app.get_webview_window("main") {
        win.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = win_hide(&handle);
                schedule_destroy(&handle);
            }
        });
    }
}

fn schedule_destroy(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(60));
        if let Some(win) = app.get_webview_window("main") {
            if !win.is_visible().unwrap_or(true) {
                let _ = win.destroy();
            }
        }
    });
}
```

`show_main_window`：窗口存在则 `show` + `set_focus`；已销毁则用 `WebviewWindowBuilder` 重建并加载 Vite URL（开发）或 `index.html`（生产）。阶段 01 允许重建时间为 300–800ms，前端显示骨架屏。

阶段 01 启动不创建主窗口：从 `tauri.conf.json` 移除 `app.windows` 数组；`show_main_window` 在窗口不存在时用 `WebviewWindowBuilder` 创建。冷启动只到托盘可用；首次打开主窗口通过托盘菜单或单实例回调（与架构 §3.3 一致）。

- [ ] **Step 3: 实现命令与启动恢复**

`crates/app-core/Cargo.toml` 加 `visrecall-library`、`visrecall-domain`、`serde`、`crossbeam-channel`。

- `AppCore::open(paths, sink)`：打开 `Library`、加载 `AppConfig`、调用 `Library::recover_jobs()`；恢复条数 >0 时记录日志并发 `app_recovered { count }`。库打开失败不使进程退出：把 `Result<Library, AppError>` 存入 `AppCore`，主窗口显示错误状态，捕获命令返回 `CONFIG_MISSING`。
- 所有命令返回 `Result<_, ApiError>`；在 `ipc.rs` 实现 `impl From<AppError> for ApiError`，把错误变体映射为稳定错误码（`INVALID_INPUT`、`NOT_FOUND`、`STORAGE_FULL`、`INTERNAL` 等）。
- `get_app_status` 返回 `AppStatus { version, library_path, library_ok }`。
- `open_library_folder` 用 `tauri-plugin-opener` 或 `opener` crate 打开库目录；路径来自 `Library`，不接受前端传路径。
- `list_items(cursor, limit)`：调用 `Library::list_items`，映射为 `Vec<ItemDto>`；`thumb_url` 仅在有 `thumb_path` 时拼接。
- `get_item(id)`：找不到返回 `ApiError { code: "NOT_FOUND", .. }`。
- `get_settings`/`update_settings`：只允许修改 `ball_enabled`、`ball_size`、`capture_hotkey`；写配置走原子写入；快捷键变更在阶段 02 才支持热重载，阶段 01 返回 `InvalidInput("restart required")` 并保留旧值。
- 库打开失败：主窗口显示错误状态与库路径，托盘图标进入异常态；不自动新建覆盖。完整的「重试 / 重新定位 / 新建库」恢复页在阶段 02 实现。

- [ ] **Step 4: 手工验证**

Run:
```bash
pnpm tauri dev
```

Expected:
1. 托盘可用；通过托盘「打开主窗口」创建并显示主窗口；关闭后窗口消失，托盘仍在。
2. 再次启动第二个实例，已运行实例的主窗口被唤醒，第二进程退出。
3. 关闭 60s 后任务管理器无 `msedgewebview2.exe` 残留。
4. 托盘「退出」结束进程。
5. 托盘「打开主窗口」能重新创建窗口。
6. `src/lib/bindings.ts` 已生成并纳入版本控制；后续 `pnpm typecheck` 依赖它。

- [ ] **Step 5: 提交**

```bash
git add src-tauri crates/app-core src/app
git commit -m "feat(shell): add tauri tray single-instance and window lifecycle"
```

---

### Task 6: 全局快捷键线程

**Files:**
- Create: `crates/shell/src/{lib.rs,hotkey.rs}`
- Modify: `crates/app-core/src/app.rs`
- Test: `crates/shell/tests/hotkey_parse.rs`

**Interfaces:**
- Consumes: `AppConfig.hotkeys.capture`
- Produces:
  - `HotkeySpec::parse(&str) -> Result<HotkeySpec, AppError>`
  - `HotkeySpec::format(&self) -> String`
  - `HotkeySpec::register_tuple(&self) -> (u32, u32)`
  - `spawn_hotkey_thread(spec, Sender<ShellEvent>) -> Result<HotkeyHandle, AppError>`（等待注册结果后返回，便于 app-core 提示占用）
  - `HotkeyHandle { pub fn stop(&self) }`

- [ ] **Step 1: 写失败测试**

```rust
use visrecall_shell::hotkey::HotkeySpec;

#[test]
fn parses_ctrl_shift_s() {
    let h = HotkeySpec::parse("Ctrl+Shift+S").unwrap();
    assert!(h.ctrl && h.shift && !h.alt && !h.win);
    assert_eq!(h.format(), "Ctrl+Shift+S");
}

#[test]
fn rejects_empty_combo() {
    assert!(HotkeySpec::parse("S").is_err());
}

#[test]
fn rejects_unknown_key() {
    assert!(HotkeySpec::parse("Ctrl+Shift+NoSuchKey").is_err());
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-shell`
Expected: 编译失败，`HotkeySpec` 未定义。

- [ ] **Step 3: 实现解析与注册线程**

`crates/shell/Cargo.toml` 追加 `crossbeam-channel`、`windows`（`Win32_UI_Input_KeyboardAndMouse`、`Win32_UI_WindowsAndMessaging`、`Win32_Foundation`）。

- 支持修饰键 `Ctrl/Control`、`Shift`、`Alt`、`Win/Meta`；主键支持 `A–Z`、`0–9`、`F1–F24`、`Space`、`Esc`、方向键。
- `register_tuple` 映射到 `MOD_CONTROL | MOD_SHIFT | ... | MOD_NOREPEAT` 与 `VK_*`。
- 线程：`RegisterHotKey(None, id, mods, vk)` → `GetMessageW` 循环 → 收到 `WM_HOTKEY` 发送 `ShellEvent::CaptureRequest(CaptureTrigger::Hotkey)` → 退出时 `UnregisterHotKey`。
- 注册失败返回 `AppError::InvalidInput("hotkey occupied")`，由 app-core 发托盘通知。

- [ ] **Step 4: 运行测试并手工验证**

Run:
```bash
cargo test -p visrecall-shell
pnpm tauri dev
```

Expected: 单测 3 个通过；按 `Ctrl+Shift+S` 日志出现 `CaptureRequest`；用其他程序占用该组合键后，启动日志出现 `hotkey occupied`。

- [ ] **Step 5: 提交**

```bash
git add crates/shell crates/app-core
git commit -m "feat(shell): add global hotkey thread"
```

---

### Task 7: 显示器枚举与 DPI 坐标

**Files:**
- Create: `crates/capture/src/{lib.rs,display.rs}`
- Create: `crates/capture/examples/list_displays.rs`
- Modify: `src-tauri/src/main.rs`
- Test: `crates/capture/tests/display_geometry.rs`

**Interfaces:**
- Consumes: `PhysicalRect`, `PhysicalPoint`, `DisplayInfo`
- Produces:
  - `enumerate_displays() -> Result<Vec<DisplayInfo>, AppError>`
  - `display_for_point(displays: &[DisplayInfo], p: PhysicalPoint) -> Option<&DisplayInfo>`
  - `virtual_bounds(displays: &[DisplayInfo]) -> PhysicalRect`
  - `normalize_position(display: &DisplayInfo, point: PhysicalPoint) -> (f32, f32)`（用于悬浮球位置记忆）

- [ ] **Step 1: 写失败测试**

```rust
use visrecall_capture::display::{display_for_point, virtual_bounds};
use visrecall_domain::capture::DisplayInfo;
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};

fn display(id: &str, x: i32, y: i32, w: u32, h: u32) -> DisplayInfo {
    DisplayInfo {
        id: id.into(),
        bounds: PhysicalRect::new(x, y, w, h),
        work_area: PhysicalRect::new(x, y, w, h),
        dpi: 96,
        primary: id == "DISPLAY1",
    }
}

#[test]
fn finds_display_with_negative_origin() {
    let ds = vec![display("DISPLAY1", 0, 0, 1920, 1080), display("DISPLAY2", -2560, 0, 2560, 1440)];
    assert_eq!(display_for_point(&ds, PhysicalPoint { x: -10, y: 10 }).unwrap().id, "DISPLAY2");
}

#[test]
fn virtual_bounds_cover_all_displays() {
    let ds = vec![display("DISPLAY1", 0, 0, 1920, 1080), display("DISPLAY2", -2560, 0, 2560, 1440)];
    assert_eq!(virtual_bounds(&ds), PhysicalRect::new(-2560, 0, 4480, 1440));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-capture`
Expected: 编译失败。

- [ ] **Step 3: 实现 Win32 枚举**

- `EnumDisplayMonitors` + `GetMonitorInfoW` 获取 `rcMonitor`/`rcWork`/`MONITORINFOF_PRIMARY`。
- `GetDpiForMonitor(MDT_EFFECTIVE_DPI)` 获取 DPI（若调用失败，退回 96）。
- 设备名作为 `DisplayInfo.id`。
- 启动时设置 Per-Monitor V2：在 `main.rs` 最早期调用 `SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)`；若已由 manifest 设置则忽略错误。
- 悬浮球位置归一化：`(point.x - bounds.x) / bounds.width`，保存到配置。

- [ ] **Step 4: 运行测试并打印真实显示器**

Run:
```bash
cargo test -p visrecall-capture
cargo run -p visrecall-capture --example list_displays
```

`examples/list_displays.rs` 打印每台显示器的 id、bounds、dpi、primary。Expected: 与 Windows 显示设置一致；150% 屏幕 dpi=144。

- [ ] **Step 5: 提交**

```bash
git add crates/capture
git commit -m "feat(capture): add display enumeration and dpi geometry"
```

---

### Task 8: GDI 抓帧

**Files:**
- Create: `crates/capture/src/{trait_defs.rs,grab_gdi.rs}`
- Test: `crates/capture/tests/grab_pipeline.rs`
- Create: `crates/capture/examples/grab_once.rs`

**Interfaces:**
- Consumes: `DisplayInfo`, `RawFrame`
- Produces:
  - `trait FrameGrabber: Send + Sync { fn grab(&self, displays: &[DisplayInfo]) -> Result<Vec<RawFrame>, AppError>; }`
  - `GdiFrameGrabber`
  - `is_blank_frame(frame: &RawFrame) -> bool`
  - `RawFrame::to_png(&self) -> Result<Vec<u8>, AppError>`（调试/测试用）

- [ ] **Step 1: 写失败测试**

```rust
use visrecall_capture::{grab_gdi::is_blank_frame, trait_defs::RawFrame};

#[test]
fn detects_all_black_frame() {
    let f = RawFrame {
        display_id: "DISPLAY1".into(),
        width: 4,
        height: 2,
        stride: 16,
        bgra: vec![0; 32],
    };
    assert!(is_blank_frame(&f));
}

#[test]
fn non_black_frame_is_not_blank() {
    let mut bgra = vec![0u8; 32];
    bgra[0] = 255;
    let f = RawFrame { display_id: "DISPLAY1".into(), width: 4, height: 2, stride: 16, bgra };
    assert!(!is_blank_frame(&f));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-capture`
Expected: 编译失败。

- [ ] **Step 3: 实现 GDI 抓帧**

`crates/capture/Cargo.toml` 加 `windows` 依赖，至少启用：`Win32_Foundation`、`Win32_Graphics_Gdi`、`Win32_Graphics_Dwm`、`Win32_UI_WindowsAndMessaging`、`Win32_UI_HiDpi`、`Win32_System_Threading`、`Win32_System_ProcessStatus`；另加 `image`（png feature）用于 `RawFrame::to_png`。版本以 `cargo add windows` 实际解析并写入 `Cargo.lock` 为准。

对每个显示器：

1. `GetDC(None)` 获取屏幕 DC。
2. `CreateCompatibleDC` + `CreateDIBSection`（`BITMAPINFOHEADER`，32bpp，`BI_RGB`，top-down：`biHeight = -height`）。
3. `SelectObject`，`BitBlt(mem_dc, 0, 0, w, h, screen_dc, bounds.x, bounds.y, SRCCOPY | CAPTUREBLT)`。
4. 从 DIB bits 复制到 `Vec<u8>`（`stride = width * 4`）。
5. 释放 DC/bitmap，返回 `RawFrame`。
6. `is_blank_frame`：抽样（每 64 像素）检查 BGRA 是否全 0；全黑视为疑似 DRM/失败。

外层顺序处理显示器，每抓完一台立即返回给调用方上传纹理（Task 11），避免同时持有多个 CPU 位图。

- [ ] **Step 4: 运行测试与真实抓帧**

Run:
```bash
cargo test -p visrecall-capture
cargo run -p visrecall-capture --example grab_once -- out.png
```

Expected: 单测通过；`out.png` 是当前主屏截图，尺寸与分辨率一致；拖动窗口时截图内容为抓帧瞬间。

- [ ] **Step 5: 提交**

```bash
git add crates/capture
git commit -m "feat(capture): add gdi frame grabber"
```

---

### Task 9: 窗口枚举与命中链

**Files:**
- Create: `crates/capture/src/window.rs`
- Test: `crates/capture/tests/window_filter.rs`
- Create: `crates/capture/examples/list_windows.rs`

**Interfaces:**
- Consumes: `PhysicalPoint`, `PhysicalRect`, `ForegroundInfo`
- Produces:
  - `trait WindowProbe: Send + Sync { fn foreground(&self) -> Result<ForegroundInfo, AppError>; fn enumerate(&self) -> Result<Vec<WindowInfo>, AppError>; fn hit_chain(&self, point: PhysicalPoint) -> Result<Vec<WindowInfo>, AppError>; }`
  - `Win32WindowProbe`
  - `WindowInfo { hwnd, title, class_name, process_name, bounds, tool_window, cloaked }`
  - `enumerate_windows() -> Result<Vec<WindowInfo>, AppError>`
  - `foreground_info() -> Result<ForegroundInfo, AppError>`
  - `hit_chain(point: PhysicalPoint) -> Result<Vec<WindowInfo>, AppError>`
  - `is_desktop_class(class_name: &str) -> bool`
  - `filter_candidates(windows: &[WindowInfo]) -> Vec<&WindowInfo>`（纯函数，测试用）

- [ ] **Step 1: 写失败测试**

```rust
use visrecall_capture::window::{filter_candidates, is_desktop_class, WindowInfo};
use visrecall_domain::geometry::PhysicalRect;

fn w(title: &str, tool: bool, cloaked: bool) -> WindowInfo {
    WindowInfo {
        hwnd: 1, title: title.into(), class_name: "Chrome_WidgetWin_1".into(),
        process_name: "chrome.exe".into(), bounds: PhysicalRect::new(0, 0, 100, 100),
        tool_window: tool, cloaked,
    }
}

#[test]
fn filters_tool_and_cloaked_windows() {
    let ws = vec![w("visible", false, false), w("tool", true, false), w("cloaked", false, true)];
    let out = filter_candidates(&ws);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].title, "visible");
}

#[test]
fn recognizes_desktop_classes() {
    assert!(is_desktop_class("Progman"));
    assert!(is_desktop_class("WorkerW"));
    assert!(is_desktop_class("Shell_TrayWnd"));
    assert!(!is_desktop_class("Chrome_WidgetWin_1"));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-capture`
Expected: 编译失败。

- [ ] **Step 3: 实现 Win32 枚举与命中**

- `EnumWindows`：过滤 `!IsWindowVisible`、`DWMWA_CLOAKED != 0`、`WS_EX_TOOLWINDOW`、空标题；`DWMWA_EXTENDED_FRAME_BOUNDS` 取边界；`GetClassNameW`、`GetWindowTextW`。
- 进程名：`GetWindowThreadProcessId` → `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` → `QueryFullProcessImageNameW` → 取文件名；失败置空。
- `foreground_info`：`GetForegroundWindow`，组装 `ForegroundInfo`，显示器用 `MonitorFromWindow`。
- `hit_chain`：`WindowFromPoint` → 循环 `RealChildWindowFromPoint` 构建链；对每个 HWND 组装 `WindowInfo`；桌面类返回当前显示器整屏。
- 滚轮层级切换由覆盖层维护索引，阶段 01 先支持链上前 8 个窗口。

- [ ] **Step 4: 运行测试与真实枚举**

Run:
```bash
cargo test -p visrecall-capture
cargo run -p visrecall-capture --example list_windows
```

Expected: 打印当前可见窗口标题、类名、进程名、边界；移动鼠标到 Chrome/资源管理器可看到对应窗口。

- [ ] **Step 5: 提交**

```bash
git add crates/capture
git commit -m "feat(capture): add window enumeration and hit testing"
```

---

### Task 10: D3D11 覆盖层门禁 Spike

**Files:**
- Create: `crates/shell/src/overlay/{mod.rs,d3d.rs,render.rs}`
- Create: `crates/shell/examples/overlay_spike.rs`
- Create/Modify: `docs/specs/v0.1/01-capture-loop/verification.md`

**Interfaces:**
- Consumes: `DisplayInfo`
- Produces:
  - `D3dContext::new() -> Result<D3dContext, AppError>`
  - `D3dContext::create_swap_chain(hwnd, width, height) -> Result<SwapChain, AppError>`
  - `Renderer::draw_test_frame(&mut self, frame: &str) -> Result<(), AppError>`

- [ ] **Step 1: 写 spike 示例**

`crates/shell/Cargo.toml` 加 `windows` 依赖（至少启用：`Win32_Foundation`、`Win32_Graphics_Direct3D`、`Win32_Graphics_Direct3D11`、`Win32_Graphics_Direct2D`、`Win32_Graphics_DirectWrite`、`Win32_Graphics_Dxgi`、`Win32_Graphics_Dxgi_Common`、`Win32_System_Com`、`Win32_UI_WindowsAndMessaging`、`Win32_UI_HiDpi`）、`visrecall-domain`、`visrecall-capture`、`crossbeam-channel`。版本以 `cargo add windows` 实际解析为准。

`overlay_spike.rs`：为每台显示器创建一个 `WS_POPUP | WS_VISIBLE` 窗口，D3D11 设备 + swap chain，D2D 画纯色背景、白色文字「VisRecall Overlay Spike · 已冻结」和一个移动矩形，循环 Present。

```rust
// 关键调用顺序
let device = D3dContext::new()?;
let swap = device.create_swap_chain(hwnd, width, height)?;
let mut renderer = Renderer::new(&device, swap)?;
loop {
    if !pump_messages() { break; }
    renderer.draw_test_frame("已冻结")?;
    renderer.present()?;
}
```

- [ ] **Step 2: 编译并运行**

Run:
```bash
cargo run -p visrecall-shell --example overlay_spike
```

Expected: 每台显示器出现覆盖窗口；文字清晰；无闪烁；关闭窗口后进程退出。

- [ ] **Step 3: 性能门禁**

用 PresentMon 记录 30 秒：

- 4K 单屏拖动测试矩形：平均帧时间 ≤16.7ms，P95 ≤20ms。
- 从进程启动到首帧：记录时间。
- 双 4K：同时打开两个 overlay，确认无 D3D 设备错误。

把结果写入 `verification.md` 的「D3D11 门禁」表。若 2 周内不达标，停止本任务后续，按 `design.md` §7.3 降级并更新 spec。

- [ ] **Step 4: 提交**

```bash
git add crates/shell docs/specs/v0.1/01-capture-loop/verification.md
git commit -m "feat(shell): add d3d11 overlay spike"
```

---

### Task 11: 覆盖层窗口与冻结帧渲染

**Files:**
- Modify: `crates/shell/src/overlay/{mod.rs,d3d.rs,render.rs}`
- Create: `crates/shell/src/overlay/frame.rs`
- Test: `crates/shell/tests/frame_upload.rs`（纯数据/尺寸校验）

**Interfaces:**
- Consumes: `Vec<RawFrame>`, `Vec<DisplayInfo>`, `ForegroundInfo`
- Produces:
  - `OverlayHandle::show(frames: Vec<RawFrame>, displays: Vec<DisplayInfo>, foreground: ForegroundInfo)`
  - `FrozenTexture { texture: ID3D11Texture2D, width: u32, height: u32 }`
  - `upload_frame(device: &D3dContext, frame: &RawFrame) -> Result<FrozenTexture, AppError>`
  - `validate_frame_shape(width: u32, height: u32, stride: usize) -> Result<(), AppError>`

- [ ] **Step 1: 写尺寸/行距测试**

```rust
use visrecall_shell::overlay::frame::validate_frame_shape;

#[test]
fn accepts_expected_stride() {
    assert!(validate_frame_shape(4, 2, 16).is_ok());
}

#[test]
fn rejects_short_buffer() {
    assert!(validate_frame_shape(4, 2, 8).is_err());
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-shell`
Expected: 编译失败。

- [ ] **Step 3: 实现窗口与纹理上传**

- 每个显示器创建覆盖窗口（类名 `VisRecallOverlay`），位置/尺寸用 `DisplayInfo.bounds`；`WS_EX_TOPMOST | WS_EX_TOOLWINDOW`；显示后 `SetForegroundWindow` + `SetFocus`，失败用 `AttachThreadInput`。
- `upload_frame`：校验 `bgra.len() >= stride * height`；创建 `D3D11_USAGE_DEFAULT` 纹理（`DXGI_FORMAT_B8G8R8A8_UNORM`，`BIND_SHADER_RESOURCE`），`UpdateSubresource` 上传；之后释放 `RawFrame.bgra`。
- 冻结纹理转 D2D bitmap，渲染时 `DrawBitmap` 全屏铺满；不缩放（1:1 物理像素）。
- 窗口 `WM_PAINT`/定时器统一由覆盖层线程驱动；阶段 01 用 `InvalidateRect` + 输入事件重绘，不用 60fps 常驻定时器。
- 覆盖层事件循环收到 `OverlayCommand::Cancel` 或提交后销毁/隐藏所有窗口，恢复焦点。

- [ ] **Step 4: 手工验证**

Run:
```bash
cargo run -p visrecall-shell --example overlay_spike
```

Expected: 每台显示器显示一张静态测试图（example 运行时生成，不提交大二进制 fixture），内容与屏幕 1:1；在 150% 屏幕上边界不缩放、不偏移；移动鼠标无重影。

- [ ] **Step 5: 提交**

```bash
git add crates/shell
git commit -m "feat(shell): render frozen frames in overlay windows"
```

---

### Task 12: 选区状态机（纯逻辑）

**Files:**
- Create: `crates/shell/src/selection.rs`
- Test: `crates/shell/tests/selection.rs`

**Interfaces:**
- Consumes: `PhysicalPoint`, `PhysicalRect`, `DisplayInfo`
- Produces:
  - `SelectionState`、`SelectionPhase`、`Handle`、`Modifiers`（见 `design.md` §8）
  - `hit_test_handle(rect, p, dpi) -> Option<Handle>`
  - `drag_rect(handle, start, delta, min) -> PhysicalRect`
  - `snap_rect(rect, candidates, threshold) -> PhysicalRect`
  - `clamp_to_virtual_screen(rect, displays) -> PhysicalRect`

- [ ] **Step 1: 写失败测试**

```rust
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
use visrecall_shell::selection::{drag_rect, Handle, SelectionState};

#[test]
fn move_keeps_size() {
    let start = PhysicalRect::new(10, 10, 100, 100);
    let r = drag_rect(Handle::Move, start, PhysicalPoint { x: 20, y: -5 }, 10);
    assert_eq!(r, PhysicalRect::new(30, 5, 100, 100));
}

#[test]
fn resize_never_goes_below_min() {
    let start = PhysicalRect::new(10, 10, 100, 100);
    let r = drag_rect(Handle::East, start, PhysicalPoint { x: -500, y: 0 }, 10);
    assert!(r.width >= 10);
}

#[test]
fn escape_cancels_selection() {
    let mut s = SelectionState::new();
    s.on_mouse_down(PhysicalPoint { x: 10, y: 10 }, None);
    s.on_mouse_move(PhysicalPoint { x: 110, y: 60 }, Default::default(), &[]);
    s.cancel();
    assert_eq!(s.commit(), None);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-shell`
Expected: 编译失败。

- [ ] **Step 3: 实现状态机**

- `hit_test_handle`：控制点半径 6 物理 px（按 dpi/96 缩放）；内部返回 `Move`；外部 `None`。
- `drag_rect`：按 handle 调整对应边；`Normalize` 后若宽/高 < min，保持 `min` 并向拖拽方向扩展；不允许反向穿过对边。
- `snap_rect`：对四条边分别找最近候选边，距离 ≤ threshold 时对齐；吸附后若宽/高 < min 放弃该方向吸附。
- `clamp_to_virtual_screen`：与 `virtual_bounds` 求交；阶段 01 允许跨屏，不按单屏裁剪。
- `SelectionState::commit`：`rect.width >= 10 && rect.height >= 10` 才返回 `Some`；否则 `None` 并进入 `Cancelled`。

- [ ] **Step 4: 补 proptest 并运行**

```rust
proptest! {
    #[test]
    fn resized_rect_never_negative(w in 0u32..2000, h in 0u32..2000, dx in -2000i32..2000, dy in -2000i32..2000) {
        let start = PhysicalRect::new(100, 100, w.max(10), h.max(10));
        let r = drag_rect(Handle::SouthEast, start, PhysicalPoint { x: dx, y: dy }, 10);
        prop_assert!(r.width >= 10 && r.height >= 10);
    }
}
```

Run: `cargo test -p visrecall-shell`
Expected: 单测与属性测试通过。

- [ ] **Step 5: 提交**

```bash
git add crates/shell
git commit -m "feat(shell): add selection state machine"
```

---

### Task 13: 覆盖层输入与提交回读

**Files:**
- Create: `crates/shell/src/overlay/input.rs`
- Modify: `crates/shell/src/overlay/{mod.rs,render.rs}`
- Test: `crates/shell/tests/stitch.rs`

**Interfaces:**
- Consumes: `SelectionState`, `FrozenTexture`, `DisplayInfo`
- Produces:
  - `OverlayEvent::Committed { image: CapturedImage, global_rect: PhysicalRect, foreground: ForegroundInfo }`
  - `OverlayEvent::Cancelled`
  - `compose_bgra(dst_w: u32, dst_h: u32, regions: &[(u32, u32, u32, u32, Vec<u8>)]) -> Vec<u8>`（参数为 `x, y, w, h, bgra`，纯函数，测试用）

- [ ] **Step 1: 写拼接测试**

```rust
use visrecall_domain::geometry::PhysicalRect;
use visrecall_shell::overlay::input::compose_bgra;

#[test]
fn compose_copies_region_at_offset() {
    let dst_w = 4u32;
    let dst_h = 2u32;
    let src = vec![9u8; 2 * 1 * 4];
    let out = compose_bgra(dst_w, dst_h, &[(1, 1, 2, 1, src.clone())]);
    assert_eq!(out.len(), (dst_w * dst_h * 4) as usize);
    // (1,1) 处应写入 9
    let idx = ((1 * dst_w + 1) * 4) as usize;
    assert_eq!(&out[idx..idx + 4], &[9, 9, 9, 9]);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-shell`
Expected: 编译失败。

- [ ] **Step 3: 实现输入与回读**

- 覆盖窗口 `WM_LBUTTONDOWN`：若 `GetCursorPos` 不在任何显示器内则忽略；`SetCapture`；若命中控制点/内部进入 `Adjusting`/`Move`，否则 `Dragging`。
- `WM_MOUSEMOVE`：`GetCursorPos` 全局坐标 → `SelectionState::on_mouse_move`；候选窗口来自 `hit_chain`；`Ctrl` 关闭吸附。
- `WM_LBUTTONUP`：`ReleaseCapture`，进入可提交态。
- `WM_KEYDOWN`：Esc 取消；Enter 提交；方向键微调（Shift 10px）。
- `WM_RBUTTONDOWN`：取消。
- 滚轮：在 `hit_chain` 中切换高亮窗口；单击采用窗口 `bounds`。
- 提交：对每个显示器求 `selection ∩ display.bounds`；创建 staging texture，`CopyResource` 冻结纹理 → `Map` → 按相交区域裁剪 → `compose_bgra` 拼合为全局选区尺寸。
- 发送 `OverlayEvent::Committed` 后关闭覆盖层并恢复焦点。

- [ ] **Step 4: 手工验证**

Run:
```bash
pnpm tauri dev
```

Expected:
1. 单屏/双屏框选，保存图与选区一致。
2. 150% 屏幕边界不偏移。
3. Esc/右键取消无记录。
4. 方向键 1px、Shift+方向键 10px。
5. 吸附 8px、Ctrl 临时关闭。
6. 提交后焦点回到原窗口。

- [ ] **Step 5: 提交**

```bash
git add crates/shell
git commit -m "feat(shell): add overlay input and crop readback"
```

---

### Task 14: 捕获编排、PNG 落盘与入库

**Files:**
- Create: `crates/app-core/src/capture_flow.rs`
- Modify: `crates/app-core/src/{app.rs,events.rs}`
- Modify: `crates/library/src/files.rs`（可用空间检查）
- Test: `crates/app-core/tests/capture_flow.rs`

**Interfaces:**
- Consumes: `FrameGrabber`, `WindowProbe`, `OverlayHandle`, `Library`
- Produces:
  - `CaptureCoordinator::new(...)`
  - `CaptureCoordinator::start(trigger: CaptureTrigger) -> Result<(), AppError>`
  - `CaptureCoordinator::on_overlay_event(event: OverlayEvent) -> Result<(), AppError>`
  - `persist_capture(image: CapturedImage, foreground: ForegroundInfo, library: &Library) -> Result<Item, AppError>`

- [ ] **Step 1: 写集成测试（假实现）**

```rust
use visrecall_app_core::capture_flow::persist_capture;
use visrecall_domain::capture::{CapturedImage, ForegroundInfo};
use visrecall_domain::item::ItemStatus;
use visrecall_domain::geometry::PhysicalRect;

#[test]
fn persist_writes_png_and_inserts_item() {
    let dir = tempfile::tempdir().unwrap();
    let lib = visrecall_library::Library::open(dir.path().to_path_buf()).unwrap();
    let image = CapturedImage { width: 2, height: 1, bgra: vec![0, 0, 255, 255, 0, 255, 0, 255] };
    let fg = ForegroundInfo {
        hwnd: 0, pid: 1, process_name: "chrome.exe".into(), display_name: "Chrome".into(),
        title: "Test".into(), bounds: PhysicalRect::new(0, 0, 2, 1),
    };

    let item = persist_capture(image, fg, &lib).unwrap();
    assert_eq!(item.status, ItemStatus::Parsing);
    assert!(item.original_path.starts_with("originals/images/"));
    assert!(dir.path().join(&item.original_path).exists());
    assert!(lib.get_item(&item.id).unwrap().is_some());
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-app-core --test capture_flow`
Expected: 编译失败。

- [ ] **Step 3: 实现编排与落盘**

`crates/app-core/Cargo.toml` 追加 `visrecall-library`、`visrecall-capture`、`visrecall-shell`、`image`（png feature）、`sha2`、`tokio`（rt + time）、`crossbeam-channel`、`time`（路径日期格式化，local-offset 失败回退 UTC）。可用空间检查实现为 `crates/library/src/files.rs` 的 `free_space_bytes(path)`（`GetDiskFreeSpaceExW`），由 app-core 调用。

`persist_capture`：

1. `new_item_id()`；创建 `tmp/<id>.tmp`。
2. 将 BGRA 转为 RGBA（交换 B/R 通道），再用 `image::codecs::png::PngEncoder` 编码；`sync_all`。
3. `rename` 到 `originals/images/<YYYY>/<MM>/<id>.png`；创建目录。
4. 计算 `content_hash = sha256(png_bytes)`。
5. `library.insert_item(NewItem { captured_at: now_millis(), original_path: relative, ... })`。
6. 返回 `Item`。

`CaptureCoordinator::start` 按 `design.md` §5 执行；抓帧与覆盖层通过 trait/句柄注入，便于测试。抓帧后若任一显示器 `is_blank_frame` 为真，返回 `AppError::DrmProtected` 并发 `capture_failed { code: "DRM_PROTECTED", message }`，不显示覆盖层。开始抓帧前检查库所在盘可用空间，<2GB 时返回 `AppError::StorageFull` 并发 `capture_failed { code: "STORAGE_FULL", message }`，不进入覆盖层。`on_overlay_event(Committed)` 调用 `persist_capture`，发 `item_created`；本任务结束时素材保持 `parsing`，缩略图在 Task 17 接入。

- [ ] **Step 4: 运行测试与手工闭环**

Run:
```bash
cargo test -p visrecall-app-core --test capture_flow
pnpm tauri dev
```

Expected: 集成测试通过；快捷键 → 框选 → Enter 后，库目录出现 PNG，主窗口列表出现卡片（Task 18 完成后）。

- [ ] **Step 5: 提交**

```bash
git add crates/app-core crates/library
git commit -m "feat(app): wire capture persistence pipeline"
```

---

### Task 15: 悬浮球（阶段 01 最小版）

**Files:**
- Create: `crates/shell/src/ball.rs`
- Modify: `crates/shell/src/lib.rs`, `crates/app-core/src/app.rs`
- Modify: `docs/specs/v0.1/01-capture-loop/design.md`（先补 `ShellEvent::BallMoved` 签名）
- Test: `crates/shell/tests/ball_position.rs`

**Interfaces:**
- Consumes: `DisplayInfo`, `BallConfig`
- Produces:
  - `spawn_ball_thread(config, events: Sender<ShellEvent>) -> BallHandle`
  - `clamp_ball_position(displays, desired: PhysicalPoint, size: u32) -> PhysicalPoint`
  - `normalize_position(display, point) -> (f32, f32)`（与 Task 7 一致）
  - `BallHandle::set_visible(bool)`

- [ ] **Step 1: 写位置测试**

```rust
use visrecall_capture::display::normalize_position;
use visrecall_domain::capture::DisplayInfo;
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};

#[test]
fn normalized_position_roundtrips() {
    let d = DisplayInfo {
        id: "DISPLAY1".into(), bounds: PhysicalRect::new(100, 50, 1000, 800),
        work_area: PhysicalRect::new(100, 50, 1000, 800), dpi: 96, primary: true,
    };
    let (nx, ny) = normalize_position(&d, PhysicalPoint { x: 600, y: 450 });
    assert!((nx - 0.5).abs() < 0.001);
    assert!((ny - 0.5).abs() < 0.001);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-shell`
Expected: 编译失败。

- [ ] **Step 3: 实现分层窗口**

`crates/shell/Cargo.toml` 追加 `Win32_Graphics_GdiPlus`、`Win32_Graphics_Gdi`、`Win32_UI_WindowsAndMessaging`、`Win32_Foundation`、`visrecall-domain`、`visrecall-capture`；悬浮球线程启动时 `GdiplusStartup`，退出时 `GdiplusShutdown`。

- 创建 `WS_POPUP` 窗口，扩展样式 `WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`。
- 32 位 ARGB DIB，GDI+ 绘制圆/阴影/白色图标；`UpdateLayeredWindow` 显示。
- 单击（按下后移动 ≤4px）发送 `ShellEvent::CaptureRequest(CaptureTrigger::BallClick)`。
- 拖动更新窗口位置；松开后发送 `ShellEvent::BallMoved { display_id, nx, ny }`，由 app-core 写入 `config.json`（悬浮球线程不直接写配置）。
- `set_visible(false)` 时 `ShowWindow(SW_HIDE)`；快捷键仍工作。

- [ ] **Step 4: 手工验证**

Run: `pnpm tauri dev`

Expected: 球在桌面置顶、不抢焦点、不进任务栏；单击进入捕获；拖动后重启位置恢复；关闭球后快捷键仍可捕获。

- [ ] **Step 5: 提交**

```bash
git add crates/shell crates/app-core
git commit -m "feat(shell): add minimal floating ball"
```

---

### Task 16: OLE 拖放

**Files:**
- Create: `crates/shell/src/drop_target.rs`
- Create: `crates/app-core/src/import_flow.rs`
- Modify: `crates/app-core/src/app.rs`（路由 `ShellEvent::Drop` 到 ImportFlow）
- Test: `crates/shell/tests/drop_parse.rs`

**Interfaces:**
- Consumes: `DropContent`, `Library`
- Produces:
  - `classify_drop(format_available: &DropFormats, plain: Option<&str>, files: &[PathBuf]) -> Result<DropContent, AppError>`
  - `DropFormats { hdrop: bool, unicode_text: bool, html: bool, dib: bool }`（`#[derive(Default)]`）
  - `ImportFlow::ingest(content: DropContent) -> Result<Vec<Item>, AppError>`

- [ ] **Step 1: 写分类测试**

```rust
use std::path::PathBuf;
use visrecall_shell::drop_target::{classify_drop, DropFormats};

#[test]
fn url_becomes_link() {
    let c = classify_drop(&DropFormats { unicode_text: true, ..Default::default() }, Some("https://example.com"), &[]).unwrap();
    assert!(matches!(c, visrecall_shell::drop_target::DropContent::Url { .. }));
}

#[test]
fn oversized_text_is_rejected() {
    let big = "a".repeat(10_001);
    let err = classify_drop(&DropFormats { unicode_text: true, ..Default::default() }, Some(&big), &[]).unwrap_err();
    assert!(format!("{err}").contains("10000"));
}

#[test]
fn image_files_are_preferred_over_text() {
    let files = vec![PathBuf::from("a.png")];
    let c = classify_drop(&DropFormats { hdrop: true, unicode_text: true, ..Default::default() }, Some("note"), &files).unwrap();
    assert!(matches!(c, visrecall_shell::drop_target::DropContent::Files(_)));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-shell`
Expected: 编译失败。

- [ ] **Step 3: 实现 OLE 目标与导入**

`crates/shell/Cargo.toml` 追加：`Win32_System_Ole`、`Win32_System_Com`、`Win32_UI_Shell`、`Win32_System_DataExchange`、`Win32_System_Memory`、`Win32_Graphics_Gdi` 与 `url`。`crates/app-core/Cargo.toml` 追加 `image`、`sha2`、`encoding_rs`、`reqwest`（blocking + rustls）。

- 悬浮球线程 `OleInitialize` → `RegisterDragDrop(hwnd, drop_target)`；`DragEnter/DragOver` 根据 `IDataObject::QueryGetData` 设置 `pdwEffect` 与提示；同时把球体放大到 1.15× 并显示动作文案，`DragLeave`/`Drop` 后恢复。`Drop` 读取：
  - `CF_HDROP`：`DragQueryFileW` 读取路径。
  - `CF_UNICODETEXT`：`GlobalLock` 读取 UTF-16；`CF_HTML` 读取后剥离标签。
  - `CF_DIB`：`GetDIBits` 读取 `BITMAPINFO` + 像素，转换为 top-down BGRA；若拖拽源提供注册的 PNG 格式，先按 PNG 解码，失败回退 `CF_DIB`。该路径必须实现，不接受延后。
- 投递 `ShellEvent::Drop(DropContent)`；悬浮球线程不做落盘。
- `ImportFlow`：
  - `DropContent::ImageDib`：按图片素材入库，写 `originals/images/YYYY/MM/<id>.png`，来源记录为当前前台应用。
  - 文件：按扩展名分图片/文本；图片 ≤20MB，文本 ≤500KB；长边 >8000px 用 `image` 解码缩放；批量 ≤20。
  - 文本：UTF-8 优先，`encoding_rs` 回退 GB18030；写入 `texts/<id>.txt`；标题取文件名或首个 `# ` 标题。
  - URL：`reqwest` 阻塞客户端超时 5s 抓 `<title>`/站点名；失败标题=URL；阶段 01 允许先用 `reqwest::blocking` 在工作线程。
  - 混合：图片优先，文本写入 `note`。
  - 文本/链接素材写入成功后标记 `ready`（无缩略图）；图片素材保持 `parsing`，由 Task 17 生成缩略图后转 `ready`。
  - 返回插入的 `Item` 列表并发送事件。

- [ ] **Step 4: 运行测试与四类来源手工验证**

Run:
```bash
cargo test -p visrecall-shell -p visrecall-app-core
pnpm tauri dev
```

Expected:
- Chrome 选中文字拖入 → 文本素材；
- Word 拖文本 → 文本素材；
- 资源管理器拖 PNG/JPG → 图片素材；
- 微信拖图片 → 图片素材；
- 拖文件夹/PDF/压缩包 → 拒绝提示；
- 拖拽中 Esc → 无记录。

- [ ] **Step 5: 提交**

```bash
git add crates/shell crates/app-core tests
git commit -m "feat(shell): add ole drop target and import flow"
```

---

### Task 17: 缩略图与 `visrecall://thumb` 协议

**Files:**
- Create: `crates/app-core/src/thumbnail.rs`
- Create: `src-tauri/src/protocol.rs`
- Modify: `src-tauri/src/main.rs`
- Modify: `crates/app-core/src/import_flow.rs`（导入图片也生成缩略图）
- Test: `crates/app-core/tests/thumbnail.rs`

**Interfaces:**
- Consumes: `Item`, `Library`
- Produces:
  - `generate_thumbnail(src: &Path, dst: &Path, max_edge: u32) -> Result<(), AppError>`
  - 协议 `visrecall://thumb/<item_id>`

- [ ] **Step 1: 写缩略图测试**

```rust
use image::{Rgba, RgbaImage};
use visrecall_app_core::thumbnail::generate_thumbnail;

#[test]
fn thumbnail_long_edge_is_480() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.png");
    let dst = dir.path().join("dst.png");
    let img = RgbaImage::from_pixel(1000, 500, Rgba([255, 0, 0, 255]));
    img.save(&src).unwrap();

    generate_thumbnail(&src, &dst, 480).unwrap();
    let out = image::open(&dst).unwrap();
    assert_eq!(out.width(), 480);
    assert_eq!(out.height(), 240);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-app-core --test thumbnail`
Expected: 编译失败。

- [ ] **Step 3: 实现缩略图与协议**

- `generate_thumbnail`：`image::open` → `thumbnail(480, 480)`（保持比例）→ 保存 PNG；失败返回 `AppError::Io`。
- 在 `CaptureCoordinator::on_overlay_event` 中，`persist_capture` 成功后用 `spawn_blocking` 调用 `generate_thumbnail`；成功后更新 DB 与事件。
- 捕获提交后：写 `thumbs/<id>.png`，`Library::update_status(id, Ready, Some(relative))`，发 `item_updated`。
- 缩略图失败：`Library::update_status(id, Failed, None)`，发 `item_updated`；原图与卡片占位保留，不删除素材。
- 导入图片同样在后台生成 `thumbs/<id>.png` 并转 `ready`；失败转 `failed`，与捕获路径一致。
- 协议：在 `src-tauri/src/main.rs` 的 `tauri::Builder` 上调用 `register_uri_scheme_protocol("visrecall", ...)`；解析 URI path，只接受 `/thumb/<uuid>`；查 DB → `relative_to_root` → 读文件 → 返回 `200` + `image/png`；非法 ID/路径/缺文件返回 `404` 或 `400`。
- Windows 上 Tauri 自定义协议实际映射为 `http://visrecall.localhost/thumb/<id>`；前端使用 `@tauri-apps/api/core` 的 `convertFileSrc` 转换逻辑地址，测试中 mock 为原样返回。
- 不暴露绝对路径；不读取库外文件。

- [ ] **Step 4: 运行测试并手工验证**

Run:
```bash
cargo test -p visrecall-app-core --test thumbnail
pnpm tauri dev
```

Expected: 捕获后卡片显示缩略图；浏览器/前端访问 `visrecall://thumb/<不存在ID>` 返回 404；路径穿越请求被拒绝。

- [ ] **Step 5: 提交**

```bash
git add crates/app-core src-tauri
git commit -m "feat(app): add thumbnails and custom protocol"
```

---

### Task 18: React 素材列表与事件刷新

**Files:**
- Modify: `src/app/App.tsx`
- Create: `src/features/library/{ItemGrid.tsx,ItemCard.tsx,EmptyState.tsx,useItems.ts}`
- Create: `src/lib/{queryClient.ts,events.ts,format.ts}`
- Test: `src/features/library/ItemCard.test.tsx`

**Interfaces:**
- Consumes: 生成的 `bindings.ts`、`item_created`/`item_updated`
- Produces: 可浏览的素材网格；新素材自动出现。

- [ ] **Step 1: 写前端测试**

```tsx
import { render, screen } from "@testing-library/react";
import { ItemCard, type ItemCardData } from "./ItemCard";

const base: ItemCardData = {
  id: "1",
  kind: "image",
  status: "parsing",
  capturedAt: 1_700_000_000_000,
  sourceApp: "chrome.exe",
  sourceTitle: "Test",
  thumbUrl: null,
};

test("shows parsing status", () => {
  render(<ItemCard item={base} />);
  expect(screen.getByText("解析中")).toBeInTheDocument();
});

test("shows ready thumbnail", () => {
  render(<ItemCard item={{ ...base, id: "2", status: "ready", thumbUrl: "visrecall://thumb/2" }} />);
  expect(screen.getByRole("img")).toHaveAttribute("src", "visrecall://thumb/2");
});
```

- [ ] **Step 2: 运行测试确认失败**

Run: `pnpm test`
Expected: 失败，`ItemCard` 不存在。

- [ ] **Step 3: 实现查询与列表**

- `queryClient`：默认 `staleTime: 5_000`、`refetchOnWindowFocus: false`。
- `useItems`：`useInfiniteQuery` 调用生成的 `list_items`（导出名以 `bindings.ts` 为准）；`getNextPageParam` 使用最后一项 `capturedAt`。
- `events.ts`：监听 `item_created`/`item_updated`，`queryClient.invalidateQueries({ queryKey: ["items"] })`。
- `ItemGrid`：CSS grid，`auto-fill minmax(180px, 1fr)`；阶段 01 先渲染全部已加载项（≤100 条），不做复杂虚拟化；加载更多按钮。
- `ItemCardData`：`{ id, kind, status, capturedAt, sourceApp, sourceTitle, thumbUrl }`；`useItems` 把生成的 `ItemDto` 映射为 `ItemCardData`，并用 `convertFileSrc` 转换 `thumbUrl`（测试中 mock 为原样返回）。
- `ItemCard`：缩略图/占位、状态徽标（解析中/正常/待解析/失败）、来源应用、相对时间（毫秒时间戳格式化为本地时间）。
- `EmptyState`：提示「按 Ctrl+Shift+S 或点击悬浮球开始捕获」。

- [ ] **Step 4: 运行测试与手工验证**

Run:
```bash
pnpm test
pnpm typecheck
pnpm tauri dev
```

Expected: 测试通过；捕获后卡片自动出现在顶部；状态从「解析中」变「正常」；100 条以内滚动正常。

- [ ] **Step 5: 提交**

```bash
git add src
git commit -m "feat(ui): add item grid and event refresh"
```

---

### Task 19: 阶段 01 退出验证

**Files:**
- Create/Modify: `docs/specs/v0.1/01-capture-loop/verification.md`
- Modify: `docs/specs/v0.1/README.md`（状态更新）

**Interfaces:**
- Consumes: 全部阶段 01 产物
- Produces: 可复现的验收记录与已知限制清单。

- [ ] **Step 1: 运行自动化验证**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm lint
pnpm typecheck
pnpm test
pnpm build
```

Expected: 全部通过。若 clippy 有警告，修复后重跑。

- [ ] **Step 2: 手工验收矩阵**

在目标 Windows 机器上逐项记录：

| 场景 | 步骤 | 期望 | 结果 |
|---|---|---|---|
| 单屏 100% | 快捷键 → 框选 → Enter | 图与选区一致，卡片出现 | |
| 单屏 150% | 同上 | 无偏移 | |
| 双屏同 DPI | 跨屏框选 | 图覆盖跨屏区域 | |
| 双屏混合 DPI | 150% + 100% | 边界不偏移 | |
| 窗口点击 | 鼠标悬停 Chrome/Word | 高亮与标签正确 | |
| 滚轮层级 | 浏览器内容区滚轮 | 高亮下钻/上移 | |
| 取消 | Esc/右键 | 无记录 | |
| 选区过小 | 拖动 5×5 → Enter | 提示「选区过小」，无记录 | |
| 拖放四类 | Chrome 文字/Word 文本/资源管理器图片/微信图片 | 正确入库 | |
| 超限拖放 | 10001 字符、21 个文件、21MB 图 | 明确拒绝，不截断 | |
| 托盘 | 关闭主窗口 60s | WebView2 退出，内存 ≤150MB | |
| 二次实例 | 再次启动 exe | 唤醒已有主窗口 | |
| 强杀恢复 | 解析中 `taskkill /F` | 重启无损坏；临时文件清理 | |
| 空库 | 首次启动 | 空状态引导 | |
| 100 条列表 | 导入/捕获 100 条 | 滚动流畅，排序正确 | |

- [ ] **Step 3: 性能记录**

记录并写入 `verification.md`：

- 捕获入口响应（快捷键 → 覆盖层首帧）。
- 4K 双屏抓帧工作集增量。
- 托盘空闲内存与空闲 CPU。
- 提交到卡片可见时间（100 条库）。
- D3D11 门禁结果（Task 10）。

- [ ] **Step 4: 记录已知限制**

至少记录：DRM/独占全屏、提权窗口、跨屏间隙填充、缩略图 PNG（非 WebP）、CF_DIB 拖放是否完成、ARM64 未验证。

- [ ] **Step 5: 更新阶段状态并提交**

把 `docs/specs/v0.1/README.md` 中阶段 01 状态改为「已完成（日期）」，并写下一阶段入口条件。

```bash
git add docs/specs/v0.1
git commit -m "docs(spec): record phase 01 verification results"
```

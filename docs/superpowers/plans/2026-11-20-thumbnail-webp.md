# 缩略图改为有损 WebP 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 把 480px 缩略图从 PNG 改为有损 WebP（质量 85），并在启动时把库内已有 PNG 缩略图在后台就地迁移为 WebP。

**架构：** `app-core::thumbnail` 负责编码（`webp` crate / libwebp），新文件统一命名为 `thumbs/<id>.webp`；资源协议改为按 `thumb_path` 扩展名返回 MIME，旧 `.png` 缩略图继续可读；`AppCore` 绑定完成后起一个后台线程，逐条把旧 PNG 解码、重编码为 WebP、更新 `thumb_path` 并删除旧文件。前端 URL 仍是 `visrecall://thumb/<id>?rev=N`，零改动。

**技术栈：** Rust、`webp = 0.3`（libwebp 绑定，`default-features = false`）、`image 0.25`、rusqlite、Tauri v2（资源协议）、Cargo Test。

**Spec：** `docs/VisRecall-技术架构设计.md` §5（库目录 `thumbs/<uuid>.webp`）、`docs/VisRecall-阶段三-AI与检索架构设计.md`（缩略图 480px WebP）；本计划的编码参数与迁移策略来自 2026-11-20 的设计讨论（实测：本机 18 张缩略图 PNG 平均 253KB → WebP q85 平均 30KB）。

## 全局约束

- 平台仅 Windows（WebView2 原生支持 WebP），不引入跨平台兼容层。
- 缩略图长边保持 `THUMB_MAX_EDGE = 480`，不做分辨率调整；编码质量固定 `85.0`。
- 有损编码会丢弃全不透明图的 alpha 通道，透明缩略图仍保留 alpha（libwebp 自动判断）。
- 原图（`originals/`）格式不变，仍是 PNG；`capture_flow::encode_rgba_png` 不得改动或删除。
- 前端零改动：`src/lib/format.ts`、`ItemCard`、`DetailPanel`、`ImageOverlay` 都不需要修改。
- 旧 `.png` 缩略图必须继续可被协议服务（按扩展名返回 `image/png`），迁移失败不能导致已有缩略图不可用。
- 迁移任务幂等：中断后再次启动会继续剩余项；单条失败只 warn、保留旧文件，绝不阻止启动。
- 新增代码注释使用简体中文；`src/lib/bindings.ts` 不涉及，无需重新生成。
- 不修改历史任务文档（`docs/specs/v0.1/*/tasks.md`），只更新设计文档。

---

### 任务 1：缩略图编码切换为有损 WebP

**文件：**
- 修改：`crates/app-core/Cargo.toml:21`（新增 `webp` 依赖）
- 修改：`crates/app-core/src/thumbnail.rs:1-102`（编码实现与路径常量）
- 修改：`crates/app-core/src/thumbnail.rs:487-755`（内联测试中的路径断言）
- 修改：`crates/app-core/tests/thumbnail.rs:1-113`（集成测试路径与格式断言）
- 修改：`crates/app-core/src/capture_flow.rs:4,1359-1374`、`crates/app-core/src/import_flow.rs:113,1083-1104`（测试断言与注释）

**接口：**
- 新增 `pub const THUMB_WEBP_QUALITY: f32 = 85.0;`
- `pub fn thumb_relative_path(item_id: &str) -> String` 返回 `thumbs/<id>.webp`
- `pub fn generate_thumbnail(src: &Path, dst: &Path, max_edge: u32) -> Result<(), AppError>` 签名不变，输出改为 WebP 字节
- 内部新增 `fn encode_webp_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, AppError>`

- [ ] **Step 1: 添加依赖**

```toml
# crates/app-core/Cargo.toml，放在 `encoding_rs` 之后
webp = { version = "0.3", default-features = false }
```

说明：关闭默认的 `img` feature，避免 `webp` crate 再拉一份 `image`；只用 `Encoder::from_rgba`。

- [ ] **Step 2: 写失败的测试（集成测试）**

修改 `crates/app-core/tests/thumbnail.rs`：把 `image_with_thumb` 中的 `rel_thumb` 改为 `.webp`，新增格式断言。

```rust
// crates/app-core/tests/thumbnail.rs
fn image_with_thumb(dir: &Path) -> (Library, String) {
    let library = open_library(dir);
    let id = "thumb-item".to_string();
    let original = format!("originals/images/2026/01/{id}.png");
    let rel_thumb = format!("thumbs/{id}.webp");

    library
        .insert_item_with_id(
            id.clone(),
            NewItem {
                kind: ItemKind::Image,
                captured_at: 1,
                source_app: None,
                source_title: None,
                source_url: None,
                width: Some(1000),
                height: Some(500),
                char_count: None,
                original_path: original.clone(),
                content_hash: "hash".into(),
            },
        )
        .unwrap();

    let source = dir.join("source.png");
    RgbaImage::from_pixel(1000, 500, Rgba([9, 9, 9, 255]))
        .save(&source)
        .unwrap();
    fs::create_dir_all(dir.join(&original).parent().unwrap()).unwrap();
    fs::copy(&source, dir.join(&original)).unwrap();

    generate_thumbnail(&dir.join(&original), &dir.join(&rel_thumb), THUMB_MAX_EDGE).unwrap();
    library
        .update_status(&id, ItemStatus::Ready, Some(&rel_thumb))
        .unwrap();

    (library, id)
}

#[test]
fn thumbnail_output_is_webp() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.png");
    let dst = dir.path().join("thumb.webp");
    RgbaImage::from_pixel(1000, 500, Rgba([255, 0, 0, 255]))
        .save(&src)
        .unwrap();

    generate_thumbnail(&src, &dst, THUMB_MAX_EDGE).unwrap();

    let bytes = fs::read(&dst).unwrap();
    assert_eq!(
        image::guess_format(&bytes).unwrap(),
        image::ImageFormat::WebP
    );
}
```

同时把文件里所有 `.png` 目标路径改为 `.webp`：
- `thumbnail_creates_the_parent_directory` 中 `thumbs/nested/out.png` → `thumbs/nested/out.webp`
- `thumbnail_long_edge_is_480` 中 `dst.png` → `dst.webp`
- `load_thumb_bytes_returns_the_png` 中 `format!("thumbs/{id}.png")` → `.webp`，函数改名为 `load_thumb_bytes_returns_the_webp`
- `load_thumb_bytes_reports_missing_files` 中删除路径同步改 `.webp`

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p visrecall-app-core --test thumbnail`
Expected: FAIL —— 找不到 `thumbs/<id>.webp`，且 `guess_format` 返回 `Png`。

- [ ] **Step 4: 实现编码切换**

`crates/app-core/src/thumbnail.rs`：

```rust
// 模块文档第 1 行改为：
//! 缩略图（design §10.2）：长边 480px 的有损 WebP `thumbs/<id>.webp`，捕获/导入提交后异步生成。

use webp::Encoder;

/// 缩略图编码质量（有损 WebP，0–100）。q85 在 228px 卡片上文字伪影不可见。
pub const THUMB_WEBP_QUALITY: f32 = 85.0;

/// 素材缩略图的库内相对路径。
pub fn thumb_relative_path(item_id: &str) -> String {
    format!("{THUMB_DIR}/{item_id}.webp")
}

/// 把 RGBA 像素编码为有损 WebP；全不透明图会自动省略 alpha 通道。
fn encode_webp_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, AppError> {
    let encoder = Encoder::from_rgba(rgba, width, height);
    Ok(encoder.encode(THUMB_WEBP_QUALITY).to_vec())
}
```

`generate_thumbnail` 末尾替换（去掉 `encode_rgba_png` 调用与该 import）：

```rust
    let rgba = scaled.to_rgba8();
    let (width, height) = rgba.dimensions();
    let webp = encode_webp_rgba(width, height, rgba.as_raw())?;

    // 临时文件与目标同目录：缩略图不参与 `tmp/` 的启动清理策略。
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let tmp_id = dst
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "thumbnail".to_string());
    write_bytes_atomic_in(parent, &tmp_id, dst, &webp)
```

删除 `use crate::capture_flow::{encode_rgba_png, write_bytes_atomic_in};` 中的 `encode_rgba_png`，import 行改为：

```rust
use crate::capture_flow::write_bytes_atomic_in;
```

（`encode_rgba_png` 仍被 `capture_flow.rs` 用于原图落盘，定义保留。）

- [ ] **Step 5: 更新内联测试路径**

`crates/app-core/src/thumbnail.rs` 的 `mod tests`：

```rust
    // generate_for_item_marks_the_item_ready_with_a_thumb
    assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/item-1.webp"));
    assert!(library.root().join("thumbs/item-1.webp").exists());

    // small_images_are_not_upscaled
    let dst = dir.path().join("thumbs/small.webp");

    // spawn_thumbnail_generates_in_the_background
    assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/item-bg.webp"));
```

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test -p visrecall-app-core --lib thumbnail && cargo test -p visrecall-app-core --test thumbnail`
Expected: PASS（含 `thumbnail_output_is_webp`）。

- [ ] **Step 7: 更新流程测试的扩展名断言**

`crates/app-core/src/capture_flow.rs`（第 4 行注释与第 1373 行左右）：

```rust
//! 入库并发 `item_created` 之后，缩略图（`thumbs/<id>.webp`）派发到有界缩略图 worker

        // 缩略图线程在 5s 内把素材转 `ready` 并写入 `thumbs/<id>.webp`。
        let thumb = format!("thumbs/{id}.webp");
```

`crates/app-core/src/import_flow.rs`（第 113 行注释与第 1099 行左右）：

```rust
    /// 发完 `item_created` 再派发图片缩略图任务（有界 worker 队列，`thumbs/<id>.webp`），

    /// Task 17：图片导入后 `emit_created` 派发缩略图，最终转 `ready` 并带 `thumbs/<id>.webp`。
        let thumb = format!("thumbs/{id}.webp");
```

- [ ] **Step 8: 运行整个 app-core 测试**

Run: `cargo test -p visrecall-app-core`
Expected: PASS（缩略图、捕获、导入相关用例全部通过）。

- [ ] **Step 9: 提交**

```bash
git add crates/app-core/Cargo.toml crates/app-core/src/thumbnail.rs crates/app-core/tests/thumbnail.rs crates/app-core/src/capture_flow.rs crates/app-core/src/import_flow.rs Cargo.lock
git commit -m "feat(thumbnail): 缩略图编码改为有损 WebP q85"
```

---

### 任务 2：资源协议按扩展名返回 MIME

**文件：**
- 修改：`crates/app-core/src/thumbnail.rs:104-130`（`load_thumb_bytes` 返回值）
- 修改：`src-tauri/src/protocol.rs:158-161`（缩略图分支）
- 修改：`src-tauri/src/protocol.rs:346-376`（协议测试）
- 修改：`crates/app-core/tests/thumbnail.rs:102-113`（读取断言）

**接口：**
- `pub fn load_thumb_bytes(library: &Library, item_id: &str) -> Result<(Vec<u8>, &'static str), AppError>`
  - 新签名与 `load_original_bytes` 同构：`(bytes, mime)`，MIME 由 `thumb_path` 扩展名推导（`.webp` → `image/webp`，旧 `.png` → `image/png`）。

- [ ] **Step 1: 写失败的测试（协议）**

在 `src-tauri/src/protocol.rs` 的测试模块新增 helper 与用例：

```rust
    fn library_with_webp_thumb(dir: &Path) -> (Library, String) {
        let library = Library::open(dir.to_path_buf()).expect("open temp library");
        let id = "webp-1".to_string();
        library
            .insert_item_with_id(
                id.clone(),
                NewItem {
                    kind: ItemKind::Image,
                    captured_at: 1,
                    source_app: None,
                    source_title: None,
                    source_url: None,
                    width: Some(10),
                    height: Some(10),
                    char_count: None,
                    original_path: "originals/images/2026/01/webp-1.png".into(),
                    content_hash: "hash".into(),
                },
            )
            .unwrap();
        let relative = format!("thumbs/{id}.webp");
        fs::write(dir.join(&relative), b"RIFF\x1a\x00\x00\x00WEBPVP8 thumb").unwrap();
        library
            .update_status(&id, ItemStatus::Ready, Some(&relative))
            .unwrap();
        (library, id)
    }

    #[test]
    fn returns_webp_with_cache_header() {
        let dir = tempfile::tempdir().unwrap();
        let (library, id) = library_with_webp_thumb(dir.path());

        let response = asset_response(Some(&library), &Method::GET, &format!("/thumb/{id}"));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "image/webp");
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
    }
```

同时把原有用例 `returns_png_with_cache_header` 改名为 `returns_legacy_png_with_cache_header`（断言不变，保留旧格式兼容覆盖），`head_returns_headers_without_a_body` 保持 PNG 用例即可。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall protocol`
Expected: FAIL —— WebP 分支仍返回 `image/png`。

- [ ] **Step 3: 实现 MIME 推导**

`crates/app-core/src/thumbnail.rs`：

```rust
/// 读取素材缩略图的字节与 MIME（资源协议用）。
///
/// MIME 按 `thumb_path` 扩展名推导：新缩略图是 `.webp`，旧库里的 `.png` 继续可用。
/// 其余错误语义不变：素材不存在 / 没有 `thumb_path` / 文件已丢失 → [`AppError::NotFound`]；
/// `thumb_path` 不在 `thumbs/` 下或不是库内相对路径 → [`AppError::InvalidInput`]。
pub fn load_thumb_bytes(
    library: &Library,
    item_id: &str,
) -> Result<(Vec<u8>, &'static str), AppError> {
    let item = library.get_item(item_id)?.ok_or(AppError::NotFound)?;
    let thumb = item.thumb_path.ok_or(AppError::NotFound)?;

    // 纵深防御：协议只服务缩略图，库里的路径再坏也不能读到其他库内文件。
    if !is_thumb_relative(&thumb) {
        return Err(AppError::InvalidInput(format!(
            "thumb path is not under {THUMB_DIR}/: {thumb}"
        )));
    }

    let path = library.relative_to_root(&thumb).map_err(|err| match err {
        AppError::InvalidInput(detail) => AppError::InvalidInput(detail),
        // 路径解析失败（文件被删/目录被移走）对调用方等价于「没有缩略图」。
        _ => AppError::NotFound,
    })?;

    let bytes = fs::read(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => AppError::NotFound,
        _ => AppError::Io(format!("read {thumb}: {err}")),
    })?;
    Ok((bytes, image_mime(&thumb)))
}
```

`src-tauri/src/protocol.rs`：

```rust
    let loaded = match kind {
        AssetKind::Thumb => load_thumb_bytes(library, item_id),
        AssetKind::Original => load_original_bytes(library, item_id),
    };
```

- [ ] **Step 4: 更新 app-core 集成测试的返回值**

`crates/app-core/tests/thumbnail.rs`：

```rust
#[test]
fn load_thumb_bytes_returns_the_webp() {
    let dir = tempfile::tempdir().unwrap();
    let (library, id) = image_with_thumb(dir.path());

    let (bytes, mime) = load_thumb_bytes(&library, &id).expect("read thumbnail");
    assert_eq!(mime, "image/webp");
    assert_eq!(
        bytes,
        fs::read(dir.path().join(format!("thumbs/{id}.webp"))).unwrap()
    );
}
```

其余 `load_thumb_bytes` 用例都走 `unwrap_err()`，错误分支类型不变，不需要修改。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p visrecall-app-core --test thumbnail && cargo test -p visrecall protocol`
Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/app-core/src/thumbnail.rs crates/app-core/tests/thumbnail.rs src-tauri/src/protocol.rs
git commit -m "feat(protocol): 缩略图按扩展名返回 WebP/PNG MIME"
```

---

### 任务 3：库层查询旧格式缩略图

**文件：**
- 修改：`crates/library/src/items.rs`（新增 `LegacyThumb` 与 `legacy_png_thumbs`）
- 测试：`crates/library/src/items.rs` 的 `mod tests`

**接口：**
- 新增 `pub struct LegacyThumb { pub id: String, pub thumb_path: String, pub revision: i64 }`
- 新增 `pub fn legacy_png_thumbs(&self) -> Result<Vec<LegacyThumb>, AppError>`

- [ ] **Step 1: 写失败的测试**

在 `crates/library/src/items.rs` 的 `mod tests` 末尾新增：

```rust
    #[test]
    fn legacy_png_thumbs_lists_only_png_paths() {
        let (_dir, lib) = test_library();
        let png = lib.insert_item(image_item()).unwrap();
        lib.update_status(&png.id, ItemStatus::Ready, Some("thumbs/png.png"))
            .unwrap();
        let _webp = lib.insert_item(image_item()).unwrap();
        lib.update_status(&_webp.id, ItemStatus::Ready, Some("thumbs/webp.webp"))
            .unwrap();
        let _none = lib.insert_item(image_item()).unwrap();

        let legacy = lib.legacy_png_thumbs().unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].id, png.id);
        assert_eq!(legacy[0].thumb_path, "thumbs/png.png");
        assert_eq!(legacy[0].revision, 0);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-library legacy_png_thumbs`
Expected: FAIL —— 方法不存在。

- [ ] **Step 3: 实现查询**

`crates/library/src/items.rs`（`MAX_FILTERED_IDS` 之后加类型，`impl Library` 内加方法）：

```rust
/// 一条待迁移的旧格式缩略图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyThumb {
    pub id: String,
    pub thumb_path: String,
    pub revision: i64,
}

    /// 列出 `thumb_path` 以 `.png` 结尾的素材（缩略图格式迁移用）。
    ///
    /// 用 `substr` 而不是 `LIKE`：`LIKE '%.png'` 里的 `.` 会匹配任意字符。
    /// 按新到旧排序，迁移中途中断时用户最可能看到的素材优先完成。
    pub fn legacy_png_thumbs(&self) -> Result<Vec<LegacyThumb>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, thumb_path, revision FROM items
                 WHERE thumb_path IS NOT NULL AND substr(thumb_path, -4) = '.png'
                 ORDER BY captured_at DESC, rowid DESC",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(LegacyThumb {
                    id: row.get(0)?,
                    thumb_path: row.get(1)?,
                    revision: row.get(2)?,
                })
            })
            .map_err(db_error)?;

        let mut thumbs = Vec::new();
        for row in rows {
            thumbs.push(row.map_err(db_error)?);
        }
        Ok(thumbs)
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p visrecall-library legacy_png_thumbs`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/library/src/items.rs
git commit -m "feat(library): 增加旧 PNG 缩略图枚举查询"
```

---

### 任务 4：启动后台迁移旧 PNG 缩略图

**文件：**
- 修改：`crates/app-core/src/thumbnail.rs`（新增迁移函数与测试）
- 修改：`crates/app-core/src/app.rs:484`（启动挂载）
- 修改：`crates/app-core/src/app.rs:921-951`（新增 `spawn_legacy_thumb_migration`）

**接口：**
- `pub fn spawn_legacy_thumb_migration(library: Arc<Library>) -> Result<(), AppError>`：起命名为 `visrecall-thumb-migrate` 的后台线程，处理完即退出。
- `pub(crate) fn migrate_legacy_thumbs(library: &Library)`：同步入口（测试直接调用）。
- `pub(crate) fn migrate_legacy_thumb(library: &Library, entry: &LegacyThumb) -> Result<(), AppError>`：单条迁移；`Ok(())` 包含「已迁移」与「安全跳过」两种情况。
- 消费任务 1 的 `encode_webp_rgba` / `thumb_relative_path`、任务 2 起不变的 `update_status`、任务 3 的 `Library::legacy_png_thumbs`。

- [ ] **Step 1: 写失败的测试**

在 `crates/app-core/src/thumbnail.rs` 的 `mod tests` 末尾新增：

```rust
    /// 旧 PNG 缩略图迁移为 WebP：换库路径、删旧文件、输出可解码。
    #[test]
    fn migrates_legacy_png_thumbs_to_webp() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let id = "legacy-1";
        insert_image(&library, id, "originals/images/2026/01/legacy-1.png");
        let png_rel = format!("thumbs/{id}.png");
        let png_path = library.root().join(&png_rel);
        fs::create_dir_all(png_path.parent().unwrap()).unwrap();
        write_png(&png_path, 100, 50);
        library
            .update_status(id, ItemStatus::Ready, Some(&png_rel))
            .unwrap();

        migrate_legacy_thumbs(&library);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/legacy-1.webp"));
        assert!(!png_path.exists());
        let bytes = fs::read(library.root().join("thumbs/legacy-1.webp")).unwrap();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::WebP
        );
        assert!(library.legacy_png_thumbs().unwrap().is_empty());
    }

    /// 迁移不得把 `failed` 素材改成 `ready`。
    #[test]
    fn migration_keeps_failed_status() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let id = "legacy-failed";
        insert_image(&library, id, "originals/images/2026/01/legacy-failed.png");
        let png_rel = format!("thumbs/{id}.png");
        let png_path = library.root().join(&png_rel);
        fs::create_dir_all(png_path.parent().unwrap()).unwrap();
        write_png(&png_path, 100, 50);
        library
            .update_status(id, ItemStatus::Failed, Some(&png_rel))
            .unwrap();

        migrate_legacy_thumbs(&library);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.status, ItemStatus::Failed);
        assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/legacy-failed.webp"));
    }

    /// 旧文件缺失：跳过，不改库，也不产生新文件。
    #[test]
    fn migration_skips_missing_legacy_files() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let id = "legacy-missing";
        insert_image(&library, id, "originals/images/2026/01/legacy-missing.png");
        let png_rel = format!("thumbs/{id}.png");
        library
            .update_status(id, ItemStatus::Ready, Some(&png_rel))
            .unwrap();

        migrate_legacy_thumbs(&library);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.thumb_path.as_deref(), Some(png_rel.as_str()));
        assert!(!library.root().join("thumbs/legacy-missing.webp").exists());
    }

    /// 扫描后、逐条处理前素材被更新（thumb_path 变了）：该条跳过，旧文件保留。
    #[test]
    fn migration_skips_entries_whose_thumb_changed_after_scan() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let id = "legacy-changed";
        insert_image(&library, id, "originals/images/2026/01/legacy-changed.png");
        let png_rel = format!("thumbs/{id}.png");
        let png_path = library.root().join(&png_rel);
        fs::create_dir_all(png_path.parent().unwrap()).unwrap();
        write_png(&png_path, 100, 50);
        library
            .update_status(id, ItemStatus::Ready, Some(&png_rel))
            .unwrap();
        let entries = library.legacy_png_thumbs().unwrap();
        assert_eq!(entries.len(), 1);

        library
            .update_status(id, ItemStatus::Ready, Some("thumbs/other.webp"))
            .unwrap();
        migrate_legacy_thumb(&library, &entries[0]).unwrap();

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/other.webp"));
        assert!(png_path.exists());
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-app-core --lib thumbnail`
Expected: FAIL —— `migrate_legacy_thumbs` / `migrate_legacy_thumb` 不存在。

- [ ] **Step 3: 实现迁移**

`crates/app-core/src/thumbnail.rs` 顶部导入补充：

```rust
use visrecall_library::items::LegacyThumb;
```

新增函数（放在 `build_thumbnail` 之后）：

```rust
/// 起一个后台线程，把旧 PNG 缩略图迁移为 WebP；处理完即退出。
///
/// 失败只返回线程创建错误（调用方记录 warn），迁移条目本身的失败在内部计数。
pub fn spawn_legacy_thumb_migration(library: Arc<Library>) -> Result<(), AppError> {
    thread::Builder::new()
        .name("visrecall-thumb-migrate".into())
        .spawn(move || migrate_legacy_thumbs(&library))
        .map(|_| ())
        .map_err(|err| AppError::Internal(format!("spawn thumbnail migration thread: {err}")))
}

/// 同步迁移全部旧 PNG 缩略图（测试与排障直接调用；生产走 [`spawn_legacy_thumb_migration`]）。
pub(crate) fn migrate_legacy_thumbs(library: &Library) {
    let entries = match library.legacy_png_thumbs() {
        Ok(entries) => entries,
        Err(err) => {
            warn!(error = %err, "failed to list legacy png thumbs");
            return;
        }
    };
    if entries.is_empty() {
        return;
    }
    let total = entries.len();
    let mut failed = 0;
    for entry in &entries {
        if let Err(err) = migrate_legacy_thumb(library, entry) {
            failed += 1;
            warn!(
                id = %entry.id,
                path = %entry.thumb_path,
                error = %err,
                "legacy thumb migration failed"
            );
        }
    }
    info!(total, failed, "legacy png thumb migration finished");
}

/// 迁移一条：解码 PNG → 编码 WebP → 原子写 → 换库路径 → 删旧文件。
///
/// `Ok(())` 包含「已迁移」与「安全跳过」（素材已变、旧文件缺失）。
/// 已存在 `.webp`（上次崩在写库之前）时跳过重编码，直接补写库路径。
pub(crate) fn migrate_legacy_thumb(
    library: &Library,
    entry: &LegacyThumb,
) -> Result<(), AppError> {
    // 复核：迁移期间素材可能被刷新/重新链接，旧任务不得覆盖新状态。
    let Some(item) = library.get_item(&entry.id)? else {
        return Ok(());
    };
    if item.revision != entry.revision
        || item.thumb_path.as_deref() != Some(entry.thumb_path.as_str())
    {
        return Ok(());
    }

    let png_path = library.relative_to_root(&entry.thumb_path)?;
    if !png_path.is_file() {
        return Ok(());
    }

    let webp_relative = thumb_relative_path(&entry.id);
    let webp_path = library.root().join(&webp_relative);
    if !webp_path.is_file() {
        let image = image::open(&png_path)
            .map_err(|err| AppError::Io(format!("open {}: {err}", png_path.display())))?;
        let rgba = image.to_rgba8();
        let (width, height) = rgba.dimensions();
        let webp = encode_webp_rgba(width, height, rgba.as_raw())?;
        let parent = webp_path.parent().unwrap_or_else(|| Path::new("."));
        write_bytes_atomic_in(parent, &entry.id, &webp_path, &webp)?;
    }

    // 保留原状态（迁移不是修复失败素材的时机）。
    library.update_status(&entry.id, item.status, Some(&webp_relative))?;

    // 库已指向新文件；删除失败只留下一个孤儿 png，不影响正确性。
    if let Err(err) = fs::remove_file(&png_path) {
        warn!(path = %entry.thumb_path, error = %err, "failed to remove migrated png thumb");
    }
    Ok(())
}
```

- [ ] **Step 4: 运行迁移测试确认通过**

Run: `cargo test -p visrecall-app-core --lib thumbnail`
Expected: PASS（4 个迁移用例 + 原有缩略图用例）。

- [ ] **Step 5: 挂载到启动流程**

`crates/app-core/src/app.rs:484` 附近：

```rust
        tracing::info!("capture runtime started");
        // 保留策略在绑定启动之后跑一次：清理失败只记 warn，不阻止启动（P2-R08）。
        self.spawn_startup_prune();
        // 旧 PNG 缩略图在后台迁移为 WebP：幂等，失败不影响启动。
        self.spawn_legacy_thumb_migration();
        Ok(())
```

`spawn_startup_prune` 之后新增方法：

```rust
    /// 缩略图格式迁移：绑定启动完成后在后台线程跑一次（幂等，失败只记 warn）。
    fn spawn_legacy_thumb_migration(&self) {
        let library = match &self.library {
            Ok(library) => Arc::clone(library),
            Err(_) => return,
        };
        if let Err(err) = crate::thumbnail::spawn_legacy_thumb_migration(library) {
            tracing::warn!(error = %err, "failed to spawn legacy thumbnail migration");
        }
    }
```

- [ ] **Step 6: 编译并运行相关测试**

Run: `cargo test -p visrecall-app-core`
Expected: PASS（含启动编排相关用例）。

- [ ] **Step 7: 提交**

```bash
git add crates/app-core/src/thumbnail.rs crates/app-core/src/app.rs
git commit -m "feat(thumbnail): 启动时后台迁移旧 PNG 缩略图"
```

---

### 任务 5：设计文档口径与全量验证

**文件：**
- 修改：`docs/specs/v0.1/01-capture-loop/design.md:18,576`
- 修改：`docs/specs/v0.1/02-library-detail/design.md:46`

**接口：** 无新接口，只统一文档口径。

- [ ] **Step 1: 更新设计文档口径**

`docs/specs/v0.1/01-capture-loop/design.md:18`：

```markdown
| 缩略图 | 有损 WebP q85（480px 长边，libwebp）；阶段 01 曾用 PNG，2026-11-20 完成切换与存量迁移 | `image` 解码 + `webp` 编码 |
```

`docs/specs/v0.1/01-capture-loop/design.md:576`：

```markdown
- 缩略图：`thumbs/<uuid>.webp`，长边 480px、有损 WebP q85；生成失败不阻塞列表。
```

`docs/specs/v0.1/02-library-detail/design.md:46`：

```markdown
| 缩略图生成（WebP q85，480px，有界并发 2 worker） | `crates/app-core/src/thumbnail.rs` |
```

- [ ] **Step 2: 全量验证**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets && cargo test --workspace`
Expected: 全部 PASS，无新 clippy 警告。

Run: `pnpm typecheck && pnpm lint && pnpm test`
Expected: 通过（前端零改动，用于确认没有意外影响）。

- [ ] **Step 3: 手工验证**

1. `pnpm tauri dev`，截取一张屏幕截图，确认 `库根/thumbs/<id>.webp` 生成、列表与详情正常显示。
2. 放一个旧 `.png` 缩略图 + 对应库记录（或直接使用升级前的库），重启应用，确认启动日志出现 `legacy png thumb migration finished`，`.webp` 生成、`.png` 消失、界面正常。
3. 删除某条素材的 `.webp` 后重启，确认卡片显示占位、不报错（`missing_thumbs` 统计 +1）。

- [ ] **Step 4: 提交**

```bash
git add docs/specs/v0.1/01-capture-loop/design.md docs/specs/v0.1/02-library-detail/design.md
git commit -m "docs(thumbnail): 同步 WebP 缩略图口径"
```

---

## 附：实测参考（2026-11-20，本机库 18 张缩略图）

| 方案 | 平均体积 | 相对 PNG |
|---|---|---|
| PNG（现状） | 253KB | 100% |
| WebP 无损 | 159KB | 63% |
| **WebP q85** | **30KB** | **12%** |
| WebP q80 | 25KB | 10% |
| JPEG q80 | 30KB | 12% |

结论：有损 WebP q85 是体积/画质平衡点；内存峰值只多几 MB（编码期间的临时缓冲，受 2 个 worker 约束），稳态 RSS 不变。

//! 缩略图（design §10.2）：长边 480px 的 `thumbs/<id>.png`，捕获/导入提交后异步生成。
//!
//! 线程模型：生成放在独立线程（[`spawn_thumbnail`]，线程名 `visrecall-thumb`），
//! 捕获循环与导入工作线程只负责派发，PNG 解码/缩放不会占住它们。`Library` 的写连接
//! 本身串行化，多个缩略图线程与命令层互不冲突。
//!
//! 失败语义（design §12）：缩略图失败**不删除原图**，只把素材转 `failed` 并发
//! `item_updated`，卡片保留占位（重试入口属于阶段 02）。

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use image::imageops::FilterType;
use tracing::{info, warn};
use visrecall_domain::item::{ItemKind, ItemStatus};
use visrecall_domain::AppError;
use visrecall_library::Library;

use crate::capture_flow::{encode_rgba_png, write_bytes_atomic_in};
use crate::events::{AppEvent, EventSink, ItemUpdatedEvent};

/// 缩略图长边上限（design §10.2）。
pub const THUMB_MAX_EDGE: u32 = 480;

/// 缩略图目录（相对库根）；`Library` 打开时已建好。
pub const THUMB_DIR: &str = "thumbs";

/// 素材缩略图的库内相对路径。
pub fn thumb_relative_path(item_id: &str) -> String {
    format!("{THUMB_DIR}/{item_id}.png")
}

/// 把 `src` 缩到长边 `max_edge` 并保存为 PNG 到 `dst`。
///
/// - 保持宽高比；原图长边不超过 `max_edge` 时不放大（缩略图只缩小）。
/// - 建父目录；先写同目录临时文件再 `rename`，失败清理临时文件（design §10.2）。
/// - 读图/编码/写盘失败统一返回 [`AppError::Io`]。
pub fn generate_thumbnail(src: &Path, dst: &Path, max_edge: u32) -> Result<(), AppError> {
    if max_edge == 0 {
        return Err(AppError::InvalidInput(
            "thumbnail max edge must be positive".into(),
        ));
    }

    let image =
        image::open(src).map_err(|err| AppError::Io(format!("open {}: {err}", src.display())))?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return Err(AppError::Io(format!(
            "image {} has an empty size: {width}x{height}",
            src.display()
        )));
    }
    let scaled = if width.max(height) > max_edge {
        image.resize(max_edge, max_edge, FilterType::Lanczos3)
    } else {
        image
    };

    let rgba = scaled.to_rgba8();
    let (width, height) = rgba.dimensions();
    let png = encode_rgba_png(width, height, rgba.as_raw())?;

    // 临时文件与目标同目录：缩略图不参与 `tmp/` 的启动清理策略。
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let tmp_id = dst
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "thumbnail".to_string());
    write_bytes_atomic_in(parent, &tmp_id, dst, &png)
}

/// 读取素材缩略图的字节（资源协议用）。
///
/// 素材不存在、没有 `thumb_path`、或缩略图文件已丢失都返回 [`AppError::NotFound`]；
/// `thumb_path` 不是库内相对路径（库被手工改坏）返回 [`AppError::InvalidInput`]，
/// 从不读取库根目录之外的文件。
pub fn load_thumb_bytes(library: &Library, item_id: &str) -> Result<Vec<u8>, AppError> {
    let item = library.get_item(item_id)?.ok_or(AppError::NotFound)?;
    let thumb = item.thumb_path.ok_or(AppError::NotFound)?;

    let path = library.relative_to_root(&thumb).map_err(|err| match err {
        AppError::InvalidInput(detail) => AppError::InvalidInput(detail),
        // 路径解析失败（文件被删/目录被移走）对调用方等价于「没有缩略图」。
        _ => AppError::NotFound,
    })?;

    fs::read(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => AppError::NotFound,
        _ => AppError::Io(format!("read {thumb}: {err}")),
    })
}

/// 异步生成缩略图：派发到独立的 `visrecall-thumb` 线程，立即返回。
///
/// 调用方（捕获循环/导入工作线程）不等待；完成或失败通过 [`generate_for_item`]
/// 更新库状态并发 `item_updated`。
pub fn spawn_thumbnail(
    library: Arc<Library>,
    sink: Arc<dyn EventSink>,
    item_id: String,
) -> Result<(), AppError> {
    thread::Builder::new()
        .name("visrecall-thumb".into())
        .spawn(move || generate_for_item(&library, sink.as_ref(), &item_id))
        .map(|_| ())
        .map_err(|err| AppError::Internal(format!("spawn thumbnail worker: {err}")))
}

/// 同步生成一张缩略图并写回状态（[`spawn_thumbnail`] 的线程体，测试直接调用）。
///
/// - 成功：`thumbs/<id>.png` + `update_status(Ready, Some(相对路径))` +
///   `item_updated { fields: ["status", "thumbPath"] }`。
/// - 失败：`update_status(Failed, None)` + `item_updated { fields: ["status"] }`；
///   原图保留，卡片显示占位。
pub fn generate_for_item(library: &Library, sink: &dyn EventSink, item_id: &str) {
    match build_thumbnail(library, item_id) {
        Ok(relative) => match library.update_status(item_id, ItemStatus::Ready, Some(&relative)) {
            Ok(()) => {
                info!(id = item_id, path = %relative, "thumbnail generated");
                emit_updated(sink, item_id, &["status", "thumbPath"]);
            }
            Err(err) => warn!(id = item_id, error = %err, "failed to store the thumbnail path"),
        },
        Err(err) => {
            warn!(id = item_id, error = %err, "thumbnail generation failed; keeping the original");
            match library.update_status(item_id, ItemStatus::Failed, None) {
                Ok(()) => emit_updated(sink, item_id, &["status"]),
                Err(db_err) => {
                    warn!(id = item_id, error = %db_err, "failed to mark the item failed")
                }
            }
        }
    }
}

/// 读原图 → 写 `thumbs/<id>.png`，返回缩略图的库内相对路径。
fn build_thumbnail(library: &Library, item_id: &str) -> Result<String, AppError> {
    let item = library.get_item(item_id)?.ok_or(AppError::NotFound)?;
    if item.kind != ItemKind::Image || item.original_path.is_empty() {
        return Err(AppError::InvalidInput(format!(
            "item {item_id} has no image to thumbnail"
        )));
    }

    let source = library.relative_to_root(&item.original_path)?;
    let relative = thumb_relative_path(item_id);
    let target = library.root().join(&relative);
    generate_thumbnail(&source, &target, THUMB_MAX_EDGE)?;
    Ok(relative)
}

fn emit_updated(sink: &dyn EventSink, item_id: &str, fields: &[&str]) {
    sink.emit(AppEvent::ItemUpdated(ItemUpdatedEvent {
        id: item_id.to_string(),
        fields: fields.iter().map(|field| (*field).to_string()).collect(),
    }));
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};
    use visrecall_domain::item::NewItem;

    use super::*;
    use crate::events::RecordingSink;

    fn write_png(path: &Path, width: u32, height: u32) {
        RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]))
            .save(path)
            .unwrap();
    }

    /// 把原图放进库里 `relative` 对应的位置（按需建 `originals/images/YYYY/MM`）。
    fn place_original(library: &Library, source: &Path, relative: &str) {
        let target = library.root().join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(source, target).unwrap();
    }

    fn insert_image(library: &Library, id: &str, relative: &str) {
        library
            .insert_item_with_id(
                id.to_string(),
                NewItem {
                    kind: ItemKind::Image,
                    captured_at: 1,
                    source_app: None,
                    source_title: None,
                    source_url: None,
                    width: Some(10),
                    height: Some(10),
                    char_count: None,
                    original_path: relative.to_string(),
                    content_hash: "hash".into(),
                },
            )
            .unwrap();
    }

    fn updated_events(sink: &RecordingSink) -> Vec<ItemUpdatedEvent> {
        sink.events()
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::ItemUpdated(payload) => Some(payload),
                _ => None,
            })
            .collect()
    }

    /// 同步路径的完整链路：缩略图落盘 → `ready` + `thumb_path` → `item_updated`。
    #[test]
    fn generate_for_item_marks_the_item_ready_with_a_thumb() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let original = dir.path().join("original.png");
        write_png(&original, 1000, 500);

        let id = "item-1";
        let relative = "originals/images/2026/01/item-1.png";
        insert_image(&library, id, relative);
        place_original(&library, &original, relative);
        let sink = RecordingSink::default();

        generate_for_item(&library, &sink, id);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.status, ItemStatus::Ready);
        assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/item-1.png"));
        assert!(library.root().join("thumbs/item-1.png").exists());
        assert!(!library.root().join("thumbs/item-1.tmp").exists());

        let updates = updated_events(&sink);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].id, id);
        assert_eq!(updates[0].fields, vec!["status", "thumbPath"]);
    }

    /// 缩略图失败（原图丢失）：素材转 `failed`，不发 `thumbPath` 字段。
    #[test]
    fn generate_for_item_marks_failed_when_the_original_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let id = "item-broken";
        insert_image(&library, id, "originals/images/2026/01/missing.png");
        let sink = RecordingSink::default();

        generate_for_item(&library, &sink, id);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.status, ItemStatus::Failed);
        assert_eq!(stored.thumb_path, None);

        let updates = updated_events(&sink);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].fields, vec!["status"]);
    }

    /// 小图不放大：长边小于上限时保持原尺寸。
    #[test]
    fn small_images_are_not_upscaled() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("small.png");
        let dst = dir.path().join("thumbs/small.png");
        write_png(&src, 40, 20);

        generate_thumbnail(&src, &dst, THUMB_MAX_EDGE).unwrap();

        let out = image::open(&dst).unwrap();
        assert_eq!((out.width(), out.height()), (40, 20));
    }

    /// `spawn_thumbnail` 派发后线程体完成生成（不阻塞调用方）。
    #[test]
    fn spawn_thumbnail_generates_in_the_background() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().join("library")).unwrap());
        let original = dir.path().join("original.png");
        write_png(&original, 1000, 500);
        let id = "item-bg";
        let relative = "originals/images/2026/01/item-bg.png";
        insert_image(&library, id, relative);
        place_original(&library, &original, relative);

        let sink = Arc::new(RecordingSink::default());
        spawn_thumbnail(
            Arc::clone(&library),
            Arc::clone(&sink) as Arc<dyn EventSink>,
            id.to_string(),
        )
        .expect("spawn thumbnail worker");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let stored = library.get_item(id).unwrap().unwrap();
            if stored.status == ItemStatus::Ready {
                assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/item-bg.png"));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "thumbnail was not generated within 5s"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!updated_events(&sink).is_empty());
    }
}

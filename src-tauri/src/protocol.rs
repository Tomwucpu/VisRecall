//! `visrecall://thumb/<item_id>` 资源协议（design §11.3）。
//!
//! 只服务缩略图：按 id 查库里的 `thumb_path`，校验为库内相对路径后再读文件。
//! 不暴露任意文件或绝对路径——路径全部来自数据库，ID 只允许 `[A-Za-z0-9_-]`。
//!
//! Windows 上 Tauri 把自定义协议映射为 `http://visrecall.localhost/thumb/<id>`，
//! 处理器看到的 `request.uri().path()` 仍是 `/thumb/<id>`；前端用
//! `convertFileSrc` 的等价逻辑把 `visrecall://thumb/<id>` 转成该地址（Task 18）。

use tauri::http::header::{HeaderValue, CACHE_CONTROL, CONTENT_TYPE};
use tauri::http::{Request, Response, StatusCode};
use tauri::Manager;
use visrecall_app_core::thumbnail::load_thumb_bytes;
use visrecall_app_core::{AppCore, AppError, Library};

/// 已注册的协议名；Windows 上的实际地址是 `http://visrecall.localhost/...`。
pub const SCHEME: &str = "visrecall";

/// 单段路径前缀。
const THUMB_PREFIX: &str = "thumb";

/// 协议处理结果里与 HTTP 状态无关的部分。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// 路径不是 `/thumb/<id>`、含多段/穿越/非法字符。
    BadRequest,
    /// 没有该素材、没有缩略图、或缩略图文件已丢失。
    NotFound,
}

impl ProtocolError {
    fn status(self) -> StatusCode {
        match self {
            ProtocolError::BadRequest => StatusCode::BAD_REQUEST,
            ProtocolError::NotFound => StatusCode::NOT_FOUND,
        }
    }

    /// 固定文案：错误信息里不出现任何路径，避免泄露库位置。
    fn body(self) -> &'static str {
        match self {
            ProtocolError::BadRequest => "bad request",
            ProtocolError::NotFound => "not found",
        }
    }
}

/// 从请求路径里取出缩略图素材 id。
///
/// 只接受 `/thumb/<id>` 两段路径（容忍 `visrecall://thumb/<id>` 的完整写法，
/// 由 Tauri 的映射决定实际传入形式）：多段路径、`..`、绝对路径、空 id、
/// 带扩展名/斜杠/冒号的 id 一律拒绝。
pub fn parse_thumb_request_path(path: &str) -> Result<&str, ProtocolError> {
    let rest = path
        .strip_prefix(SCHEME)
        .and_then(|tail| tail.strip_prefix("://"))
        .unwrap_or(path);
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    if rest.is_empty() {
        return Err(ProtocolError::BadRequest);
    }

    let mut segments = rest.split('/');
    let prefix = segments.next().ok_or(ProtocolError::BadRequest)?;
    let item_id = segments.next().ok_or(ProtocolError::BadRequest)?;
    if prefix != THUMB_PREFIX || segments.next().is_some() || !is_valid_item_id(item_id) {
        return Err(ProtocolError::BadRequest);
    }
    Ok(item_id)
}

/// 素材 id 只允许 ASCII 字母数字、`-`、`_`：天然排除 `..`、盘符、百分号编码与斜杠。
fn is_valid_item_id(item_id: &str) -> bool {
    const MAX_LEN: usize = 128;
    !item_id.is_empty()
        && item_id.len() <= MAX_LEN
        && item_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Tauri 协议处理器：解析路径 → 查库 → 读缩略图 → 200/400/404。
pub fn handler<R: tauri::Runtime>(
    ctx: tauri::UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let path = request.uri().path();
    match ctx.app_handle().try_state::<AppCore>() {
        Some(core) => match core.library() {
            Ok(library) => thumb_response(Some(library), path),
            // 库打不开：与「没有缩略图」一致，避免把启动错误细节暴露给页面。
            Err(_) => thumb_response(None, path),
        },
        None => thumb_response(None, path),
    }
}

/// 纯响应构造（`library = None` 表示库不可用），便于测试。
fn thumb_response(library: Option<&Library>, path: &str) -> Response<Vec<u8>> {
    let item_id = match parse_thumb_request_path(path) {
        Ok(item_id) => item_id,
        Err(err) => return error_response(err),
    };
    let Some(library) = library else {
        return error_response(ProtocolError::NotFound);
    };

    match load_thumb_bytes(library, item_id) {
        Ok(bytes) => {
            let mut response = Response::new(bytes);
            *response.status_mut() = StatusCode::OK;
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("image/png"));
            // 缩略图按素材 id 命名、生成后不再变化（design §11.3）。
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("immutable"));
            response
        }
        Err(AppError::InvalidInput(_)) => error_response(ProtocolError::BadRequest),
        Err(_) => error_response(ProtocolError::NotFound),
    }
}

fn error_response(err: ProtocolError) -> Response<Vec<u8>> {
    let mut response = Response::new(err.body().as_bytes().to_vec());
    *response.status_mut() = err.status();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use visrecall_domain::item::{ItemKind, ItemStatus, NewItem};

    use super::*;

    fn library_with_thumb(dir: &Path) -> (Library, String) {
        let library = Library::open(dir.to_path_buf()).expect("open temp library");
        let id = "abc-123".to_string();
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
                    original_path: "originals/images/2026/01/abc-123.png".into(),
                    content_hash: "hash".into(),
                },
            )
            .unwrap();
        let relative = format!("thumbs/{id}.png");
        fs::write(dir.join(&relative), b"\x89PNG\r\n\x1a\nthumb").unwrap();
        library
            .update_status(&id, ItemStatus::Ready, Some(&relative))
            .unwrap();
        (library, id)
    }

    #[test]
    fn parses_thumb_paths() {
        assert_eq!(parse_thumb_request_path("/thumb/abc123"), Ok("abc123"));
        assert_eq!(
            parse_thumb_request_path("/thumb/3f2a_9c-D"),
            Ok("3f2a_9c-D")
        );
        // 容忍完整协议写法（Tauri 映射前/后的两种形态）。
        assert_eq!(
            parse_thumb_request_path("visrecall://thumb/abc123"),
            Ok("abc123")
        );
    }

    /// wry 把 `http://visrecall.localhost/thumb/<id>` 还原成 `visrecall://localhost/thumb/<id>`
    /// 再交给处理器；两种 URI 的 `path()` 都是 `/thumb/<id>`。
    #[test]
    fn accepts_the_uri_forms_wry_produces() {
        let rewritten: tauri::http::Uri = "visrecall://localhost/thumb/abc123".parse().unwrap();
        assert_eq!(parse_thumb_request_path(rewritten.path()), Ok("abc123"));

        let windows: tauri::http::Uri = "http://visrecall.localhost/thumb/abc123".parse().unwrap();
        assert_eq!(parse_thumb_request_path(windows.path()), Ok("abc123"));
    }

    #[test]
    fn rejects_traversal_and_extra_segments() {
        for path in [
            "",
            "/",
            "/thumb",
            "/thumb/",
            "/thumb/abc/",
            "/thumb/a/b",
            "/thumb/../config.json",
            "/thumb/..%2Fconfig.json",
            "/thumb/C:%5CUsers%5Cme",
            "/thumb/abc.png",
            "/original/abc",
            "/config.json",
            "//thumb/abc",
        ] {
            assert_eq!(
                parse_thumb_request_path(path),
                Err(ProtocolError::BadRequest),
                "path {path:?} must be rejected"
            );
        }
    }

    #[test]
    fn returns_png_with_cache_header() {
        let dir = tempfile::tempdir().unwrap();
        let (library, id) = library_with_thumb(dir.path());

        let response = thumb_response(Some(&library), &format!("/thumb/{id}"));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "image/png");
        assert_eq!(response.headers()[CACHE_CONTROL], "immutable");
        assert_eq!(response.body().as_slice(), b"\x89PNG\r\n\x1a\nthumb");
    }

    #[test]
    fn unknown_ids_and_broken_paths_map_to_4xx() {
        let dir = tempfile::tempdir().unwrap();
        let (library, _id) = library_with_thumb(dir.path());

        let missing = thumb_response(Some(&library), "/thumb/nope");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let traversal = thumb_response(Some(&library), "/thumb/../config.json");
        assert_eq!(traversal.status(), StatusCode::BAD_REQUEST);

        // 库不可用：与缺图一致返回 404，不泄露启动错误。
        let unavailable = thumb_response(None, "/thumb/abc-123");
        assert_eq!(unavailable.status(), StatusCode::NOT_FOUND);
    }
}

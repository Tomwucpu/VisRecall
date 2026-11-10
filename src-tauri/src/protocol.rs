//! `visrecall://thumb/<item_id>` 与 `visrecall://original/<item_id>` 资源协议（design §11.3、§8.3）。
//!
//! 只服务库内资产：按 id 查库里的 `thumb_path` / `original_path`，校验为对应目录下的
//! 库内相对路径后再读文件。不暴露任意文件或绝对路径——路径全部来自数据库，ID 只允许
//! `[A-Za-z0-9_-]`。只接受 `GET`/`HEAD`（其余方法 405）；200 带一年 `immutable` 缓存头，
//! 4xx 不带。
//!
//! Windows 上 Tauri 把自定义协议映射为 `http://visrecall.localhost/thumb/<id>`，
//! 处理器看到的 `request.uri().path()` 仍是 `/thumb/<id>`；前端用
//! `convertFileSrc` 的等价逻辑把 `visrecall://thumb/<id>` 转成该地址。

use tauri::http::header::{HeaderValue, ALLOW, CACHE_CONTROL, CONTENT_TYPE};
use tauri::http::{Method, Request, Response, StatusCode};
use tauri::Manager;
use visrecall_app_core::thumbnail::{load_original_bytes, load_thumb_bytes};
use visrecall_app_core::{AppCore, AppError, Library};

/// 已注册的协议名；Windows 上的实际地址是 `http://visrecall.localhost/...`。
pub const SCHEME: &str = "visrecall";

/// 缩略图路径前缀。
const THUMB_PREFIX: &str = "thumb";

/// 原图路径前缀。
pub const ORIGINAL_PREFIX: &str = "original";

/// 缩略图内容按素材 id 命名、生成后不再变化：一年的 `immutable` 长缓存。
const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";
const CACHE_REVALIDATE: &str = "no-cache";

/// 只读方法；其他方法一律 405（协议只提供图片，不接受写入）。
const ALLOWED_METHODS: &str = "GET, HEAD";

/// 协议服务的资产类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    Thumb,
    Original,
}

impl AssetKind {
    fn from_prefix(prefix: &str) -> Option<AssetKind> {
        match prefix {
            THUMB_PREFIX => Some(AssetKind::Thumb),
            ORIGINAL_PREFIX => Some(AssetKind::Original),
            _ => None,
        }
    }
}

/// 协议处理结果里与 HTTP 状态无关的部分。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// 路径不是 `/<thumb|original>/<id>`、含多段/穿越/非法字符。
    BadRequest,
    /// 没有该素材、没有对应资产、或文件已丢失。
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

/// 从请求路径里取出资产类别与素材 id。
///
/// 只接受 `/<prefix>/<id>` 两段路径（容忍 `visrecall://<prefix>/<id>` 的完整写法，
/// 由 Tauri 的映射决定实际传入形式）：多段路径、`..`、绝对路径、空 id、
/// 带扩展名/斜杠/冒号的 id 一律拒绝；未知前缀同样拒绝。
pub fn parse_asset_request_path(path: &str) -> Result<(AssetKind, &str), ProtocolError> {
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
    let kind = AssetKind::from_prefix(prefix).ok_or(ProtocolError::BadRequest)?;
    if segments.next().is_some() || !is_valid_item_id(item_id) {
        return Err(ProtocolError::BadRequest);
    }
    Ok((kind, item_id))
}

/// 兼容旧调用：只解析缩略图路径，非 `thumb` 前缀按 `BadRequest` 处理。
///
/// 生产路径统一走 [`parse_asset_request_path`]；保留这个薄封装是为了不让既有测试
/// 因签名变化大改。
#[allow(dead_code)]
pub fn parse_thumb_request_path(path: &str) -> Result<&str, ProtocolError> {
    match parse_asset_request_path(path)? {
        (AssetKind::Thumb, item_id) => Ok(item_id),
        (AssetKind::Original, _) => Err(ProtocolError::BadRequest),
    }
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

/// Tauri 协议处理器：方法/路径校验 → 查库 → 读资产 → 200/400/404/405。
pub fn handler<R: tauri::Runtime>(
    ctx: tauri::UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    match ctx.app_handle().try_state::<AppCore>() {
        Some(core) => match core.library() {
            Ok(library) => asset_response(Some(library), &method, &path),
            // 库打不开：与「没有该资产」一致，避免把启动错误细节暴露给页面。
            Err(_) => asset_response(None, &method, &path),
        },
        None => asset_response(None, &method, &path),
    }
}

/// 纯响应构造（`library = None` 表示库不可用），便于测试。
///
/// 只有 `GET`/`HEAD` 返回内容；其他方法 405（带 `Allow`）。`HEAD` 与 `GET` 同状态/头，
/// 但不带正文。错误响应（400/404/405）一律不缓存。
fn asset_response(library: Option<&Library>, method: &Method, path: &str) -> Response<Vec<u8>> {
    if *method != Method::GET && *method != Method::HEAD {
        return method_not_allowed();
    }

    let (kind, item_id) = match parse_asset_request_path(path) {
        Ok(parsed) => parsed,
        Err(err) => return error_response(err),
    };
    let Some(library) = library else {
        return error_response(ProtocolError::NotFound);
    };

    let loaded = match kind {
        AssetKind::Thumb => load_thumb_bytes(library, item_id).map(|bytes| (bytes, "image/png")),
        AssetKind::Original => load_original_bytes(library, item_id),
    };

    match loaded {
        Ok((bytes, mime)) => {
            // `HEAD` 只需要头，正文留空。
            let body = if *method == Method::HEAD {
                Vec::new()
            } else {
                bytes
            };
            let mut response = Response::new(body);
            *response.status_mut() = StatusCode::OK;
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static(mime));
            let cache = if library
                .external_asset_for_item(item_id)
                .ok()
                .flatten()
                .is_some()
            {
                CACHE_REVALIDATE
            } else {
                CACHE_IMMUTABLE
            };
            // 引用原件可能变化，必须重新校验；托管资产仍按 ID 长缓存。
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static(cache));
            response
        }
        Err(AppError::InvalidInput(_)) => error_response(ProtocolError::BadRequest),
        Err(_) => error_response(ProtocolError::NotFound),
    }
}

fn method_not_allowed() -> Response<Vec<u8>> {
    let mut response = Response::new(b"method not allowed".to_vec());
    *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(ALLOW, HeaderValue::from_static(ALLOWED_METHODS));
    response
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

    #[test]
    fn parses_original_paths() {
        assert_eq!(
            parse_asset_request_path("/original/abc123"),
            Ok((AssetKind::Original, "abc123"))
        );
        assert_eq!(
            parse_asset_request_path("visrecall://original/abc123"),
            Ok((AssetKind::Original, "abc123"))
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

        let original: tauri::http::Uri = "http://visrecall.localhost/original/abc123"
            .parse()
            .unwrap();
        assert_eq!(
            parse_asset_request_path(original.path()),
            Ok((AssetKind::Original, "abc123"))
        );
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

    /// 原图路径同样只接受两段：多段、带扩展名、穿越一律 400。
    #[test]
    fn rejects_bad_original_paths() {
        for path in [
            "/original",
            "/original/",
            "/original/a/b",
            "/original/../config.json",
            "/original/a.png",
            "/original/abc/",
            "/original/C:%5CUsers%5Cme",
            "/originalx/abc",
        ] {
            assert_eq!(
                parse_asset_request_path(path),
                Err(ProtocolError::BadRequest),
                "path {path:?} must be rejected"
            );
        }
    }

    #[test]
    fn returns_png_with_cache_header() {
        let dir = tempfile::tempdir().unwrap();
        let (library, id) = library_with_thumb(dir.path());

        let response = asset_response(Some(&library), &Method::GET, &format!("/thumb/{id}"));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "image/png");
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(response.body().as_slice(), b"\x89PNG\r\n\x1a\nthumb");
    }

    /// `HEAD` 与 `GET` 同状态/头，但不带正文。
    #[test]
    fn head_returns_headers_without_a_body() {
        let dir = tempfile::tempdir().unwrap();
        let (library, id) = library_with_thumb(dir.path());

        let response = asset_response(Some(&library), &Method::HEAD, &format!("/thumb/{id}"));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "image/png");
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert!(response.body().is_empty());
    }

    /// 只读协议：GET/HEAD 之外的任何方法都 405（带 `Allow`、不带缓存头）。
    #[test]
    fn rejects_non_read_methods() {
        let dir = tempfile::tempdir().unwrap();
        let (library, id) = library_with_thumb(dir.path());

        for method in [
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ] {
            let response = asset_response(Some(&library), &method, &format!("/thumb/{id}"));
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "method {method} must be rejected"
            );
            assert_eq!(response.headers()[ALLOW], "GET, HEAD");
            assert!(
                !response.headers().contains_key(CACHE_CONTROL),
                "405 responses must not be cached"
            );
            assert_eq!(response.body().as_slice(), b"method not allowed");
        }
    }

    #[test]
    fn unknown_ids_and_broken_paths_map_to_4xx() {
        let dir = tempfile::tempdir().unwrap();
        let (library, _id) = library_with_thumb(dir.path());

        let missing = asset_response(Some(&library), &Method::GET, "/thumb/nope");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert!(
            !missing.headers().contains_key(CACHE_CONTROL),
            "404 responses must not be cached as immutable"
        );

        let traversal = asset_response(Some(&library), &Method::GET, "/thumb/../config.json");
        assert_eq!(traversal.status(), StatusCode::BAD_REQUEST);
        assert!(
            !traversal.headers().contains_key(CACHE_CONTROL),
            "400 responses must not be cached as immutable"
        );

        // 库不可用：与缺图一致返回 404，不泄露启动错误。
        let unavailable = asset_response(None, &Method::GET, "/thumb/abc-123");
        assert_eq!(unavailable.status(), StatusCode::NOT_FOUND);
    }

    /// 纵深防御：`thumb_path` 指向 `texts/` 等非缩略图文件时协议返回 400，不读出内容。
    #[test]
    fn thumb_paths_outside_thumbs_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (library, id) = library_with_thumb(dir.path());
        let text = format!("texts/{id}.txt");
        fs::write(dir.path().join(&text), b"secret text").unwrap();
        library
            .update_status(&id, ItemStatus::Ready, Some(&text))
            .unwrap();

        let response = asset_response(Some(&library), &Method::GET, &format!("/thumb/{id}"));
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_ne!(response.body().as_slice(), b"secret text");
    }

    fn library_with_original(dir: &Path) -> (Library, String) {
        let library = Library::open(dir.to_path_buf()).unwrap();
        let id = "orig-1".to_string();
        library
            .insert_item_with_id(
                id.clone(),
                NewItem {
                    kind: ItemKind::Image,
                    captured_at: 1,
                    source_app: None,
                    source_title: None,
                    source_url: None,
                    width: Some(2),
                    height: Some(2),
                    char_count: None,
                    original_path: "originals/images/2026/01/orig-1.png".into(),
                    content_hash: "hash".into(),
                },
            )
            .unwrap();
        let path = dir.join("originals/images/2026/01/orig-1.png");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"\x89PNG\r\n\x1a\noriginal").unwrap();
        (library, id)
    }

    #[test]
    fn serves_original_with_png_content_type() {
        let dir = tempfile::tempdir().unwrap();
        let (library, id) = library_with_original(dir.path());

        let response = asset_response(Some(&library), &Method::GET, &format!("/original/{id}"));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "image/png");
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(response.body().as_slice(), b"\x89PNG\r\n\x1a\noriginal");
    }

    #[test]
    fn original_missing_file_is_404_without_cache() {
        let dir = tempfile::tempdir().unwrap();
        let (library, id) = library_with_original(dir.path());
        fs::remove_file(dir.path().join("originals/images/2026/01/orig-1.png")).unwrap();

        let response = asset_response(Some(&library), &Method::GET, &format!("/original/{id}"));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response.headers().contains_key(CACHE_CONTROL));
    }

    /// 文本素材走 `/original/<id>` 返回 400，且不读出文本内容。
    #[test]
    fn original_rejects_non_image_items() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().to_path_buf()).unwrap();
        let id = "text-1".to_string();
        library
            .insert_item_with_id(
                id.clone(),
                NewItem {
                    kind: ItemKind::Text,
                    captured_at: 1,
                    source_app: None,
                    source_title: None,
                    source_url: None,
                    width: None,
                    height: None,
                    char_count: Some(5),
                    original_path: "texts/text-1.txt".into(),
                    content_hash: "hash".into(),
                },
            )
            .unwrap();
        fs::write(dir.path().join("texts/text-1.txt"), b"secret text").unwrap();

        let response = asset_response(Some(&library), &Method::GET, &format!("/original/{id}"));
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_ne!(response.body().as_slice(), b"secret text");
    }

    /// 纵深防御：图片素材的 `original_path` 指向 `texts/` 时同样 400，不读出内容。
    #[test]
    fn original_paths_outside_originals_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().to_path_buf()).unwrap();
        let id = "sneaky-1".to_string();
        library
            .insert_item_with_id(
                id.clone(),
                NewItem {
                    kind: ItemKind::Image,
                    captured_at: 1,
                    source_app: None,
                    source_title: None,
                    source_url: None,
                    width: Some(2),
                    height: Some(2),
                    char_count: None,
                    // 库里被改坏/被伪造的路径：指向 texts/。
                    original_path: "texts/payload.txt".into(),
                    content_hash: "hash".into(),
                },
            )
            .unwrap();
        fs::create_dir_all(dir.path().join("texts")).unwrap();
        fs::write(dir.path().join("texts/payload.txt"), b"secret text").unwrap();

        let response = asset_response(Some(&library), &Method::GET, &format!("/original/{id}"));
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_ne!(response.body().as_slice(), b"secret text");
    }

    #[test]
    fn original_unknown_id_is_404() {
        let dir = tempfile::tempdir().unwrap();
        let (library, _id) = library_with_original(dir.path());
        let response = asset_response(Some(&library), &Method::GET, "/original/nope");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

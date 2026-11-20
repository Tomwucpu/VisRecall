//! `CF_HTML` 片段的清洗与重新封装。
//!
//! 入库前把浏览器给的整块 `CF_HTML` 收成「只有 fragment」的片段：删掉可执行/外链标签，
//! 把相对的图片地址用头里的 `SourceURL:` 补成绝对地址；复制时再按规范补回定宽的偏移头。
//! 纯字符串处理、不碰 Win32：拖放读取（[`crate::drop_target`]）与写剪贴板
//! （[`crate::clipboard`]）共用。
//!
//! 三条边界：
//! 1. `StartFragment`/`EndFragment` 是**字节偏移**；落在非字符边界或越界时退回全文；
//! 2. `<script>`/`<style>`/`<iframe>`/`<object>` 整段删除，`<link>`/`<meta>`/`<base>`/
//!    `<embed>` 只删标签——剪贴板 HTML 不携带可执行内容，也不让目标应用去拉无关资源；
//! 3. 超过 [`MAX_FRAGMENT_BYTES`] 的片段整块放弃（返回 `None`），由调用方回退纯文本：
//!    截断出半截标签比丢掉 HTML 更糟。

/// 单个片段可以保存/粘贴的最大字节数（UTF-8）。
pub const MAX_FRAGMENT_BYTES: usize = 256 * 1024;

/// 剪贴板注册格式名：`CF_HTML` 的载荷是 UTF-8 文本。
pub const HTML_FORMAT_NAME: &str = "HTML Format";

/// 提取 `CF_HTML` 载荷里的 fragment 区间；没有声明偏移时返回全文。
pub fn fragment_of(raw: &str) -> &str {
    let start = header_offset(raw, "StartFragment:");
    let end = header_offset(raw, "EndFragment:");
    match (start, end) {
        (Some(start), Some(end)) if start < end && end <= raw.len() => {
            // 偏移必须落在字符边界上，否则按全文处理。
            if raw.is_char_boundary(start) && raw.is_char_boundary(end) {
                &raw[start..end]
            } else {
                raw
            }
        }
        _ => raw,
    }
}

/// 读取头里的 `SourceURL:`：相对图片地址要靠它补成绝对地址。
pub fn source_url_of(raw: &str) -> Option<&str> {
    const KEY: &str = "SourceURL:";
    let index = raw.find(KEY)?;
    let rest = &raw[index + KEY.len()..];
    let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
    let value = rest[..end].trim();
    (!value.is_empty()).then_some(value)
}

/// 把原始 `CF_HTML` 载荷清洗成可入库、可粘贴的片段。
///
/// 返回 `None` 的情形：片段为空、只剩空白，或清洗后仍超过 [`MAX_FRAGMENT_BYTES`]。
pub fn sanitize(raw: &str) -> Option<String> {
    let fragment = fragment_of(raw);
    if fragment.trim().is_empty() {
        return None;
    }

    let base = source_url_of(raw).and_then(|url| url::Url::parse(url).ok());
    let cleaned = remove_elements(fragment, &["script", "style", "iframe", "object"], true);
    let cleaned = remove_tags(&cleaned, &["link", "meta", "base", "embed"]);
    let html = absolutize(&cleaned, base.as_ref());

    if html.trim().is_empty() || html.len() > MAX_FRAGMENT_BYTES {
        return None;
    }
    Some(html)
}

/// 把片段封装成 `CF_HTML` 剪贴板载荷（UTF-8，含定宽的偏移头）。
///
/// 头里的四个偏移都是**从数据块起始算起的字节数**；数字固定 10 位，因此头长度与
/// 具体数值无关，可以先用占位值量出长度再回填。`<!--StartFragment-->` 标记同时保留：
/// 只认标记的应用（部分 Office 版本）也能定位片段。
pub fn cf_html_bytes(fragment: &str) -> Vec<u8> {
    let header =
        |start_html: usize, end_html: usize, start_fragment: usize, end_fragment: usize| {
            format!(
                "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\n\
             StartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
            )
        };

    const PREFIX: &str = "<html><body>\r\n<!--StartFragment-->";
    const SUFFIX: &str = "<!--EndFragment-->\r\n</body></html>";

    let header_len = header(0, 0, 0, 0).len();
    let start_html = header_len;
    let start_fragment = start_html + PREFIX.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + SUFFIX.len();

    let mut out = String::with_capacity(end_html);
    out.push_str(&header(start_html, end_html, start_fragment, end_fragment));
    out.push_str(PREFIX);
    out.push_str(fragment);
    out.push_str(SUFFIX);
    out.into_bytes()
}

/// 读 `Key:` 后面紧跟的十进制数字（`CF_HTML` 头的偏移字段）。
fn header_offset(raw: &str, key: &str) -> Option<usize> {
    let index = raw.find(key)?;
    let digits: String = raw[index + key.len()..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// 整段删除成对标签（含内容）；没有闭合标签时按 `drop_rest_if_unclosed` 决定是否删到结尾。
fn remove_elements(html: &str, tags: &[&str], drop_rest_if_unclosed: bool) -> String {
    let lowered = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut index = 0usize;

    while index < html.len() {
        let Some((start, tag_index)) = find_open_tag(&lowered, index, tags) else {
            break;
        };
        out.push_str(&html[index..start]);

        let open_end = html[start..]
            .find('>')
            .map(|pos| start + pos + 1)
            .unwrap_or(html.len());
        let close = format!("</{}", tags[tag_index]);
        match lowered[open_end..].find(&close) {
            Some(relative) => {
                let after = open_end + relative;
                index = html[after..]
                    .find('>')
                    .map(|pos| after + pos + 1)
                    .unwrap_or(html.len());
            }
            None if drop_rest_if_unclosed => index = html.len(),
            None => index = open_end,
        }
    }

    out.push_str(&html[index..]);
    out
}

/// 删除无内容标签本身（`<link ...>` / `<meta ...>` / `<embed ...>`）。
fn remove_tags(html: &str, tags: &[&str]) -> String {
    let lowered = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut index = 0usize;

    while index < html.len() {
        let Some((start, _)) = find_open_tag(&lowered, index, tags) else {
            break;
        };
        out.push_str(&html[index..start]);
        index = html[start..]
            .find('>')
            .map(|pos| start + pos + 1)
            .unwrap_or(html.len());
    }

    out.push_str(&html[index..]);
    out
}

/// 从 `from` 开始找第一个 `<(tag)` 标签，返回（字节位置，`tags` 下标）。
///
/// 标签名后面必须是空白、`>` 或 `/`，避免把 `<scriptx>` 当成 `<script>`。
fn find_open_tag(lowered: &str, from: usize, tags: &[&str]) -> Option<(usize, usize)> {
    let mut search = from;
    while search < lowered.len() {
        let start = search + lowered[search..].find('<')?;
        let rest = &lowered[start + 1..];
        for (index, tag) in tags.iter().enumerate() {
            if let Some(after) = rest.strip_prefix(tag) {
                let boundary = after
                    .chars()
                    .next()
                    .map(|c| c.is_whitespace() || c == '>' || c == '/')
                    .unwrap_or(true);
                if boundary {
                    return Some((start, index));
                }
            }
        }
        search = start + 1;
    }
    None
}

/// 把 `src`/`href` 的相对地址补成绝对地址；`javascript:` 一律清空。
fn absolutize(html: &str, base: Option<&url::Url>) -> String {
    let lowered = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut index = 0usize;

    while index < html.len() {
        let found = ["src=", "href="]
            .iter()
            .filter_map(|attr| lowered[index..].find(attr).map(|pos| index + pos))
            .min();
        let Some(at) = found else {
            out.push_str(&html[index..]);
            break;
        };

        let attr_len = if lowered[at..].starts_with("src=") {
            "src=".len()
        } else {
            "href=".len()
        };
        // 属性名前面必须是空白或引号：否则会命中 `data-src=`、`xlink:href=`。
        let before = html[..at].chars().next_back();
        let boundary = matches!(before, Some(c) if c.is_whitespace() || c == '"' || c == '\'');
        if !boundary {
            out.push_str(&html[index..at + attr_len]);
            index = at + attr_len;
            continue;
        }

        out.push_str(&html[index..at]);
        let mut cursor = at + attr_len;
        while cursor < html.len() && html.as_bytes()[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        out.push_str(&html[at..cursor]);

        let quote = html[cursor..].chars().next();
        let Some(quote) = quote.filter(|c| *c == '"' || *c == '\'') else {
            // 没有引号的属性值：原样保留，不做推测。
            index = cursor;
            continue;
        };
        out.push(quote);
        cursor += quote.len_utf8();

        let value_end = html[cursor..]
            .find(quote)
            .map(|pos| cursor + pos)
            .unwrap_or(html.len());
        out.push_str(&resolve_url(&html[cursor..value_end], base));
        if value_end < html.len() {
            out.push(quote);
            index = value_end + quote.len_utf8();
        } else {
            index = html.len();
        }
    }

    out
}

/// 相对地址用片段来源页做 `join`；绝对地址与 `data:` 原样保留。
fn resolve_url(value: &str, base: Option<&url::Url>) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return value.to_string();
    }
    match url::Url::parse(trimmed) {
        Ok(parsed) if parsed.scheme() == "javascript" => return String::new(),
        // 已经是绝对地址（http/https/data/file/mailto…）：原样保留。
        Ok(_) => return trimmed.to_string(),
        Err(_) => {}
    }
    let Some(base) = base else {
        return value.to_string();
    };
    base.join(trimmed)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(body: &str) -> String {
        let header_len =
            "Version:0.9\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len();
        let start = header_len;
        let end = start + body.len();
        format!("Version:0.9\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\n{body}")
    }

    #[test]
    fn fragment_uses_declared_byte_offsets() {
        let body = "<html>hello</html>";
        let raw = payload(body);
        assert_eq!(fragment_of(&raw), body);
        assert_eq!(fragment_of("<b>hi</b>"), "<b>hi</b>");
    }

    #[test]
    fn source_url_is_read_from_the_header() {
        let raw = "Version:0.9\r\nSourceURL:https://example.com/a\r\n\r\n<p>x</p>";
        assert_eq!(source_url_of(raw), Some("https://example.com/a"));
        assert_eq!(source_url_of("Version:0.9\r\n"), None);
    }

    #[test]
    fn sanitize_strips_scripts_and_keeps_images() {
        let raw = payload(
            r#"<p>正文</p><script>alert(1)</script><style>p{color:red}</style><img src="https://example.com/a.png">"#,
        );
        let html = sanitize(&raw).unwrap();
        assert!(!html.contains("script"), "script must be removed: {html}");
        assert!(
            !html.contains("alert"),
            "script body must be removed: {html}"
        );
        assert!(!html.contains("color:red"), "style must be removed: {html}");
        assert!(
            html.contains(r#"<img src="https://example.com/a.png">"#),
            "{html}"
        );
    }

    #[test]
    fn sanitize_absolutizes_relative_image_sources() {
        let body = r#"<img src="/img/a.png"><a href="b.html">链接</a>"#;
        let raw = format!(
            "Version:0.9\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n\
             SourceURL:https://example.com/post/1\r\n{body}"
        );
        let header_len = raw.len() - body.len();
        let raw = raw.replacen(
            "StartFragment:0000000000",
            &format!("StartFragment:{header_len:010}"),
            1,
        );
        let end = header_len + body.len();
        let raw = raw.replacen(
            "EndFragment:0000000000",
            &format!("EndFragment:{end:010}"),
            1,
        );

        let html = sanitize(&raw).unwrap();
        assert!(
            html.contains(r#"src="https://example.com/img/a.png""#),
            "{html}"
        );
        assert!(
            html.contains(r#"href="https://example.com/post/b.html""#),
            "{html}"
        );
    }

    #[test]
    fn sanitize_drops_javascript_urls_and_oversized_fragments() {
        let raw = payload(r#"<a href="javascript:alert(1)">x</a>"#);
        let html = sanitize(&raw).unwrap();
        assert!(!html.contains("javascript:"), "{html}");

        let big = format!("<p>{}</p>", "x".repeat(MAX_FRAGMENT_BYTES));
        assert!(
            sanitize(&payload(&big)).is_none(),
            "oversized snippet is dropped"
        );
    }

    #[test]
    fn sanitize_returns_none_for_blank_fragments() {
        assert!(sanitize(&payload("   ")).is_none());
        assert!(sanitize(payload("<script>alert(1)</script>").as_str()).is_none());
    }

    #[test]
    fn cf_html_offsets_point_at_the_fragment() {
        let fragment = "<p>你好 &amp; world</p>";
        let bytes = cf_html_bytes(fragment);
        let text = String::from_utf8(bytes).unwrap();

        let start_fragment = header_offset(&text, "StartFragment:").unwrap();
        let end_fragment = header_offset(&text, "EndFragment:").unwrap();
        assert_eq!(&text[start_fragment..end_fragment], fragment);

        let start_html = header_offset(&text, "StartHTML:").unwrap();
        let end_html = header_offset(&text, "EndHTML:").unwrap();
        assert_eq!(&text[start_html..start_html + 5], "<html");
        assert!(text[start_html..end_html].ends_with("</html>"));
        assert_eq!(end_html, text.len(), "EndHTML covers the whole document");
    }
}

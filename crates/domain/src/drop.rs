//! 拖放载荷的纯数据表示。

use serde::{Deserialize, Serialize};

/// 一次拖放操作携带的内容。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DropContent {
    /// 文件路径（`CF_HDROP`）。
    Files(Vec<std::path::PathBuf>),
    /// 文件 + 来源附带的文字片段（混合拖拽，design §9.2）。
    ///
    /// 文字不单独建文本素材：图片入库后写入 `items.note`。
    FilesWithNote {
        files: Vec<std::path::PathBuf>,
        note: String,
    },
    /// 纯文本，可附带 HTML 剪贴板片段。
    Text { plain: String, html: Option<String> },
    /// 链接（`CFSTR_INETURLW` / 文本 URL）。
    Url { url: String },
    /// 位图（DIB 解出的 top-down BGRA8）。
    ImageDib {
        width: u32,
        height: u32,
        bgra: Vec<u8>,
    },
}

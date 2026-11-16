//! AI 解析结果（`insights` 表）的领域类型：摘要、结构化属性、OCR 文本与包围盒。

use serde::{Deserialize, Serialize};

/// OCR 包围盒：归一化到原图宽高的 `[0,1]` 比例，便于前端按百分比定位高亮框。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrBox {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl OcrBox {
    pub fn new(text: impl Into<String>, x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            text: text.into(),
            x,
            y,
            w,
            h,
        }
    }

    /// 归一化并夹紧到 `[0,1]`；宽高非正时返回 `None`（脏数据不参与高亮）。
    pub fn normalized(
        text: impl Into<String>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        iw: f32,
        ih: f32,
    ) -> Option<Self> {
        if iw <= 0.0 || ih <= 0.0 || w <= 0.0 || h <= 0.0 {
            return None;
        }
        let clamp = |v: f32| v.clamp(0.0, 1.0);
        let nx = clamp(x / iw);
        let ny = clamp(y / ih);
        let nw = clamp(w / iw).min(1.0 - nx);
        let nh = clamp(h / ih).min(1.0 - ny);
        if nw <= 0.0 || nh <= 0.0 {
            return None;
        }
        Some(Self {
            text: text.into(),
            x: nx,
            y: ny,
            w: nw,
            h: nh,
        })
    }

    pub fn contains(&self, needle: &str) -> bool {
        !needle.is_empty() && self.text.contains(needle)
    }
}

/// 摘要来源：决定详情面板的徽标与「是否需要重新解析」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SummarySource {
    #[default]
    None,
    Vlm,
    Ocr,
    Extractive,
    User,
}

impl SummarySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Vlm => "vlm",
            Self::Ocr => "ocr",
            Self::Extractive => "extractive",
            Self::User => "user",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "none" => Some(Self::None),
            "vlm" => Some(Self::Vlm),
            "ocr" => Some(Self::Ocr),
            "extractive" => Some(Self::Extractive),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// 图片素材的属性枚举（VLM 输出与查询解析共用同一套字面量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Screenshot,
    Photo,
    Diagram,
    Document,
    Other,
}

impl ContentType {
    pub const ALL: [ContentType; 5] = [
        Self::Screenshot,
        Self::Photo,
        Self::Diagram,
        Self::Document,
        Self::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screenshot => "screenshot",
            Self::Photo => "photo",
            Self::Diagram => "diagram",
            Self::Document => "document",
            Self::Other => "other",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "screenshot" => Some(Self::Screenshot),
            "photo" => Some(Self::Photo),
            "diagram" => Some(Self::Diagram),
            "document" => Some(Self::Document),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// 主色枚举（design §4.4）。同义词表把「橙」「橙色」都归一到 `Orange`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrimaryColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Pink,
    Brown,
    Black,
    White,
    Gray,
    Multicolor,
}

impl PrimaryColor {
    pub const ALL: [PrimaryColor; 12] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Purple,
        Self::Pink,
        Self::Brown,
        Self::Black,
        Self::White,
        Self::Gray,
        Self::Multicolor,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Pink => "pink",
            Self::Brown => "brown",
            Self::Black => "black",
            Self::White => "white",
            Self::Gray => "gray",
            Self::Multicolor => "multicolor",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let lower = raw.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|c| c.as_str() == lower)
    }

    /// 中文展示名（chip 与解释用）。
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Red => "红色",
            Self::Orange => "橙色",
            Self::Yellow => "黄色",
            Self::Green => "绿色",
            Self::Blue => "蓝色",
            Self::Purple => "紫色",
            Self::Pink => "粉色",
            Self::Brown => "棕色",
            Self::Black => "黑色",
            Self::White => "白色",
            Self::Gray => "灰色",
            Self::Multicolor => "多彩",
        }
    }
}

/// 图表类型枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChartType {
    Bar,
    Line,
    Pie,
    Funnel,
    Table,
    None,
}

impl ChartType {
    pub const ALL: [ChartType; 6] = [
        Self::Bar,
        Self::Line,
        Self::Pie,
        Self::Funnel,
        Self::Table,
        Self::None,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Line => "line",
            Self::Pie => "pie",
            Self::Funnel => "funnel",
            Self::Table => "table",
            Self::None => "none",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let lower = raw.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|c| c.as_str() == lower)
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Bar => "柱状图",
            Self::Line => "折线图",
            Self::Pie => "饼图",
            Self::Funnel => "漏斗图",
            Self::Table => "表格",
            Self::None => "无图表",
        }
    }
}

/// 界面类型枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiType {
    Dialog,
    Settings,
    List,
    Article,
    Chat,
    Code,
    Other,
}

impl UiType {
    pub const ALL: [UiType; 7] = [
        Self::Dialog,
        Self::Settings,
        Self::List,
        Self::Article,
        Self::Chat,
        Self::Code,
        Self::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dialog => "dialog",
            Self::Settings => "settings",
            Self::List => "list",
            Self::Article => "article",
            Self::Chat => "chat",
            Self::Code => "code",
            Self::Other => "other",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let lower = raw.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|c| c.as_str() == lower)
    }
}

/// 一条素材的完整 AI 解析结果（`insights` 表一行）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Insight {
    pub item_id: String,
    pub summary: Option<String>,
    pub summary_source: SummarySource,
    pub content_type: Option<String>,
    pub primary_color: Option<String>,
    pub chart_type: Option<String>,
    pub ui_type: Option<String>,
    pub entities: Vec<String>,
    pub ocr_text: Option<String>,
    pub ocr_boxes: Vec<OcrBox>,
    pub ocr_engine: Option<String>,
    pub ocr_confidence: Option<f32>,
    pub latex: Option<String>,
    pub has_formula: bool,
    pub model_provider: Option<String>,
    pub model_name: Option<String>,
    pub confidence: Option<f32>,
    pub summary_user_edited: bool,
    pub tags_user_edited: bool,
    /// 用户手工改过 OCR 文本：重新识别不得覆盖 `ocr_text` / `ocr_boxes`。
    pub ocr_user_edited: bool,
    pub updated_at: i64,
}

impl Insight {
    /// 空 insight（OCR 无文字 / 尚未解析时的行）。
    pub fn empty(item_id: impl Into<String>, updated_at: i64) -> Self {
        Self {
            item_id: item_id.into(),
            summary: None,
            summary_source: SummarySource::None,
            content_type: None,
            primary_color: None,
            chart_type: None,
            ui_type: None,
            entities: Vec::new(),
            ocr_text: None,
            ocr_boxes: Vec::new(),
            ocr_engine: None,
            ocr_confidence: None,
            latex: None,
            has_formula: false,
            model_provider: None,
            model_name: None,
            confidence: None,
            summary_user_edited: false,
            tags_user_edited: false,
            ocr_user_edited: false,
            updated_at,
        }
    }

    /// 是否有可用于检索的 OCR 文本（空文本不算）。
    pub fn has_ocr_text(&self) -> bool {
        self.ocr_text
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty())
    }

    /// OCR 置信度低于 0.6 才标注「置信度低」；无文字（`None`）不触发。
    pub fn low_ocr_confidence(&self) -> bool {
        self.ocr_confidence.is_some_and(|c| c < 0.6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_box_normalizes_and_clamps() {
        let b = OcrBox::normalized("确定", 50.0, 25.0, 100.0, 50.0, 200.0, 100.0).unwrap();
        assert!((b.x - 0.25).abs() < 1e-6);
        assert!((b.y - 0.25).abs() < 1e-6);
        assert!((b.w - 0.5).abs() < 1e-6);
        assert!((b.h - 0.5).abs() < 1e-6);

        // 越界收缩到图内
        let b = OcrBox::normalized("越界", 190.0, 90.0, 100.0, 100.0, 200.0, 100.0).unwrap();
        assert!((b.w - 0.05).abs() < 1e-6);
        assert!((b.h - 0.1).abs() < 1e-6);
        assert!(b.x + b.w <= 1.0 + 1e-6);

        assert!(OcrBox::normalized("空", 0.0, 0.0, 0.0, 10.0, 200.0, 100.0).is_none());
    }

    #[test]
    fn summary_source_roundtrips() {
        for s in [
            SummarySource::None,
            SummarySource::Vlm,
            SummarySource::Ocr,
            SummarySource::Extractive,
            SummarySource::User,
        ] {
            assert_eq!(SummarySource::parse(s.as_str()), Some(s));
        }
        assert_eq!(SummarySource::parse("bogus"), None);
    }

    #[test]
    fn color_enum_roundtrips_and_labels() {
        for c in PrimaryColor::ALL {
            assert_eq!(PrimaryColor::parse(c.as_str()), Some(c));
            assert!(!c.label_zh().is_empty());
        }
        assert_eq!(PrimaryColor::parse("ORANGE"), Some(PrimaryColor::Orange));
        for c in ChartType::ALL {
            assert_eq!(ChartType::parse(c.as_str()), Some(c));
        }
        for u in UiType::ALL {
            assert_eq!(UiType::parse(u.as_str()), Some(u));
        }
        for c in ContentType::ALL {
            assert_eq!(ContentType::parse(c.as_str()), Some(c));
        }
    }

    #[test]
    fn empty_insight_is_not_low_confidence() {
        let insight = Insight::empty("item-1", 0);
        assert!(!insight.has_ocr_text());
        assert!(!insight.low_ocr_confidence(), "无文字不应触发低置信标注");
        assert_eq!(insight.summary_source, SummarySource::None);
    }

    #[test]
    fn ocr_box_contains_matches_substring() {
        let b = OcrBox::new("导出 PDF", 0.0, 0.0, 0.1, 0.1);
        assert!(b.contains("导出"));
        assert!(!b.contains("导入"));
        assert!(!b.contains(""));
    }
}

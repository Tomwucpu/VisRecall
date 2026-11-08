//! PP-OCR（PaddleOCR mobile det + rec，ONNX）实现。
//!
//! 流程：图片 → 等比缩放到长边 960（32 对齐）→ det（DBNet）输出概率图 →
//! 二值化 + 连通域 → 轴对齐包围盒（unclip 近似按面积/周长外扩）→ 逐框 crop →
//! rec（CRNN + CTC 贪心解码）→ 按 y-x 阅读序拼接。
//!
//! 与 PaddleOCR 官方实现的差异（design §1.2 / tasks Task 6 注记）：
//! - 不做旋转框（minAreaRect）与透视裁剪，用轴对齐框近似——UI 截图文字基本水平；
//! - unclip 用「面积/周长」公式的外扩距离，不做多边形偏移；
//! - 长边 > 4096 先降采样到 4096，角标级小字可能丢（已登记为限制）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use image::{imageops::FilterType, RgbImage};
use ort::session::Session;
use ort::value::Tensor;
use visrecall_domain::insight::OcrBox;
use visrecall_domain::providers::OcrOutput;
use visrecall_domain::AppError;

use crate::runtime::session_builder;

/// 写入 `insights.ocr_engine` 的引擎标识。
pub const PPOCR_ID: &str = "ppocr-mobile";

/// 模型与字典文件名（`models/manifest.json` 同名）。
pub const DET_FILE: &str = "ch_PP-OCRv4_det_infer.onnx";
pub const REC_FILE: &str = "ch_PP-OCRv4_rec_infer.onnx";
pub const KEYS_FILE: &str = "ppocr_keys_v1.txt";

const DET_INPUT: &str = "x";
const REC_INPUT: &str = "x";

/// det 输入长边上限（PaddleOCR mobile det 常用 960）。
const DET_LIMIT_SIDE: f32 = 960.0;
/// det 概率图二值化阈值。
const DET_BIN_THRESHOLD: f32 = 0.3;
/// 连通域平均概率下限（低于即噪声）。
const DET_BOX_THRESHOLD: f32 = 0.5;
/// unclip 外扩比例（PaddleOCR det_db_unclip_ratio）。
const DET_UNCLIP_RATIO: f32 = 1.5;
/// 连通域最小边长（像素，det 输入尺度）。
const DET_MIN_SIDE: usize = 3;
/// 超长图降采样上限。
const MAX_IMAGE_SIDE: u32 = 4096;

/// rec 输入高度。
const REC_HEIGHT: u32 = 48;
/// rec 输入宽度上限与下限。
const REC_MAX_WIDTH: u32 = 320;
const REC_MIN_WIDTH: u32 = 8;

/// PP-OCR 引擎（懒加载后常驻，内部用 `Mutex` 串行化 `run`）。
pub struct PpocrOcr {
    det: Mutex<Session>,
    rec: Mutex<Session>,
    keys: Vec<String>,
    model_dir: PathBuf,
}

impl std::fmt::Debug for PpocrOcr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PpocrOcr")
            .field("model_dir", &self.model_dir)
            .field("keys", &self.keys.len())
            .finish()
    }
}

impl PpocrOcr {
    /// 从模型目录加载 det/rec 与字典。模型缺失或加载失败返回 `Err`（调用方走兜底）。
    pub fn load(model_dir: &Path) -> Result<Self, AppError> {
        let det_path = model_dir.join(DET_FILE);
        let rec_path = model_dir.join(REC_FILE);
        for path in [&det_path, &rec_path] {
            if !path.is_file() {
                return Err(AppError::FileMissing(path.display().to_string()));
            }
        }
        let keys = load_keys(&model_dir.join(KEYS_FILE))?;
        let det = session_builder()?
            .commit_from_file(&det_path)
            .map_err(|e| AppError::Internal(format!("ppocr det: {e}")))?;
        let rec = session_builder()?
            .commit_from_file(&rec_path)
            .map_err(|e| AppError::Internal(format!("ppocr rec: {e}")))?;
        Ok(Self {
            det: Mutex::new(det),
            rec: Mutex::new(rec),
            keys,
            model_dir: model_dir.to_path_buf(),
        })
    }

    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    /// 识别 PNG/JPEG 等常见格式的字节。
    pub fn recognize_bytes(&self, bytes: &[u8]) -> Result<OcrOutput, AppError> {
        let img = image::load_from_memory(bytes)
            .map_err(|e| AppError::InvalidInput(format!("ocr: decode image: {e}")))?;
        self.recognize_image(&img.to_rgb8())
    }

    /// 识别 RGB 图片。无文字返回 `Ok(OcrOutput::empty())`——空结果是正常结果。
    pub fn recognize_image(&self, img: &RgbImage) -> Result<OcrOutput, AppError> {
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 {
            return Ok(OcrOutput::empty());
        }
        let img = downscale_if_huge(img);

        let boxes = self.detect(&img)?;
        if boxes.is_empty() {
            return Ok(OcrOutput::empty());
        }

        let mut recognized: Vec<RecognizedBox> = Vec::with_capacity(boxes.len());
        for b in &boxes {
            let crop = crop_rgb(img.as_ref(), b);
            let (text, conf) = self.recognize_crop(&crop)?;
            if text.trim().is_empty() {
                continue;
            }
            recognized.push(RecognizedBox {
                rect: *b,
                text,
                confidence: conf,
            });
        }
        Ok(assemble(recognized, img.as_ref()))
    }

    /// det：返回原图坐标系下的候选框。
    fn detect(&self, img: &RgbImage) -> Result<Vec<Rect>, AppError> {
        let (ow, oh) = img.dimensions();
        let scale = (DET_LIMIT_SIDE / ow.max(oh) as f32).min(1.0);
        let nw = align_up(((ow as f32 * scale).round() as u32).max(32), 32);
        let nh = align_up(((oh as f32 * scale).round() as u32).max(32), 32);
        let resized = image::imageops::resize(img, nw, nh, FilterType::Triangle);

        let mut data = vec![0f32; 3 * (nh as usize) * (nw as usize)];
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD: [f32; 3] = [0.229, 0.224, 0.225];
        let plane = (nh as usize) * (nw as usize);
        for (x, y, px) in resized.enumerate_pixels() {
            let idx = (y as usize) * (nw as usize) + x as usize;
            for c in 0..3 {
                data[c * plane + idx] = (px[c] as f32 / 255.0 - MEAN[c]) / STD[c];
            }
        }

        let input = Tensor::from_array((vec![1i64, 3, nh as i64, nw as i64], data))
            .map_err(|e| AppError::Internal(format!("ppocr det tensor: {e}")))?;
        let mut session = self
            .det
            .lock()
            .map_err(|_| AppError::Internal("ppocr det session poisoned".to_string()))?;
        let outputs = session
            .run(ort::inputs![DET_INPUT => input])
            .map_err(|e| AppError::Internal(format!("ppocr det run: {e}")))?;
        let (shape, probs) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Internal(format!("ppocr det output: {e}")))?;
        let map_h = *shape.get(2).unwrap_or(&0) as usize;
        let map_w = *shape.get(3).unwrap_or(&0) as usize;
        if map_h != nh as usize || map_w != nw as usize {
            return Err(AppError::Internal(format!(
                "ppocr det: unexpected output shape {shape:?}"
            )));
        }

        let mut boxes = extract_boxes(probs, map_w, map_h);
        let sx = ow as f32 / map_w as f32;
        let sy = oh as f32 / map_h as f32;
        for b in &mut boxes {
            b.x0 *= sx;
            b.x1 *= sx;
            b.y0 *= sy;
            b.y1 *= sy;
            unclip(b, DET_UNCLIP_RATIO, ow as f32, oh as f32);
        }
        boxes.retain(|b| b.width() >= 2.0 && b.height() >= 2.0);
        Ok(boxes)
    }

    /// rec：单框识别，返回 (文本, 置信度)。
    fn recognize_crop(&self, crop: &RgbImage) -> Result<(String, f32), AppError> {
        let (cw, ch) = crop.dimensions();
        if cw == 0 || ch == 0 {
            return Ok((String::new(), 0.0));
        }
        let ratio = cw as f32 / ch as f32;
        let rw = ((REC_HEIGHT as f32 * ratio).ceil() as u32).clamp(REC_MIN_WIDTH, REC_MAX_WIDTH);
        let resized = image::imageops::resize(crop, rw, REC_HEIGHT, FilterType::Triangle);

        let plane = (REC_HEIGHT as usize) * (rw as usize);
        let mut data = vec![0f32; 3 * plane];
        for (x, y, px) in resized.enumerate_pixels() {
            let idx = (y as usize) * (rw as usize) + x as usize;
            for c in 0..3 {
                data[c * plane + idx] = (px[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }

        let input = Tensor::from_array((vec![1i64, 3, REC_HEIGHT as i64, rw as i64], data))
            .map_err(|e| AppError::Internal(format!("ppocr rec tensor: {e}")))?;
        let mut session = self
            .rec
            .lock()
            .map_err(|_| AppError::Internal("ppocr rec session poisoned".to_string()))?;
        let outputs = session
            .run(ort::inputs![REC_INPUT => input])
            .map_err(|e| AppError::Internal(format!("ppocr rec run: {e}")))?;
        let (shape, probs) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Internal(format!("ppocr rec output: {e}")))?;
        let steps = *shape.get(1).unwrap_or(&0) as usize;
        let classes = *shape.get(2).unwrap_or(&0) as usize;
        Ok(ctc_greedy_decode(probs, steps, classes, &self.keys))
    }
}

/// det 后的轴对齐框（原图坐标）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }

    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }

    fn center_y(&self) -> f32 {
        (self.y0 + self.y1) / 2.0
    }
}

struct RecognizedBox {
    rect: Rect,
    text: String,
    confidence: f32,
}

fn align_up(value: u32, align: u32) -> u32 {
    value.div_ceil(align) * align
}

fn downscale_if_huge(img: &RgbImage) -> std::borrow::Cow<'_, RgbImage> {
    let (w, h) = img.dimensions();
    let max_side = w.max(h);
    if max_side <= MAX_IMAGE_SIDE {
        return std::borrow::Cow::Borrowed(img);
    }
    let scale = MAX_IMAGE_SIDE as f32 / max_side as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    std::borrow::Cow::Owned(image::imageops::resize(img, nw, nh, FilterType::Triangle))
}

fn crop_rgb(img: &RgbImage, rect: &Rect) -> RgbImage {
    let (w, h) = img.dimensions();
    let x0 = rect.x0.floor().clamp(0.0, w as f32) as u32;
    let y0 = rect.y0.floor().clamp(0.0, h as f32) as u32;
    let x1 = rect.x1.ceil().clamp(0.0, w as f32) as u32;
    let y1 = rect.y1.ceil().clamp(0.0, h as f32) as u32;
    let cw = x1.saturating_sub(x0).max(1);
    let ch = y1.saturating_sub(y0).max(1);
    let cw = cw.min(w.saturating_sub(x0)).max(1);
    let ch = ch.min(h.saturating_sub(y0)).max(1);
    image::imageops::crop_imm(img, x0, y0, cw, ch).to_image()
}

/// DB 概率图 → 轴对齐框（det 输入坐标）。用连通域代替轮廓 + minAreaRect。
fn extract_boxes(probs: &[f32], w: usize, h: usize) -> Vec<Rect> {
    if w == 0 || h == 0 || probs.len() < w * h {
        return Vec::new();
    }
    let mut visited = vec![false; w * h];
    let mut out: Vec<Rect> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();

    for start in 0..w * h {
        if visited[start] || probs[start] <= DET_BIN_THRESHOLD {
            continue;
        }
        visited[start] = true;
        stack.push(start);
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0usize, 0usize);
        let mut score_sum = 0f32;
        let mut count = 0usize;
        while let Some(idx) = stack.pop() {
            let x = idx % w;
            let y = idx / w;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            score_sum += probs[idx];
            count += 1;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let nidx = ny as usize * w + nx as usize;
                    if !visited[nidx] && probs[nidx] > DET_BIN_THRESHOLD {
                        visited[nidx] = true;
                        stack.push(nidx);
                    }
                }
            }
        }
        let bw = max_x - min_x + 1;
        let bh = max_y - min_y + 1;
        if bw < DET_MIN_SIDE || bh < DET_MIN_SIDE {
            continue;
        }
        #[allow(clippy::cast_precision_loss)]
        let mean = score_sum / count as f32;
        if mean < DET_BOX_THRESHOLD {
            continue;
        }
        out.push(Rect {
            x0: min_x as f32,
            y0: min_y as f32,
            x1: (max_x + 1) as f32,
            y1: (max_y + 1) as f32,
        });
    }
    out
}

/// 按「面积 × ratio / 周长」外扩（unclip 的轴对齐近似）。
fn unclip(rect: &mut Rect, ratio: f32, max_w: f32, max_h: f32) {
    let w = rect.width();
    let h = rect.height();
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let distance = (w * h * ratio) / (2.0 * (w + h));
    rect.x0 = (rect.x0 - distance).clamp(0.0, max_w);
    rect.y0 = (rect.y0 - distance).clamp(0.0, max_h);
    rect.x1 = (rect.x1 + distance).clamp(0.0, max_w);
    rect.y1 = (rect.y1 + distance).clamp(0.0, max_h);
}

/// CTC 贪心解码：逐时刻取 argmax，去重去 blank，越界索引丢弃。
fn ctc_greedy_decode(
    probs: &[f32],
    steps: usize,
    classes: usize,
    keys: &[String],
) -> (String, f32) {
    if steps == 0 || classes == 0 || probs.len() < steps * classes {
        return (String::new(), 0.0);
    }
    let mut text = String::new();
    let mut prev = usize::MAX;
    let mut prob_sum = 0f32;
    let mut prob_count = 0usize;
    for t in 0..steps {
        let row = &probs[t * classes..(t + 1) * classes];
        let mut best = 0usize;
        let mut best_p = f32::NEG_INFINITY;
        for (i, &p) in row.iter().enumerate() {
            if p > best_p {
                best_p = p;
                best = i;
            }
        }
        if best != 0 && best != prev {
            if let Some(ch) = decode_index(best, keys) {
                text.push_str(&ch);
                prob_sum += best_p;
                prob_count += 1;
            }
        }
        prev = best;
    }
    let confidence = if prob_count == 0 {
        0.0
    } else {
        prob_sum / prob_count as f32
    };
    (text, confidence)
}

/// 索引映射：0 = blank，1..=keys.len() = 字典，keys.len()+1 = 空格。
fn decode_index(index: usize, keys: &[String]) -> Option<String> {
    if index == 0 {
        return None;
    }
    let key_idx = index - 1;
    if key_idx < keys.len() {
        Some(keys[key_idx].clone())
    } else if key_idx == keys.len() {
        Some(" ".to_string())
    } else {
        None
    }
}

/// 按 y-x 阅读序拼装：行内空格连接、行间换行；boxes 同步按阅读序排列。
fn assemble(boxes: Vec<RecognizedBox>, img: &RgbImage) -> OcrOutput {
    if boxes.is_empty() {
        return OcrOutput::empty();
    }
    let (iw, ih) = img.dimensions();
    let mut sorted = boxes;
    sorted.sort_by(|a, b| {
        a.rect
            .center_y()
            .partial_cmp(&b.rect.center_y())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.rect
                    .x0
                    .partial_cmp(&b.rect.x0)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let mut lines: Vec<Vec<usize>> = Vec::new();
    for (i, b) in sorted.iter().enumerate() {
        let mut placed = false;
        if let Some(line) = lines.last_mut() {
            let anchor = &sorted[line[0]];
            let overlap = (anchor.rect.y1.min(b.rect.y1) - anchor.rect.y0.max(b.rect.y0)).max(0.0);
            let min_h = anchor.rect.height().min(b.rect.height()).max(1.0);
            if overlap / min_h > 0.5 {
                line.push(i);
                placed = true;
            }
        }
        if !placed {
            lines.push(vec![i]);
        }
    }

    let mut text_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut ordered: Vec<usize> = Vec::with_capacity(sorted.len());
    for line in &mut lines {
        line.sort_by(|&a, &b| {
            sorted[a]
                .rect
                .x0
                .partial_cmp(&sorted[b].rect.x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let joined = line
            .iter()
            .map(|&i| sorted[i].text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if !joined.trim().is_empty() {
            text_lines.push(joined);
            ordered.extend(line.iter().copied());
        }
    }
    if ordered.is_empty() {
        return OcrOutput::empty();
    }

    let mut out_boxes = Vec::with_capacity(ordered.len());
    let mut conf_sum = 0f32;
    for &i in &ordered {
        let b = &sorted[i];
        if let Some(obox) = OcrBox::normalized(
            b.text.clone(),
            b.rect.x0,
            b.rect.y0,
            b.rect.width(),
            b.rect.height(),
            iw as f32,
            ih as f32,
        ) {
            out_boxes.push(obox);
        }
        conf_sum += b.confidence;
    }

    #[allow(clippy::cast_precision_loss)]
    let confidence = conf_sum / ordered.len() as f32;
    OcrOutput {
        text: Some(text_lines.join("\n")),
        boxes: out_boxes,
        confidence: Some(confidence.clamp(0.0, 1.0)),
    }
}

fn load_keys(path: &Path) -> Result<Vec<String>, AppError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|_| AppError::FileMissing(path.display().to_string()))?;
    let keys: Vec<String> = raw
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .filter(|line| !line.is_empty())
        .collect();
    if keys.is_empty() {
        return Err(AppError::InvalidInput(format!(
            "ppocr: empty dictionary {}",
            path.display()
        )));
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<String> {
        vec!["你".to_string(), "好".to_string(), "A".to_string()]
    }

    #[test]
    fn ctc_decode_drops_blanks_and_repeats() {
        // 词表 3 + blank + space = 5 类
        let classes = 5;
        // 时间步: 1(你) 1(你,重复) 0(blank) 2(好) 4(space) 3(A)
        let steps = 6;
        let mut probs = vec![0f32; steps * classes];
        let seq = [1usize, 1, 0, 2, 4, 3];
        for (t, &cls) in seq.iter().enumerate() {
            probs[t * classes + cls] = 0.9;
        }
        let (text, conf) = ctc_greedy_decode(&probs, steps, classes, &keys());
        assert_eq!(text, "你好 A");
        assert!((conf - 0.9).abs() < 1e-6);
    }

    #[test]
    fn ctc_decode_out_of_range_index_is_ignored() {
        let classes = 8;
        let mut probs = vec![0f32; classes];
        probs[7] = 1.0; // 索引 7 超出 字典(3)+blank+space=5
        let (text, _) = ctc_greedy_decode(&probs, 1, classes, &keys());
        assert!(text.is_empty());
    }

    #[test]
    fn empty_prob_map_yields_no_boxes() {
        let probs = vec![0.0f32; 32 * 32];
        assert!(extract_boxes(&probs, 32, 32).is_empty());
    }

    #[test]
    fn connected_component_produces_axis_aligned_box() {
        let (w, h) = (32usize, 32usize);
        let mut probs = vec![0.0f32; w * h];
        for y in 8..16 {
            for x in 4..20 {
                probs[y * w + x] = 0.9;
            }
        }
        let boxes = extract_boxes(&probs, w, h);
        assert_eq!(boxes.len(), 1);
        let b = boxes[0];
        assert!((b.x0 - 4.0).abs() < 1e-6);
        assert!((b.y0 - 8.0).abs() < 1e-6);
        assert!((b.width() - 16.0).abs() < 1e-6);
        assert!((b.height() - 8.0).abs() < 1e-6);
    }

    #[test]
    fn low_score_component_is_filtered() {
        let (w, h) = (16usize, 16usize);
        let mut probs = vec![0.0f32; w * h];
        for y in 4..10 {
            for x in 4..10 {
                probs[y * w + x] = 0.35; // 过二值化阈值但均值 < 0.5
            }
        }
        assert!(extract_boxes(&probs, w, h).is_empty());
    }

    #[test]
    fn unclip_expands_and_clamps() {
        let mut r = Rect {
            x0: 10.0,
            y0: 10.0,
            x1: 30.0,
            y1: 20.0,
        };
        unclip(&mut r, 1.5, 100.0, 100.0);
        assert!(r.x0 < 10.0 && r.y0 < 10.0);
        assert!(r.x1 > 30.0 && r.y1 > 20.0);

        let mut edge = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 5.0,
            y1: 5.0,
        };
        unclip(&mut edge, 1.5, 100.0, 100.0);
        assert!(edge.x0 >= 0.0 && edge.y0 >= 0.0);
    }

    #[test]
    fn assemble_groups_boxes_into_lines() {
        let img = RgbImage::new(100, 100);
        let boxes = vec![
            RecognizedBox {
                rect: Rect {
                    x0: 50.0,
                    y0: 10.0,
                    x1: 80.0,
                    y1: 20.0,
                },
                text: "world".into(),
                confidence: 0.9,
            },
            RecognizedBox {
                rect: Rect {
                    x0: 10.0,
                    y0: 10.5,
                    x1: 40.0,
                    y1: 20.5,
                },
                text: "hello".into(),
                confidence: 0.8,
            },
            RecognizedBox {
                rect: Rect {
                    x0: 10.0,
                    y0: 40.0,
                    x1: 40.0,
                    y1: 50.0,
                },
                text: "second".into(),
                confidence: 0.7,
            },
        ];
        let out = assemble(boxes, &img);
        assert_eq!(out.text.as_deref(), Some("hello world\nsecond"));
        assert_eq!(out.boxes.len(), 3);
        assert_eq!(out.boxes[0].text, "hello");
        let conf = out.confidence.unwrap();
        assert!((conf - 0.8).abs() < 1e-6);
    }

    #[test]
    fn assemble_empty_is_empty_output() {
        let out = assemble(Vec::new(), &RgbImage::new(10, 10));
        assert!(!out.has_text());
        assert!(out.boxes.is_empty());
        assert!(
            out.confidence.is_none(),
            "无文字时置信度必须是 None 而不是 0"
        );
    }
}

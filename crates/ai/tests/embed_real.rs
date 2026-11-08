//! 需要真实模型文件的嵌入用例（`models/bge-small-zh-v1.5-int8.onnx` + `tokenizer.json`）。
//!
//! CI 无模型时跳过（`cargo test -p visrecall-ai` 不跑这些）；本地验收前必须跑
//! `cargo test -p visrecall-ai -- --ignored`（tasks.md Global Constraints）。

use std::time::Instant;

use visrecall_ai::embedder::{BgeEmbedder, CACHE_CAPACITY, DIM, MAX_TOKENS, MODEL_ID};
use visrecall_domain::providers::EmbeddingProvider;

fn embedder() -> BgeEmbedder {
    let dir = visrecall_ai::models::resolve_model_dir()
        .expect("模型目录缺失：先准备 models/（见 models/manifest.json）");
    let embedder = BgeEmbedder::new(dir);
    embedder.warm_up().expect("模型加载失败");
    assert!(embedder.ready(), "warm_up 之后 ready() 必须为真");
    embedder
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[test]
#[ignore = "needs real model files under models/"]
fn dimension_and_id_are_pinned() {
    let embedder = embedder();
    assert_eq!(embedder.id(), MODEL_ID);
    assert_eq!(embedder.dim(), DIM);

    let vector = embedder.embed_one("季度营收漏斗图").expect("嵌入失败");
    assert_eq!(vector.len(), DIM);
    // L2 归一化：模长必须接近 1
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4, "norm={norm}");
}

#[test]
#[ignore = "needs real model files under models/"]
fn same_text_gives_identical_vector() {
    let embedder = embedder();
    let a = embedder.embed_one("发票号码 00329181").unwrap();
    let b = embedder.embed_one("发票号码 00329181").unwrap();
    let max_diff = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert_eq!(max_diff, 0.0, "同一文本两次嵌入必须逐位相同");
}

#[test]
#[ignore = "needs real model files under models/"]
fn long_text_is_truncated_to_max_tokens() {
    let embedder = embedder();
    let long = "这是一段很长的界面文字".repeat(400);
    assert!(long.chars().count() > MAX_TOKENS * 3);
    let vector = embedder.embed_one(&long).expect("超长文本不应报错");
    assert_eq!(vector.len(), DIM);
}

#[test]
#[ignore = "needs real model files under models/"]
fn latency_p95_is_under_30ms() {
    let embedder = embedder();
    let texts = [
        "系统设置页面的深色主题配色",
        "浏览器地址栏与书签栏",
        "代码编辑器的报错提示与堆栈",
        "微信聊天窗口的图片消息",
        "发票号码和开票日期",
        "季度营收漏斗图",
        "产品定价页面的三个套餐",
        "数据库连接超时的错误日志",
        "折线图显示近七天的访问量",
        "终端里执行的 git 提交命令",
    ];
    // 预热一轮，避免首次分配影响采样
    let _ = embedder.embed_one("预热").unwrap();

    let mut samples = Vec::new();
    for text in texts {
        let started = Instant::now();
        let _ = embedder.embed_one(text).unwrap();
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!(!samples.is_empty());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let p95_index = ((samples.len() as f64 * 0.95).ceil() as usize).clamp(1, samples.len()) - 1;
    let p95 = samples[p95_index];
    let max = *samples.last().unwrap();
    println!("embed latency: p95={p95:.2}ms max={max:.2}ms");
    assert!(p95 < 30.0, "单条嵌入 P95 必须 <30ms，实测 {p95:.2}ms");
}

#[test]
#[ignore = "needs real model files under models/"]
fn related_texts_are_closer_than_unrelated() {
    let embedder = embedder();
    let pairs = [
        (
            "一张关于定价的深色界面",
            "产品定价页面的三个套餐",
            "任务管理器中的内存占用",
        ),
        (
            "报错截图",
            "代码编辑器的报错提示与堆栈",
            "浅色背景的文档阅读界面",
        ),
        ("发票信息", "发票号码和开票日期", "拖拽排序的看板列"),
        (
            "聊天记录",
            "微信聊天窗口的图片消息",
            "折线图显示近七天的访问量",
        ),
        (
            "数据库连接失败",
            "数据库连接超时的错误日志",
            "左侧导航栏包含工作台与数据集",
        ),
        (
            "图表统计",
            "带有饼图占比的统计页面",
            "中文输入法的候选词面板",
        ),
        ("登录页面", "密码框与登录按钮", "命令行输出中的版本号信息"),
        (
            "文件管理",
            "文件资源管理器中的下载文件夹",
            "接口调试工具的请求参数",
        ),
        (
            "服务器监控",
            "监控大盘的告警指标",
            "会议纪要中的待办事项列表",
        ),
        (
            "设计预览",
            "带水印的设计稿预览",
            "日志中打印的 SQL 查询语句",
        ),
    ];
    let mut passed = 0;
    for (query, related, unrelated) in pairs {
        let q = embedder.embed_one(query).unwrap();
        let r = embedder.embed_one(related).unwrap();
        let u = embedder.embed_one(unrelated).unwrap();
        if cosine(&q, &r) > cosine(&q, &u) {
            passed += 1;
        }
    }
    assert_eq!(
        passed,
        pairs.len(),
        "语义 sanity 必须全通过，实际 {passed}/10"
    );
}

#[test]
#[ignore = "needs real model files under models/"]
fn query_cache_short_circuits_recomputation() {
    let embedder = embedder();
    let text = "缓存命中测试：季度营收漏斗图".to_string();

    let cold = Instant::now();
    let first = embedder.embed_sync(std::slice::from_ref(&text)).unwrap();
    let cold_ms = cold.elapsed().as_secs_f64() * 1000.0;

    let warm = Instant::now();
    let second = embedder.embed_sync(std::slice::from_ref(&text)).unwrap();
    let warm_ms = warm.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(first, second, "缓存不得改变结果");
    println!("cache: cold={cold_ms:.2}ms warm={warm_ms:.4}ms (capacity={CACHE_CAPACITY})");
    assert!(
        warm_ms < cold_ms / 2.0,
        "第二次应显著更快（cold={cold_ms:.2}ms warm={warm_ms:.4}ms）"
    );
}

#[test]
#[ignore = "needs real model files under models/"]
fn batch_embed_returns_one_vector_per_text() {
    let embedder = embedder();
    let texts = vec![
        "第一段文本".to_string(),
        "第二段文本".to_string(),
        String::new(),
    ];
    let vectors = embedder.embed_sync(&texts).unwrap();
    assert_eq!(vectors.len(), 3);
    for vector in vectors {
        assert_eq!(vector.len(), DIM);
    }
}

//! 分词 + FTS 查询构造的集成测试（tempdir + 真实 SQLite）。
//!
//! 覆盖 tasks.md Task 3 Step 4：词形一致（写入查得回）、`build_match_expr` 安全、
//! `build_search_document` 截断行为、10 条真实中文查询的字面召回。

use visrecall_domain::{ItemKind, NewItem};
use visrecall_library::{FtsColumns, Library};
use visrecall_search::document::{build_search_document, DocumentFields, DOC_CHAR_BUDGET};
use visrecall_search::fts_query::{build_match_expr, escape_phrase};
use visrecall_search::tokenizer::{terms_for_query, tokenize_for_fts};

/// `(标题, OCR/原文)`：与 T0 spike 同一批中文素材，便于对照。
const DOCS: [(&str, &str); 20] = [
    ("季度营收漏斗图", "第三季度营收漏斗图显示转化率提升"),
    (
        "发票报销明细",
        "发票号码 00329181 开票日期 2026-10-11 金额 1280.00",
    ),
    ("登录界面深色主题", "输入用户名和密码后点击登录按钮"),
    ("数据库连接超时", "Error 502 Bad Gateway 数据库连接池耗尽"),
    ("内存占用监控", "任务管理器显示内存占用 78%，CPU 使用率偏高"),
    ("产品定价方案", "基础版、专业版、企业版三档定价"),
    ("Git 提交记录", "git commit -m 修复登录接口的边界条件"),
    ("微信聊天记录", "对方发送了一张截图并询问进度"),
    (
        "代码报错堆栈",
        "NullPointerException at com.example.OrderService.calculate",
    ),
    ("表格数据统计", "本月订单数量与退款数量对比"),
    ("折线图访问趋势", "近七天站点访问量折线图"),
    ("文件下载列表", "下载文件夹中的安装包与压缩包"),
    ("会议纪要待办", "待办事项：周三前提交周报"),
    ("服务器告警通知", "磁盘使用率超过阈值 90%"),
    ("设计稿预览水印", "设计稿预览带水印，禁止外传"),
    ("接口调试参数", "POST /api/v1/orders 请求参数 userId=42"),
    ("中文输入法候选", "输入法候选词面板显示拼音"),
    ("监控大盘告警指标", "告警指标 P99 延迟 320ms"),
    ("二维码支付页面", "扫码支付，应付金额 68.5 元"),
    ("深色模式设置页面", "外观设置：跟随系统、深色、浅色"),
];

const QUERIES: [(&str, &str); 10] = [
    ("发票", "发票报销明细"),
    ("漏斗图", "季度营收漏斗图"),
    ("数据库连接", "数据库连接超时"),
    ("502", "数据库连接超时"),
    ("内存占用", "内存占用监控"),
    ("定价", "产品定价方案"),
    ("git 提交", "Git 提交记录"),
    ("周报", "会议纪要待办"),
    ("水印", "设计稿预览水印"),
    ("二维码支付", "二维码支付页面"),
];

struct Index {
    _dir: tempfile::TempDir,
    library: Library,
    /// `items_fts.rowid` 与 `title` 的对应（rowid 从 1 起，与 DOCS 顺序一致）。
    titles: Vec<String>,
}

fn build_index() -> Index {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path().to_path_buf()).unwrap();
    let mut titles = Vec::new();
    for (i, (title, body)) in DOCS.iter().enumerate() {
        let item = library
            .insert_item(NewItem {
                kind: ItemKind::Image,
                captured_at: 1_700_000_000_000 + i as i64,
                source_app: None,
                source_title: Some((*title).to_string()),
                source_url: None,
                width: None,
                height: None,
                char_count: None,
                original_path: format!("originals/images/2026/11/{i}.png"),
                content_hash: format!("hash-{i}"),
            })
            .unwrap();
        let rowid = library.item_rowid(&item.id).unwrap().unwrap();
        library
            .fts_upsert(
                rowid,
                &FtsColumns {
                    title: &tokenize_for_fts(title),
                    ocr_text: &tokenize_for_fts(body),
                    ..Default::default()
                },
            )
            .unwrap();
        titles.push((*title).to_string());
    }
    Index {
        _dir: dir,
        library,
        titles,
    }
}

#[test]
fn ten_real_chinese_queries_hit_the_right_documents() {
    let index = build_index();
    let mut failures = Vec::new();
    for (query, expected) in QUERIES {
        let terms = terms_for_query(query);
        let expr = build_match_expr(&terms, &[]);
        assert!(!expr.is_empty(), "查询 {query} 不应产生空表达式");
        let hits = index.library.fts_search(&expr, None, 10).unwrap();
        let found: Vec<&str> = hits
            .iter()
            .filter_map(|hit| {
                index
                    .titles
                    .get((hit.rowid - 1) as usize)
                    .map(String::as_str)
            })
            .collect();
        if !found.contains(&expected) {
            failures.push(format!(
                "query={query} expr={expr} expected={expected} got={found:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "字面召回失败：\n{}",
        failures.join("\n")
    );
}

#[test]
fn query_terms_are_written_and_searchable() {
    let index = build_index();
    // 写入侧与查询侧词形一致：把素材标题的分词结果当查询词，必须命中它自己
    for (i, (title, _)) in DOCS.iter().enumerate() {
        let terms = terms_for_query(title);
        let expr = build_match_expr(&terms, &[]);
        let hits = index.library.fts_search(&expr, None, 20).unwrap();
        let rowid = (i + 1) as i64;
        assert!(
            hits.iter().any(|hit| hit.rowid == rowid),
            "标题 {title} 的自我查询应命中自己，expr={expr}"
        );
    }
}

#[test]
fn quoted_terms_require_all_words() {
    let index = build_index();
    // 引号内内容按 jieba 分词后 AND（真实链路里 parser 就是这样喂给 build_match_expr 的）
    let quoted = terms_for_query("季度 营收");
    let expr = build_match_expr(&[], &quoted);
    let hits = index.library.fts_search(&expr, None, 10).unwrap();
    let found: Vec<&str> = hits
        .iter()
        .filter_map(|hit| {
            index
                .titles
                .get((hit.rowid - 1) as usize)
                .map(String::as_str)
        })
        .collect();
    assert_eq!(
        found,
        vec!["季度营收漏斗图"],
        "expr={expr} indexed_title={:?} indexed_body={:?}",
        tokenize_for_fts(DOCS[0].0),
        tokenize_for_fts(DOCS[0].1)
    );

    // 单个必命中词：只要有它就算命中
    let expr = build_match_expr(&[], &["发票".to_string()]);
    let hits = index.library.fts_search(&expr, None, 10).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn syntax_characters_never_break_the_query() {
    let index = build_index();
    for raw in [
        "a*",
        "(",
        ")",
        "d:e",
        "-x",
        "\"",
        "AND OR NOT",
        "🚀",
        "^首",
        "a\"b",
        "NEAR(a b)",
    ] {
        let terms = terms_for_query(raw);
        let expr = build_match_expr(&terms, &[]);
        if expr.is_empty() {
            continue;
        }
        let result = index.library.fts_search(&expr, None, 10);
        assert!(result.is_ok(), "查询 {raw:?} → {expr} 报错：{result:?}");
    }
}

#[test]
fn escape_phrase_keeps_alnum_and_drops_control_chars() {
    assert_eq!(escape_phrase("发票"), Some("\"发票\"".to_string()));
    assert_eq!(escape_phrase("  "), None);
    assert_eq!(escape_phrase("，。"), None);
    assert_eq!(escape_phrase("a\"b"), Some("\"a\"\"b\"".to_string()));
    assert_eq!(escape_phrase("a\u{7}b"), Some("\"ab\"".to_string()));
}

#[test]
fn search_document_keeps_title_when_ocr_is_huge() {
    let ocr = "测".repeat(DOC_CHAR_BUDGET * 3);
    let doc = build_search_document(&DocumentFields {
        title: "发票报销明细",
        summary: "报销",
        tags: "财务",
        entities: "",
        note: "本月",
        ocr_or_original: &ocr,
    });
    assert_eq!(doc.chars().count(), DOC_CHAR_BUDGET);
    assert!(doc.starts_with("发票报销明细；报销；财务；本月；"));
}

#[test]
fn search_document_round_trips_through_the_index() {
    let index = build_index();
    // 用「标题 + OCR」构造的 document 分词后必须能命中对应素材
    let (title, body) = DOCS[0];
    let doc = build_search_document(&DocumentFields {
        title,
        ocr_or_original: body,
        ..Default::default()
    });
    let terms = terms_for_query(&doc);
    let expr = build_match_expr(&terms, &[]);
    let hits = index.library.fts_search(&expr, None, 5).unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].rowid, 1);
}

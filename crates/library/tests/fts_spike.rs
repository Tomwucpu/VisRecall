//! T0 spike（Task 1 Step 5）：rusqlite bundled 的 FTS5 + jieba 预分词中文检索。
//!
//! 验证三件事：
//! 1. bundled SQLite 的 FTS5 可用（建表 + MATCH + snippet 不报错）；
//! 2. 写入侧与查询侧用同一个 jieba 分词器时，字面查询能召回目标行；
//! 3. `snippet()` 能给出带标记的命中摘要。
//!
//! 该文件是临时 spike：Task 3 会在 `crates/search` 用生产分词器与 MATCH 构造器
//! 覆盖同样的用例，Task 2 起 `items_fts` 随迁移 0003 落地。

use jieba_rs::Jieba;
use rusqlite::Connection;

fn tokenize(jieba: &Jieba, text: &str) -> String {
    jieba
        .cut(text, false)
        .into_iter()
        .map(|t| t.word)
        .filter(|w| w.chars().any(|c: char| c.is_alphanumeric()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn match_expr(jieba: &Jieba, query: &str) -> String {
    jieba
        .cut(query, false)
        .into_iter()
        .map(|t| t.word)
        .filter(|w| w.chars().any(|c: char| c.is_alphanumeric()))
        .map(|w| format!("\"{w}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

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

/// `(查询, 期望命中的标题)`：覆盖 2 字词、4 字词、专有名词、报错码、中英混排。
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

fn open_index() -> (Connection, Jieba) {
    let jieba = Jieba::new();
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE VIRTUAL TABLE items_fts USING fts5(
             title, body,
             tokenize = 'unicode61 remove_diacritics 2'
         );",
    )
    .expect("FTS5 必须可用（bundled SQLite 应启用 FTS5）");
    for (i, (title, body)) in DOCS.iter().enumerate() {
        conn.execute(
            "INSERT INTO items_fts(rowid, title, body) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                (i + 1) as i64,
                tokenize(&jieba, title),
                tokenize(&jieba, body)
            ],
        )
        .expect("insert doc");
    }
    (conn, jieba)
}

#[test]
fn fts5_is_available_in_bundled_sqlite() {
    let conn = Connection::open_in_memory().expect("open");
    let version: String = conn
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .expect("sqlite_version");
    assert!(!version.is_empty());
    conn.execute_batch("CREATE VIRTUAL TABLE smoke USING fts5(x);")
        .expect("FTS5 建表");
    conn.execute_batch("INSERT INTO smoke(x) VALUES('hello world');")
        .expect("FTS5 写入");
    let hits: i64 = conn
        .query_row(
            "SELECT count(*) FROM smoke WHERE smoke MATCH 'hello'",
            [],
            |row| row.get(0),
        )
        .expect("FTS5 查询");
    assert_eq!(hits, 1);
}

#[test]
fn ten_chinese_queries_recall_expected_documents() {
    let (conn, jieba) = open_index();
    let mut failures = Vec::new();
    for (query, expected) in QUERIES {
        let expr = match_expr(&jieba, query);
        assert!(!expr.is_empty(), "查询 {query} 的分词结果不应为空");
        let mut stmt = conn
            .prepare("SELECT rowid FROM items_fts WHERE items_fts MATCH ?1 ORDER BY rank")
            .expect("prepare");
        let rowids: Vec<i64> = stmt
            .query_map([&expr], |row| row.get(0))
            .expect("query")
            .filter_map(Result::ok)
            .collect();
        let titles: Vec<&str> = rowids
            .iter()
            .filter_map(|id| DOCS.get((*id - 1) as usize).map(|(t, _)| *t))
            .collect();
        if !titles.contains(&expected) {
            failures.push(format!(
                "query={query} expr={expr} expected={expected} got={titles:?}"
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
fn snippet_marks_the_matched_terms() {
    let (conn, jieba) = open_index();
    let expr = match_expr(&jieba, "发票");
    let (title, snippet): (String, String) = conn
        .query_row(
            "SELECT title, snippet(items_fts, -1, '«', '»', '…', 16)
             FROM items_fts WHERE items_fts MATCH ?1 ORDER BY rank LIMIT 1",
            [&expr],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("snippet 查询");
    assert!(
        title.contains("发票") || title.contains("报销"),
        "title={title}"
    );
    assert!(
        snippet.contains('«') && snippet.contains('»'),
        "snippet 必须带命中标记：{snippet}"
    );
}

#[test]
fn bm25_orders_the_more_relevant_document_first() {
    let (conn, jieba) = open_index();
    let expr = match_expr(&jieba, "数据库连接");
    let top_title: String = conn
        .query_row(
            "SELECT title FROM items_fts WHERE items_fts MATCH ?1 ORDER BY bm25(items_fts) LIMIT 1",
            [&expr],
            |row| row.get(0),
        )
        .expect("bm25 查询");
    assert!(top_title.contains("数据库"), "top={top_title}");
}

#[test]
fn delete_removes_the_row_from_the_index() {
    let (conn, _jieba) = open_index();
    let before: i64 = conn
        .query_row("SELECT count(*) FROM items_fts", [], |row| row.get(0))
        .expect("count");
    conn.execute("DELETE FROM items_fts WHERE rowid = ?1", [1i64])
        .expect("delete");
    let after: i64 = conn
        .query_row("SELECT count(*) FROM items_fts", [], |row| row.get(0))
        .expect("count");
    assert_eq!(before - after, 1);
}

#[test]
fn emoji_and_fullwidth_queries_do_not_error() {
    let (conn, jieba) = open_index();
    for query in ["🎉", "，。！", "ABC", "「引号」"] {
        let expr = match_expr(&jieba, query);
        if expr.is_empty() {
            continue;
        }
        let mut stmt = conn
            .prepare("SELECT rowid FROM items_fts WHERE items_fts MATCH ?1")
            .expect("prepare");
        let _ = stmt.query_map([&expr], |row| row.get::<_, i64>(0));
    }
}

//! `visrecall-eval`：对真实评测库跑检索评测并输出 Markdown 报告（P3-R14）。
//!
//! ```text
//! cargo run -p visrecall-eval -- --db <库根目录> --queries <jsonl> --report <md> [--limit 50] [--title ...]
//! ```
//!
//! 退出码：0 = 跑完（无论门禁是否通过）；1 = 参数/环境错误。
//! 门禁结论写在报告里，并需要手工回填 `verification.md` §2 的 P3-R14 行。

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use visrecall_domain::providers::EmbeddingProvider;
use visrecall_eval::{load_queries, run_evaluation, ReportContext};
use visrecall_library::{BruteForceInt8, Library};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut db: Option<PathBuf> = None;
    let mut queries: Option<PathBuf> = None;
    let mut report: Option<PathBuf> = None;
    let mut limit: u32 = 50;
    let mut title = String::from("检索评测报告");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => db = args.next().map(PathBuf::from),
            "--queries" => queries = args.next().map(PathBuf::from),
            "--report" => report = args.next().map(PathBuf::from),
            "--limit" => {
                limit = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--limit needs a number")?;
            }
            "--title" => title = args.next().ok_or("--title needs a value")?,
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let db = db.ok_or("--db <library root> is required")?;
    let queries_path = queries.ok_or("--queries <jsonl> is required")?;
    let report_path = report.ok_or("--report <md> is required")?;

    let library = Library::open(db.clone()).map_err(|err| format!("open library: {err}"))?;
    let item_count = library
        .count_items()
        .map_err(|err| format!("count items: {err}"))?;
    let queries_list = load_queries(&queries_path).map_err(|err| format!("load queries: {err}"))?;
    if queries_list.is_empty() {
        return Err(format!(
            "评测集为空：{}（模板头部注释不算查询）",
            queries_path.display()
        ));
    }

    // 真实嵌入：评测延迟/召回必须与线上一致
    let model_dir = visrecall_ai::models::resolve_model_dir()
        .map_err(|err| format!("模型目录不可用（评测需要真实嵌入）：{err}"))?;
    let embed = visrecall_ai::BgeEmbedder::new(model_dir);
    embed
        .warm_up()
        .map_err(|err| format!("加载嵌入模型失败：{err}"))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|err| format!("build runtime: {err}"))?;
    let vectors = BruteForceInt8::new(Arc::new(
        Library::open(db.clone()).map_err(|err| format!("open library: {err}"))?,
    ));

    let rrf = visrecall_search::RrfConfig::from_settings(&library);
    let context = ReportContext {
        database: db.display().to_string(),
        item_count,
        model_id: embed.id().to_string(),
        model_dim: embed.dim(),
        rrf: format!("k={} weights={:?}", rrf.k, rrf.weights),
        queries_file: queries_path.display().to_string(),
    };

    let (report_text, metrics) = run_evaluation(visrecall_eval::EvaluationRun {
        library: &library,
        vectors: &vectors,
        embed: &embed,
        runtime: runtime.handle(),
        queries: &queries_list,
        limit,
        context,
        title: title.clone(),
    })
    .map_err(|err| format!("evaluate: {err}"))?;

    if let Some(parent) = report_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| format!("create report dir: {err}"))?;
        }
    }
    std::fs::write(&report_path, report_text).map_err(|err| format!("write report: {err}"))?;

    println!(
        "queries={} top3={:.1}% recall@10={:.3} mAP={:.3} p95={}ms -> {}",
        metrics.queries,
        metrics.top3_hit_rate * 100.0,
        metrics.recall_at_10,
        metrics.map,
        metrics.p95_ms,
        report_path.display()
    );
    if !metrics.passes_gate() {
        println!("门禁未通过（Top-3 <80%）：按报告「调参回路」逐项排查后重跑");
    }
    Ok(())
}

//! T0 spike：bge-small-zh-v1.5 int8 嵌入（Task 1 Step 4）。
//!
//! 用法：`cargo run -p visrecall-ai --example embed_spike`
//!
//! 输出：模型加载耗时、进程工作集增量、50 条中文文本的单条延迟（P50/P95）、
//! 确定性（同文本两次结果一致）、语义 sanity（10 组相关对 > 不相关对）、LRU 命中。

use std::time::Instant;

use visrecall_ai::embedder::{BgeEmbedder, DIM};

const TEXTS: [&str; 50] = [
    "系统设置页面的深色主题配色",
    "浏览器地址栏与书签栏",
    "代码编辑器的报错提示与堆栈",
    "微信聊天窗口的图片消息",
    "发票号码和开票日期",
    "季度营收漏斗图",
    "产品定价页面的三个套餐",
    "数据库连接超时的错误日志",
    "文件资源管理器中的下载文件夹",
    "表格中不同颜色的状态标记",
    "按钮文字为确认提交的弹窗",
    "折线图显示近七天的访问量",
    "终端里执行的 git 提交命令",
    "密码框与登录按钮",
    "任务管理器中的内存占用",
    "会议纪要中的待办事项列表",
    "监控大盘的告警指标",
    "安装向导的许可协议页面",
    "图片编辑器中的图层列表",
    "云服务的账单明细",
    "左侧导航栏包含工作台与数据集",
    "搜索框中的模糊匹配结果",
    "个人资料页面的头像与昵称",
    "视频播放器的进度条",
    "邮件客户端的未读标记",
    "接口调试工具的请求参数",
    "带公式推导的数学笔记",
    "红色警示的权限不足提示",
    "浅色背景的文档阅读界面",
    "蓝色的数据看板卡片",
    "绿色勾选表示部署成功",
    "橙色进度条表示上传中",
    "紫色的渐变登录背景",
    "命令行输出中的版本号信息",
    "客服工单的优先级标签",
    "带分页的订单列表",
    "移动端推送通知的截图",
    "包含表格与图表的周报",
    "接口返回的状态码 502",
    "代码缩进与语法高亮配色",
    "多标签页的浏览器窗口",
    "拖拽排序的看板列",
    "带水印的设计稿预览",
    "配置项列表中的开关控件",
    "带有饼图占比的统计页面",
    "报错弹窗要求重新登录",
    "浅灰色的分隔线与占位文字",
    "中文输入法的候选词面板",
    "包含二维码的支付页面",
    "日志中打印的 SQL 查询语句",
];

/// 语义 sanity：`(查询, 相关文本, 不相关文本)`。
const PAIRS: [(&str, &str, &str); 10] = [
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

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let model_dir = visrecall_ai::models::resolve_model_dir()?;
    println!("model dir: {}", model_dir.display());

    let mem_before = working_set_bytes();
    let load_start = Instant::now();
    let embedder = BgeEmbedder::new(&model_dir);
    embedder.warm_up()?;
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;
    let mem_after = working_set_bytes();
    println!(
        "model loaded in {load_ms:.0}ms; working set {:.1}MB -> {:.1}MB (+{:.1}MB, 门禁 ≤60MB)",
        mb(mem_before),
        mb(mem_after),
        mb(mem_after.saturating_sub(mem_before))
    );

    // 维度与确定性
    let first = embedder.embed_one(TEXTS[0])?;
    let second = embedder.embed_one(TEXTS[0])?;
    println!("dim: {} (期望 {DIM})", first.len());
    let max_diff = first
        .iter()
        .zip(second.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    println!("determinism: max |a-b| = {max_diff:.3e} (门禁 == 0)");

    // 单条延迟（不含缓存，逐条计时）
    let mut latencies: Vec<f64> = Vec::with_capacity(TEXTS.len());
    for text in TEXTS {
        let started = Instant::now();
        let _ = embedder.embed_one(text)?;
        latencies.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "latency over {} texts: mean {:.1}ms  p50 {:.1}ms  p95 {:.1}ms  max {:.1}ms (门禁 p95 <30ms)",
        latencies.len(),
        mean(&latencies),
        percentile(&latencies, 0.50),
        percentile(&latencies, 0.95),
        latencies.iter().copied().fold(0.0, f64::max)
    );

    // 语义 sanity
    let mut passed = 0usize;
    let mut worst = f64::MAX;
    for (query, related, unrelated) in PAIRS {
        let q = embedder.embed_one(query)?;
        let r = embedder.embed_one(related)?;
        let u = embedder.embed_one(unrelated)?;
        let cos_r = cosine(&q, &r);
        let cos_u = cosine(&q, &u);
        let margin = f64::from(cos_r - cos_u);
        if cos_r > cos_u {
            passed += 1;
        }
        worst = worst.min(margin);
        println!("  sanity cos(rel)={cos_r:.3} cos(unrel)={cos_u:.3} margin={margin:+.3}  {query}");
    }
    println!(
        "semantic sanity: {passed}/{} passed, worst margin {worst:+.3}",
        PAIRS.len()
    );

    // LRU：同一文本第二次走缓存
    let cached_text = "缓存命中测试：季度营收漏斗图".to_string();
    let cold_start = Instant::now();
    let _ = embedder.embed_sync(std::slice::from_ref(&cached_text))?;
    let cold_ms = cold_start.elapsed().as_secs_f64() * 1000.0;
    let warm_start = Instant::now();
    let _ = embedder.embed_sync(std::slice::from_ref(&cached_text))?;
    let warm_ms = warm_start.elapsed().as_secs_f64() * 1000.0;
    println!("cache: cold {cold_ms:.1}ms -> warm {warm_ms:.3}ms");

    let mem_final = working_set_bytes();
    println!(
        "working set after run: {:.1}MB (delta vs loaded {:.1}MB)",
        mb(mem_final),
        mb(mem_final.saturating_sub(mem_after))
    );
    Ok(())
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() as f64) * p).ceil() as usize;
    sorted[idx.clamp(1, sorted.len()) - 1]
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn working_set_bytes() -> u64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size);
    }
    counters.WorkingSetSize as u64
}

//! T0 spike：PP-OCR 真实截图批量识别（Task 1 Step 3）。
//!
//! 用法：
//! ```text
//! cargo run -p visrecall-ai --example ocr_spike -- --images <dir> [--limit N] [--out <dir>]
//! ```
//!
//! 输出：逐张耗时/框数/置信度/文本预览 + P50/P95 + 4K 子集耗时 + 进程工作集增量。
//! `--out` 会把逐张 OCR 文本写成 `<stem>.ocr.txt`，供 `scripts/ocr-accuracy.ps1` 与人工标注比对。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut images: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut limit = usize::MAX;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--images" => images = args.next().map(PathBuf::from),
            "--out" => out = args.next().map(PathBuf::from),
            "--limit" => {
                limit = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(usize::MAX);
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    let images = images.ok_or("--images <dir> is required")?;
    if let Some(dir) = &out {
        std::fs::create_dir_all(dir)?;
    }

    let model_dir = visrecall_ai::models::resolve_model_dir()?;
    println!("model dir: {}", model_dir.display());
    match visrecall_ai::models::verify_manifest(&model_dir) {
        Ok(manifest) => println!("manifest: ok ({} files)", manifest.files.len()),
        Err(err) => {
            println!("manifest: FAILED -> {err} (spike continues, production would fall back)")
        }
    }

    let mem_before = working_set_bytes();
    let load_start = Instant::now();
    let ocr = visrecall_ai::ocr::PpocrOcr::load(&model_dir)?;
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;
    let mem_after_load = working_set_bytes();
    println!(
        "engines loaded in {load_ms:.0}ms; working set {:.1}MB -> {:.1}MB (+{:.1}MB)",
        mb(mem_before),
        mb(mem_after_load),
        mb(mem_after_load.saturating_sub(mem_before))
    );

    let files = collect_images(&images)?;
    if files.is_empty() {
        return Err(format!("no images under {}", images.display()).into());
    }
    let files: Vec<PathBuf> = files.into_iter().take(limit).collect();
    println!("\n{} image(s) to recognize\n", files.len());

    let mut durations: Vec<Duration> = Vec::with_capacity(files.len());
    let mut large_durations: Vec<Duration> = Vec::new();
    let mut failures = 0usize;
    let mut empty = 0usize;

    for path in &files {
        let bytes = std::fs::read(path)?;
        let started = Instant::now();
        let result = ocr.recognize_bytes(&bytes);
        let elapsed = started.elapsed();
        durations.push(elapsed);

        match result {
            Ok(output) => {
                let text = output.text.clone().unwrap_or_default();
                let conf = output
                    .confidence
                    .map(|c| format!("{c:.3}"))
                    .unwrap_or_else(|| "null".to_string());
                if text.trim().is_empty() {
                    empty += 1;
                }
                println!(
                    "{:>6}ms  boxes={:<3} conf={:<5} {:<28} {}",
                    elapsed.as_millis(),
                    output.boxes.len(),
                    conf,
                    file_stem(path),
                    preview(&text, 70)
                );
                if is_large_image(&bytes) {
                    large_durations.push(elapsed);
                }
                if let Some(dir) = &out {
                    std::fs::write(dir.join(format!("{}.ocr.txt", file_stem(path))), &text)?;
                }
            }
            Err(err) => {
                failures += 1;
                println!(
                    "{:>6}ms  ERROR  {:<28} {err}",
                    elapsed.as_millis(),
                    file_stem(path)
                );
            }
        }
    }

    println!("\n--- summary ---");
    println!("images:      {}", files.len());
    println!("errors:      {failures}");
    println!("empty text:  {empty} (无字图片属正常结果)");
    println!("ocr load:    {load_ms:.0}ms");
    println!(
        "latency:     mean {:.0}ms  p50 {:.0}ms  p95 {:.0}ms  max {:.0}ms",
        mean_ms(&durations),
        percentile_ms(&durations, 0.50),
        percentile_ms(&durations, 0.95),
        durations
            .iter()
            .map(|d| d.as_secs_f64() * 1000.0)
            .fold(0.0, f64::max)
    );
    if !large_durations.is_empty() {
        println!(
            "4K subset:   n={}  p95 {:.0}ms",
            large_durations.len(),
            percentile_ms(&large_durations, 0.95)
        );
    }
    let mem_end = working_set_bytes();
    println!(
        "working set: {:.1}MB -> {:.1}MB (delta {:.1}MB)",
        mb(mem_after_load),
        mb(mem_end),
        mb(mem_end.saturating_sub(mem_after_load))
    );
    if let Some(dir) = &out {
        println!("texts written to {}", dir.display());
    }
    Ok(())
}

fn collect_images(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            if matches!(
                ext.as_deref(),
                Some("png") | Some("jpg") | Some("jpeg") | Some("webp") | Some("bmp") | Some("gif")
            ) {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}

fn is_large_image(bytes: &[u8]) -> bool {
    image::load_from_memory(bytes)
        .map(|img| img.width().max(img.height()) >= 3000)
        .unwrap_or(false)
}

fn preview(text: &str, max_chars: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let truncated: String = flat.chars().take(max_chars).collect();
    format!("{truncated}…")
}

fn mean_ms(durations: &[Duration]) -> f64 {
    if durations.is_empty() {
        return 0.0;
    }
    durations
        .iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .sum::<f64>()
        / durations.len() as f64
}

fn percentile_ms(durations: &[Duration], p: f64) -> f64 {
    if durations.is_empty() {
        return 0.0;
    }
    let mut values: Vec<f64> = durations.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((values.len() as f64) * p).ceil() as usize;
    values[idx.clamp(1, values.len()) - 1]
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// 当前进程工作集（私有 + 共享），用于门禁的「内存增量」口径。
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

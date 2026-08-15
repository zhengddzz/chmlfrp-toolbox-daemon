//! 日志文件管理：滚动文件日志 + 自动清理 + 尾部读取
//!
//! - 日志同时输出到 stdout（journald 收集）与 `{data_dir}/logs/daemon.log.YYYY-MM-DD`（按天滚动）
//! - 清理任务每天执行一次，删除修改时间超过保留天数的日志文件（默认 7 天，配置 `[log] retention_days`）
//! - `read_file_logs` 供 get_logs 命令读取尾部 N 行（时间正序），文件日志缺失时调用方回退 journalctl

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tracing::{info, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// 日志目录：`{data_dir}/logs`
pub fn log_dir(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("logs")
}

/// 初始化 tracing：stdout + 滚动文件双写
///
/// 文件写入失败（目录不可写等）时自动退回仅 stdout，不阻断启动。
/// 返回的 WorkerGuard 必须在 main 中保活，drop 后文件日志停止写入。
pub fn init_tracing(data_dir: Option<&str>) -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let appender = data_dir.and_then(|dir| {
        let dir = log_dir(dir);
        fs::create_dir_all(&dir).ok()?;
        Some(tracing_appender::rolling::daily(&dir, "daemon.log"))
    });

    match appender {
        Some(file_appender) => {
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            use tracing_subscriber::fmt::writer::MakeWriterExt;
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_writer(std::io::stdout.and(non_blocking))
                .init();
            Some(guard)
        }
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .init();
            None
        }
    }
}

/// 启动日志清理任务：立即清理一次，之后每天清理一次
pub fn start_cleanup_task(data_dir: String, retention_days: u64) {
    tokio::spawn(async move {
        loop {
            cleanup_old_logs(&log_dir(&data_dir), retention_days);
            tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}

/// 删除修改时间超过保留天数的日志文件（文件名形如 daemon.log.2026-08-15）
pub fn cleanup_old_logs(dir: &Path, retention_days: u64) {
    if retention_days == 0 {
        return; // 0 表示永不清理
    }
    let cutoff = SystemTime::now() - Duration::from_secs(retention_days * 24 * 60 * 60);
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // 目录不存在（尚未写过日志）
    };

    let mut removed = 0usize;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.starts_with("daemon.log") {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|mtime| mtime < cutoff)
            .unwrap_or(false);
        if expired {
            match fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) => warn!("[logs] 删除过期日志失败 {}: {}", name, e),
            }
        }
    }
    if removed > 0 {
        info!(
            "[logs] 已自动清理 {} 个超过 {} 天的日志文件",
            removed, retention_days
        );
    }
}

/// 读取日志文件尾部 N 行（时间正序：旧 → 新）
///
/// 返回 None 表示没有文件日志（调用方回退 journalctl）。
/// 滚动文件名按日期字典序排列，与时间序一致。
pub fn read_file_logs(dir: &Path, lines: usize) -> Option<String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("daemon.log"))
                .unwrap_or(false)
        })
        .collect();
    files.sort(); // daemon.log.2026-08-14 < daemon.log.2026-08-15

    let mut result: Vec<String> = Vec::new();
    // 从最新文件往前读，凑够 lines 行为止
    for path in files.iter().rev() {
        let file_lines: Vec<String> = File::open(path)
            .map(|f| BufReader::new(f).lines().map_while(|l| l.ok()).collect())
            .unwrap_or_default();
        if file_lines.is_empty() {
            continue;
        }
        if result.len() + file_lines.len() >= lines {
            // 当前文件已足够：取其尾部补齐，插入到已收集内容之前
            let need = lines - result.len();
            let start = file_lines.len().saturating_sub(need);
            let mut combined = file_lines[start..].to_vec();
            combined.extend(result);
            result = combined;
            break;
        }
        // 不够则整文件内容插入到已收集内容之前
        let mut combined = file_lines;
        combined.extend(result);
        result = combined;
    }

    if result.is_empty() {
        return None;
    }
    Some(result.join("\n"))
}

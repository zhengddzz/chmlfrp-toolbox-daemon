//! tcp_speed_test 命令 - TCP 测速客户端
//!
//! 与桌面客户端的 TCP 测速服务端协议匹配：
//!   1. 客户端连接 host:port
//!   2. 发送 ASCII 命令 `SPEEDTEST <size_mb>\n`
//!   3. 服务端循环发送 1MB 零字节数据块，达到 size_mb 后关闭连接
//!   4. 客户端统计接收字节数和耗时，计算下载速度
//!
//! 用于端对端测试：桌面客户端A创建临时隧道+测速服务端，
//! daemon B 通过 relay RPC 执行本命令连接 A 的隧道地址，
//! 测量 B→frp节点→A 的真实链路下载带宽。

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};
use tracing::info;

/// 进度推送间隔（毫秒）
const PROGRESS_INTERVAL_MS: u64 = 200;
/// 读缓冲区大小
const READ_BUF_SIZE: usize = 256 * 1024;
/// 默认测速数据量（MB）
const DEFAULT_SIZE_MB: usize = 10;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcpSpeedTestParams {
    /// 目标主机（节点 IP 或域名）
    host: String,
    /// 目标端口（隧道远程端口）
    port: u16,
    /// 请求下载的数据量（MB），默认 10
    #[serde(default = "default_size_mb")]
    size_mb: usize,
    /// 连接超时（秒），默认 10
    #[serde(default = "default_connect_timeout")]
    connect_timeout_secs: u64,
    /// 读超时（秒），默认 60
    #[serde(default = "default_read_timeout")]
    read_timeout_secs: u64,
}

fn default_size_mb() -> usize {
    DEFAULT_SIZE_MB
}

fn default_connect_timeout() -> u64 {
    10
}

fn default_read_timeout() -> u64 {
    60
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TcpSpeedTestResult {
    success: bool,
    speed_mbps: f64,
    total_bytes: u64,
    duration_ms: u64,
    error: Option<String>,
}

/// 推送进度（非关键错误忽略）
/// 使用 try_lock 避免在 spawn_blocking 中 await
fn send_progress(
    ctx: &super::CommandContext,
    progress: f64,
    stage: &str,
    speed_mbps: f64,
) {
    if let Ok(tx) = ctx.progress_tx.try_lock() {
        if let Some(sender) = tx.as_ref() {
            let _ = sender.send(super::ProgressPayload {
                request_id: ctx.request_id.clone(),
                progress,
                stage: stage.to_string(),
                speed_mbps,
            });
        }
    }
}

pub async fn handle(
    params: &serde_json::Value,
    ctx: &super::CommandContext,
) -> super::CommandResult {
    let p: TcpSpeedTestParams = serde_json::from_value(params.clone())
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("参数解析失败: {}", e)))?;

    if p.host.is_empty() {
        return Err(super::RpcError::new("INVALID_PARAMS", "host 不能为空"));
    }
    if p.port == 0 {
        return Err(super::RpcError::new("INVALID_PARAMS", "port 不能为 0"));
    }

    info!(
        "[tcp_speed_test] {}:{} size={}MB",
        p.host, p.port, p.size_mb
    );

    send_progress(ctx, 0.0, "connecting", 0.0);

    let host = p.host.clone();
    let port = p.port;
    let size_mb = p.size_mb;
    let connect_timeout = p.connect_timeout_secs;
    let read_timeout = p.read_timeout_secs;
    let request_id = ctx.request_id.clone();
    let progress_tx = ctx.progress_tx.clone();

    let result = tokio::task::spawn_blocking(move || {
        run_tcp_speed_test(&host, port, size_mb, connect_timeout, read_timeout, &request_id, &progress_tx)
    })
    .await
    .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("任务执行失败: {}", e)))?;

    match result {
        Ok(r) => {
            if r.success {
                send_progress(ctx, 100.0, "completed", r.speed_mbps);
            }
            serde_json::to_value(r)
                .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("序列化失败: {}", e)))
        }
        Err(e) => {
            send_progress(ctx, 100.0, "error", 0.0);
            Err(super::RpcError::new("EXEC_FAILED", e))
        }
    }
}

/// 执行 TCP 测速（同步阻塞，在 spawn_blocking 中运行）
fn run_tcp_speed_test(
    host: &str,
    port: u16,
    size_mb: usize,
    connect_timeout_secs: u64,
    read_timeout_secs: u64,
    request_id: &str,
    progress_tx: &std::sync::Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<super::ProgressPayload>>>>,
) -> Result<TcpSpeedTestResult, String> {
    let target_bytes = (size_mb as u64) * 1024 * 1024;

    // 解析地址
    let addr_str = format!("{}:{}", host, port);
    let socket_addr = addr_str
        .to_socket_addrs()
        .map_err(|e| format!("解析地址失败: {}", e))?
        .next()
        .ok_or_else(|| "无法解析主机地址".to_string())?;

    let start = Instant::now();

    // 建立连接
    let mut stream = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(connect_timeout_secs))
        .map_err(|e| format!("连接失败: {}", e))?;

    // 设置超时
    let read_timeout = Duration::from_secs(read_timeout_secs);
    stream.set_read_timeout(Some(read_timeout)).ok();
    stream.set_write_timeout(Some(read_timeout)).ok();

    // 发送测速命令
    let cmd = format!("SPEEDTEST {}\n", size_mb);
    stream.write_all(cmd.as_bytes()).map_err(|e| format!("发送命令失败: {}", e))?;

    // 接收数据
    let mut buf = vec![0u8; READ_BUF_SIZE];
    let mut received: u64 = 0;
    let mut last_progress = Instant::now();

    loop {
        match stream.read(&mut buf) {
            Ok(0) => break, // EOF，服务端发完关闭连接
            Ok(n) => {
                received += n as u64;

                // 定期推送进度
                if last_progress.elapsed() >= Duration::from_millis(PROGRESS_INTERVAL_MS) {
                    let progress = if target_bytes > 0 {
                        (received as f64 / target_bytes as f64 * 100.0).min(99.0)
                    } else {
                        50.0
                    };
                    let elapsed_secs = start.elapsed().as_secs_f64();
                    let current_speed = if elapsed_secs > 0.0 {
                        (received as f64 * 8.0) / elapsed_secs / 1_000_000.0
                    } else {
                        0.0
                    };
                    push_progress(progress_tx, request_id, progress, "downloading", current_speed);
                    last_progress = Instant::now();
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Ok(TcpSpeedTestResult {
                    success: received > 0,
                    speed_mbps: calc_speed(received, start.elapsed()),
                    total_bytes: received,
                    duration_ms: start.elapsed().as_millis() as u64,
                    error: Some(format!("读取数据失败: {}", e)),
                });
            }
        }
    }

    let elapsed = start.elapsed();
    let speed_mbps = calc_speed(received, elapsed);

    Ok(TcpSpeedTestResult {
        success: received > 0,
        speed_mbps,
        total_bytes: received,
        duration_ms: elapsed.as_millis() as u64,
        error: None,
    })
}

/// 计算速度（Mbps）
fn calc_speed(bytes: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs > 0.0 {
        (bytes as f64 * 8.0) / secs / 1_000_000.0
    } else {
        0.0
    }
}

/// 推送进度到 relay（在 spawn_blocking 中使用 try_lock）
fn push_progress(
    tx: &std::sync::Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<super::ProgressPayload>>>>,
    request_id: &str,
    progress: f64,
    stage: &str,
    speed_mbps: f64,
) {
    if let Ok(guard) = tx.try_lock() {
        if let Some(sender) = guard.as_ref() {
            let _ = sender.send(super::ProgressPayload {
                request_id: request_id.to_string(),
                progress,
                stage: stage.to_string(),
                speed_mbps,
            });
        }
    }
}

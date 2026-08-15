//! tcp_speed_test 命令 - TCP 测速客户端
//!
//! 与桌面客户端的 TCP 测速服务端协议匹配：
//!   1. 客户端连接 host:port
//!   2. 发送 ASCII 命令 `SPEEDTEST_TIME <duration_ms>\n`
//!   3. 服务端持续发送数据直到指定时长结束
//!   4. 客户端统计接收字节数和耗时，计算下载速度
//!
//! 用于端对端测试：桌面客户端A创建临时隧道+测速服务端，
//! daemon B 通过 relay RPC 执行本命令连接 A 的隧道地址，
//! 测量 B→frp节点→A 的真实链路下载带宽。

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};
use tracing::info;

/// 进度推送间隔（毫秒）
const PROGRESS_INTERVAL_MS: u64 = 200;
/// 读缓冲区大小
const READ_BUF_SIZE: usize = 256 * 1024;
const FIRST_PACKET_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcpSpeedTestParams {
    /// 目标主机（节点 IP 或域名）
    host: String,
    /// 目标端口（隧道远程端口）
    port: u16,
    duration_seconds: u64,
    /// 连接超时（秒），默认 10
    #[serde(default = "default_connect_timeout")]
    connect_timeout_secs: u64,
    /// 读超时（秒），默认 60
    #[serde(default = "default_read_timeout")]
    read_timeout_secs: u64,
    #[serde(default)]
    run_id: String,
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
    speed_samples: Vec<SpeedSample>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeedSample {
    second: usize,
    bytes: u64,
    duration_ms: u64,
    mbps: f64,
}

fn parse_params(params: &serde_json::Value) -> Result<TcpSpeedTestParams, String> {
    let parsed: TcpSpeedTestParams = serde_json::from_value(params.clone())
        .map_err(|error| format!("参数解析失败: {}", error))?;
    if !(5..=120).contains(&parsed.duration_seconds) {
        return Err("durationSeconds 必须在 5 到 120 之间".to_string());
    }
    Ok(parsed)
}

/// 推送进度（非关键错误忽略）
/// 使用 try_lock 避免在 spawn_blocking 中 await
fn send_progress(ctx: &super::CommandContext, progress: f64, stage: &str, speed_mbps: f64) {
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
    let p = parse_params(params).map_err(|error| super::RpcError::new("INVALID_PARAMS", error))?;

    if p.host.is_empty() {
        return Err(super::RpcError::new("INVALID_PARAMS", "host 不能为空"));
    }
    if p.port == 0 {
        return Err(super::RpcError::new("INVALID_PARAMS", "port 不能为 0"));
    }

    info!(
        "[tcp_speed_test] {}:{} duration={}s",
        p.host, p.port, p.duration_seconds
    );

    send_progress(ctx, 0.0, "connecting", 0.0);

    let host = p.host.clone();
    let port = p.port;
    let duration_seconds = p.duration_seconds;
    let connect_timeout = p.connect_timeout_secs;
    let read_timeout = p.read_timeout_secs;
    let request_id = ctx.request_id.clone();
    let progress_tx = ctx.progress_tx.clone();
    let run_id = p.run_id.clone();
    let account_id = ctx.account_id.clone();
    let generation = super::run_generation(&account_id, &run_id);

    let result = tokio::task::spawn_blocking(move || {
        run_tcp_speed_test(
            &host,
            port,
            duration_seconds,
            connect_timeout,
            read_timeout,
            &request_id,
            &account_id,
            &run_id,
            generation,
            &progress_tx,
        )
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
    duration_seconds: u64,
    connect_timeout_secs: u64,
    _read_timeout_secs: u64,
    request_id: &str,
    account_id: &str,
    run_id: &str,
    generation: u64,
    progress_tx: &std::sync::Arc<
        tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<super::ProgressPayload>>>,
    >,
) -> Result<TcpSpeedTestResult, String> {
    let addr_str = format!("{}:{}", host, port);
    let socket_addr = addr_str
        .to_socket_addrs()
        .map_err(|e| format!("解析地址失败: {}", e))?
        .next()
        .ok_or_else(|| "无法解析主机地址".to_string())?;
    run_speed_test(
        socket_addr,
        Duration::from_secs(duration_seconds),
        Duration::from_secs(connect_timeout_secs),
        Duration::from_millis(200),
        &|| super::is_run_cancelled(account_id, run_id, generation),
        &|progress, speed| push_progress(progress_tx, request_id, progress, "downloading", speed),
    )
}

fn run_speed_test<F, P>(
    socket_addr: SocketAddr,
    duration: Duration,
    connect_timeout: Duration,
    read_poll_interval: Duration,
    cancelled: &F,
    progress_callback: &P,
) -> Result<TcpSpeedTestResult, String>
where
    F: Fn() -> bool,
    P: Fn(f64, f64),
{
    let mut stream = TcpStream::connect_timeout(&socket_addr, connect_timeout)
        .map_err(|error| format!("连接失败: {}", error))?;
    stream.set_read_timeout(Some(read_poll_interval)).ok();
    stream.set_write_timeout(Some(connect_timeout)).ok();
    let command = format!("SPEEDTEST_TIME {}\n", duration.as_millis());
    stream
        .write_all(command.as_bytes())
        .map_err(|error| format!("发送命令失败: {}", error))?;

    let waiting_since = Instant::now();
    let mut buf = vec![0u8; READ_BUF_SIZE];
    let mut received: u64 = 0;
    let mut transfer_start: Option<Instant> = None;
    let mut last_progress: Option<Instant> = None;
    let mut sample_started_at: Option<Instant> = None;
    let mut sample_bytes = 0u64;
    let mut speed_samples = Vec::new();

    loop {
        if cancelled() {
            let _ = stream.shutdown(Shutdown::Both);
            return Err("测速已强制停止".to_string());
        }
        if transfer_start.is_none() && waiting_since.elapsed() >= FIRST_PACKET_TIMEOUT {
            let _ = stream.shutdown(Shutdown::Both);
            return Err("测速连接未返回数据".to_string());
        }
        if transfer_start.is_some_and(|started| started.elapsed() >= duration) {
            let _ = stream.shutdown(Shutdown::Both);
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) if transfer_start.is_none() => return Err("测速连接未返回数据".to_string()),
            Ok(0) => break,
            Ok(n) => {
                let now = Instant::now();
                let first = *transfer_start.get_or_insert(now);
                let progress_start = *last_progress.get_or_insert(first);
                let window_start = *sample_started_at.get_or_insert(first);
                received += n as u64;
                sample_bytes += n as u64;

                if now.duration_since(progress_start) >= Duration::from_millis(PROGRESS_INTERVAL_MS)
                {
                    let progress =
                        (first.elapsed().as_secs_f64() / duration.as_secs_f64() * 100.0).min(99.0);
                    let current_speed = calc_speed(sample_bytes, now.duration_since(window_start));
                    progress_callback(progress, current_speed);
                    last_progress = Some(now);
                }
                if now.duration_since(window_start) >= Duration::from_secs(1) {
                    let window = now.duration_since(window_start);
                    speed_samples.push(SpeedSample {
                        second: speed_samples.len() + 1,
                        bytes: sample_bytes,
                        duration_ms: window.as_millis() as u64,
                        mbps: calc_speed(sample_bytes, window),
                    });
                    sample_started_at = Some(now);
                    sample_bytes = 0;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(ref e)
                if (e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut) =>
            {
                continue
            }
            Err(_) if transfer_start.is_none() => return Err("测速连接未返回数据".to_string()),
            Err(e) => return Err(format!("读取数据失败: {}", e)),
        }
    }

    let elapsed = transfer_start
        .map(|value| value.elapsed())
        .unwrap_or_default();
    if sample_bytes > 0 {
        let window = sample_started_at
            .map(|value| value.elapsed())
            .unwrap_or_default();
        speed_samples.push(SpeedSample {
            second: speed_samples.len() + 1,
            bytes: sample_bytes,
            duration_ms: window.as_millis() as u64,
            mbps: calc_speed(sample_bytes, window),
        });
    }
    let speed_mbps = calc_speed(received, elapsed);
    let ended_early = elapsed + Duration::from_millis(250) < duration;

    Ok(TcpSpeedTestResult {
        success: received > 0 && !ended_early,
        speed_mbps,
        total_bytes: received,
        duration_ms: elapsed.as_millis() as u64,
        speed_samples,
        error: ended_early.then(|| "测速连接在目标时长前结束".to_string()),
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
    tx: &std::sync::Arc<
        tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<super::ProgressPayload>>>,
    >,
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

#[cfg(test)]
mod tests {
    use super::{calc_speed, parse_params, run_speed_test, FIRST_PACKET_TIMEOUT};
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn parses_duration_parameter() {
        let duration =
            parse_params(&serde_json::json!({"host":"127.0.0.1","port":1,"durationSeconds":15}))
                .unwrap();
        assert_eq!(duration.duration_seconds, 15);
    }

    #[test]
    fn rejects_duration_outside_limits() {
        assert!(parse_params(
            &serde_json::json!({"host":"127.0.0.1","port":1,"durationSeconds":4})
        )
        .is_err());
        assert!(parse_params(
            &serde_json::json!({"host":"127.0.0.1","port":1,"durationSeconds":121})
        )
        .is_err());
    }

    #[test]
    fn calculates_window_speed_and_protects_zero_duration() {
        assert_eq!(calc_speed(1_000_000, Duration::from_secs(1)), 8.0);
        assert_eq!(calc_speed(1_000_000, Duration::ZERO), 0.0);
    }

    #[test]
    fn first_packet_timeout_is_three_seconds() {
        assert_eq!(FIRST_PACKET_TIMEOUT, Duration::from_secs(3));
    }

    #[test]
    fn duration_protocol_stops_at_deadline_without_waiting_for_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut stream = BufReader::new(stream);
            let mut command = String::new();
            stream.read_line(&mut command).unwrap();
            assert!(command.starts_with("SPEEDTEST_TIME "));
            let payload = vec![0u8; 64 * 1024];
            while stream.get_mut().write_all(&payload).is_ok() {}
        });

        let started = Instant::now();
        let result = run_speed_test(
            address,
            Duration::from_millis(150),
            Duration::from_secs(1),
            Duration::from_millis(50),
            &|| false,
            &|_, _| {},
        )
        .unwrap();
        server.join().unwrap();

        assert!(result.success);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

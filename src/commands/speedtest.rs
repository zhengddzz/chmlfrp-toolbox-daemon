//! speedtest 命令 - HTTP 带宽测试
//!
//! 支持 download / upload 方向，持续时间内分块读写，
//! 通过 progress_tx 推送实时进度。

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const SPEEDTEST_CHUNK_SIZE: usize = 64 * 1024;
const SPEEDTEST_REPORT_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpeedtestParams {
    url: String,
    direction: String,
    #[serde(default = "default_duration")]
    duration_secs: u32,
    #[serde(default = "default_threads")]
    threads: u32,
}

fn default_duration() -> u32 {
    10
}

fn default_threads() -> u32 {
    4
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeedtestResult {
    success: bool,
    download_speed_mbps: f64,
    upload_speed_mbps: f64,
    latency_ms: Option<f64>,
    jitter_ms: Option<f64>,
    error: Option<String>,
}

/// 发送进度推送
async fn send_progress(
    ctx: &super::CommandContext,
    request_id: &str,
    progress: f64,
    stage: &str,
    speed_mbps: f64,
) {
    let tx = ctx.progress_tx.lock().await;
    if let Some(sender) = tx.as_ref() {
        let _ = sender.send(super::ProgressPayload {
            request_id: request_id.to_string(),
            progress,
            stage: stage.to_string(),
            speed_mbps,
        });
    }
}

pub async fn handle(
    params: &serde_json::Value,
    ctx: &super::CommandContext,
) -> super::CommandResult {
    let p: SpeedtestParams = serde_json::from_value(params.clone())
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("参数解析失败: {}", e)))?;

    let request_id = ctx.request_id.clone();

    let duration_secs = p.duration_secs.min(60);
    info!(
        "[speedtest] url={} direction={} duration={}s requestId={}",
        p.url, p.direction, duration_secs, request_id
    );

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel_flag.clone();
    let timeout_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs((duration_secs + 10) as u64)).await;
        cancel_clone.store(true, Ordering::SeqCst);
    });

    let result = match p.direction.as_str() {
        "download" | "both" => {
            run_download(&p.url, duration_secs, cancel_flag.clone(), ctx, &request_id).await
        }
        "upload" => run_upload(&p.url, duration_secs, cancel_flag.clone(), ctx, &request_id).await,
        _ => Err(format!(
            "不支持的方向: {}（可选 download/upload/both）",
            p.direction
        )),
    };

    timeout_handle.abort();

    match result {
        Ok((download_mbps, upload_mbps)) => Ok(serde_json::json!({
            "success": true,
            "downloadSpeedMbps": download_mbps,
            "uploadSpeedMbps": upload_mbps,
            "latencyMs": null,
            "jitterMs": null,
            "error": null,
        })),
        Err(e) => {
            warn!("[speedtest] 失败: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "downloadSpeedMbps": 0.0,
                "uploadSpeedMbps": 0.0,
                "latencyMs": null,
                "jitterMs": null,
                "error": e,
            }))
        }
    }
}

/// 下载测速
async fn run_download(
    url: &str,
    duration_secs: u32,
    cancel_flag: Arc<AtomicBool>,
    ctx: &super::CommandContext,
    request_id: &str,
) -> Result<(f64, f64), String> {
    use futures_util::StreamExt;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs((duration_secs + 15) as u64))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;

    send_progress(ctx, request_id, 0.0, "connecting", 0.0).await;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    send_progress(ctx, request_id, 5.0, "downloading", 0.0).await;

    let mut stream = response.bytes_stream();
    let mut total_bytes: u64 = 0;
    let start = Instant::now();
    let mut last_report = start;

    loop {
        if start.elapsed() >= Duration::from_secs(duration_secs as u64)
            || cancel_flag.load(Ordering::SeqCst)
        {
            break;
        }

        match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                total_bytes += chunk.len() as u64;
                let now = Instant::now();
                if now - last_report >= SPEEDTEST_REPORT_INTERVAL {
                    last_report = now;
                    let elapsed_secs = start.elapsed().as_secs_f64();
                    let speed = if elapsed_secs > 0.0 {
                        (total_bytes as f64 * 8.0) / elapsed_secs / 1_000_000.0
                    } else {
                        0.0
                    };
                    let progress = 5.0 + (elapsed_secs / duration_secs as f64) * 90.0;
                    send_progress(ctx, request_id, progress.min(95.0), "downloading", speed).await;
                }
            }
            Ok(Some(Err(e))) => {
                warn!("[speedtest] 下载读取错误: {}", e);
                break;
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    let speed_mbps = if elapsed > 0.0 {
        (total_bytes as f64 * 8.0) / elapsed / 1_000_000.0
    } else {
        0.0
    };

    send_progress(ctx, request_id, 100.0, "completed", speed_mbps).await;
    Ok((speed_mbps, 0.0))
}

/// 上传测速
async fn run_upload(
    url: &str,
    duration_secs: u32,
    cancel_flag: Arc<AtomicBool>,
    ctx: &super::CommandContext,
    request_id: &str,
) -> Result<(f64, f64), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs((duration_secs + 15) as u64))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;

    send_progress(ctx, request_id, 0.0, "connecting", 0.0).await;

    let chunk = vec![0u8; SPEEDTEST_CHUNK_SIZE];
    let start = Instant::now();

    // 分块上传，每次上传 1MB，检查时间
    let mut total_uploaded: u64 = 0;
    let mut last_report = Instant::now();

    loop {
        if start.elapsed() >= Duration::from_secs(duration_secs as u64)
            || cancel_flag.load(Ordering::SeqCst)
        {
            break;
        }

        // 构造请求体（一次上传 1MB）
        let body_data = chunk.repeat(16); // 1MB
        let body_len = body_data.len();

        match client.post(url).body(body_data).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    return Err(format!("HTTP {}", resp.status()));
                }
                total_uploaded += body_len as u64;
            }
            Err(e) => {
                warn!("[speedtest] 上传错误: {}", e);
                break;
            }
        }

        let now = Instant::now();
        if now - last_report >= SPEEDTEST_REPORT_INTERVAL {
            last_report = now;
            let elapsed_secs = start.elapsed().as_secs_f64();
            let speed = if elapsed_secs > 0.0 {
                (total_uploaded as f64 * 8.0) / elapsed_secs / 1_000_000.0
            } else {
                0.0
            };
            let progress = 5.0 + (elapsed_secs / duration_secs as f64) * 90.0;
            send_progress(ctx, request_id, progress.min(95.0), "uploading", speed).await;
        }
    }

    let elapsed_secs = start.elapsed().as_secs_f64();
    let speed_mbps = if elapsed_secs > 0.0 {
        (total_uploaded as f64 * 8.0) / elapsed_secs / 1_000_000.0
    } else {
        0.0
    };

    send_progress(ctx, request_id, 100.0, "completed", speed_mbps).await;
    Ok((0.0, speed_mbps))
}

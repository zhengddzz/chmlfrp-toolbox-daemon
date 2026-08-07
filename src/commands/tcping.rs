//! tcping 命令 - TCP 连接延迟测试

use serde::{Deserialize, Serialize};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};
use tracing::info;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcpingParams {
    host: String,
    port: u16,
    #[serde(default = "default_count")]
    count: u32,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_count() -> u32 {
    4
}

fn default_timeout() -> u64 {
    3
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TcpingResult {
    rtts: Vec<f64>,
    avg: Option<f64>,
    loss: u32,
}

/// 执行单次 TCP 连接，返回延迟（毫秒）
fn tcping_once(host: &str, port: u16, timeout_secs: u64) -> Result<f64, String> {
    let addr_str = format!("{}:{}", host, port);
    let socket_addr = addr_str
        .to_socket_addrs()
        .map_err(|e| format!("解析地址失败: {}", e))?
        .next()
        .ok_or_else(|| "无法解析主机地址".to_string())?;
    let start = Instant::now();
    match TcpStream::connect_timeout(&socket_addr, Duration::from_secs(timeout_secs)) {
        Ok(_) => Ok(start.elapsed().as_secs_f64() * 1000.0),
        Err(e) => Err(format!("TCP 连接失败: {}", e)),
    }
}

pub async fn handle(params: &serde_json::Value) -> super::CommandResult {
    let p: TcpingParams = serde_json::from_value(params.clone())
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("参数解析失败: {}", e)))?;

    info!("[tcping] {}:{} count={}", p.host, p.port, p.count);

    let host = p.host.clone();
    let port = p.port;
    let count = p.count;
    let timeout = p.timeout_secs;

    let result = tokio::task::spawn_blocking(move || {
        let mut rtts: Vec<f64> = Vec::new();
        let mut loss = 0u32;
        for _ in 0..count {
            match tcping_once(&host, port, timeout) {
                Ok(ms) => rtts.push(ms),
                Err(_) => loss += 1,
            }
        }
        let avg = if rtts.is_empty() {
            None
        } else {
            Some(rtts.iter().sum::<f64>() / rtts.len() as f64)
        };
        TcpingResult { rtts, avg, loss }
    })
    .await
    .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("任务执行失败: {}", e)))?;

    serde_json::to_value(result)
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("序列化失败: {}", e)))
}

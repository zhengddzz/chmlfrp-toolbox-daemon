//! ping 命令 - ICMP 延迟测试
//!
//! 使用系统 ping 命令，解析输出获取每次 reply 的 rtt。

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::process::Command;
use tracing::info;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PingParams {
    host: String,
    #[serde(default = "default_count")]
    count: u32,
}

fn default_count() -> u32 {
    4
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PingResult {
    rtts: Vec<f64>,
    min: Option<f64>,
    avg: Option<f64>,
    max: Option<f64>,
    loss: u32,
}

/// 执行 ping 命令
fn run_ping(host: &str, count: u32) -> (Vec<f64>, u32) {
    #[cfg(target_os = "linux")]
    let output = Command::new("ping")
        .arg("-c")
        .arg(count.to_string())
        .arg("-W")
        .arg("3")
        .arg(host)
        .output();

    #[cfg(target_os = "macos")]
    let output = Command::new("ping")
        .arg("-c")
        .arg(count.to_string())
        .arg("-W")
        .arg("3000")
        .arg(host)
        .output();

    #[cfg(target_os = "windows")]
    let output = Command::new("ping")
        .arg("-n")
        .arg(count.to_string())
        .arg("-w")
        .arg("3000")
        .arg(host)
        .output();

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let output: Result<std::process::Output, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Ping not supported on this platform",
    ));

    let output = match output {
        Ok(o) => o,
        Err(_) => return (vec![], count),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut rtts: Vec<f64> = Vec::new();

    // 匹配 "time=35.2 ms" 或 "time<1 ms" 或 "时间=35ms"
    let re = Regex::new(r"time[=<]\s*(\d+(?:\.\d+)?)\s*ms").unwrap();
    for cap in re.captures_iter(&stdout) {
        if let Ok(v) = cap[1].parse::<f64>() {
            rtts.push(v);
        }
    }

    let received = rtts.len() as u32;
    let loss = count.saturating_sub(received);
    (rtts, loss)
}

pub async fn handle(params: &serde_json::Value) -> super::CommandResult {
    let p: PingParams = serde_json::from_value(params.clone())
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("参数解析失败: {}", e)))?;

    info!("[ping] {} count={}", p.host, p.count);

    let host = p.host.clone();
    let count = p.count;
    let (rtts, loss) = tokio::task::spawn_blocking(move || run_ping(&host, count))
        .await
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("任务执行失败: {}", e)))?;

    let result = if rtts.is_empty() {
        PingResult {
            rtts: vec![],
            min: None,
            avg: None,
            max: None,
            loss,
        }
    } else {
        let min = rtts.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = rtts.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let avg = rtts.iter().sum::<f64>() / rtts.len() as f64;
        PingResult {
            rtts,
            min: Some(min),
            avg: Some(avg),
            max: Some(max),
            loss,
        }
    };

    serde_json::to_value(result)
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("序列化失败: {}", e)))
}

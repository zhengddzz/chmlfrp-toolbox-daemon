use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TunnelLatencyParams {
    host: String,
    port: u16,
    #[serde(default = "default_count")]
    count: usize,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    run_id: String,
}

fn default_count() -> usize {
    4
}
fn default_timeout_ms() -> u64 {
    3000
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelLatencyResult {
    success: bool,
    avg_ms: f64,
    jitter_ms: f64,
    loss_percent: f64,
    sent: usize,
    received: usize,
    rtts: Vec<f64>,
    error: Option<String>,
}

fn calculate_latency_stats(samples: &[Option<f64>]) -> Result<TunnelLatencyResult, String> {
    let rtts: Vec<f64> = samples.iter().filter_map(|sample| *sample).collect();
    if rtts.is_empty() {
        return Err("全链路探测全部超时".to_string());
    }
    let avg_ms = rtts.iter().sum::<f64>() / rtts.len() as f64;
    let jitter_ms = if rtts.len() > 1 {
        rtts.windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .sum::<f64>()
            / (rtts.len() - 1) as f64
    } else {
        0.0
    };
    Ok(TunnelLatencyResult {
        success: true,
        avg_ms,
        jitter_ms,
        loss_percent: (samples.len() - rtts.len()) as f64 / samples.len() as f64 * 100.0,
        sent: samples.len(),
        received: rtts.len(),
        rtts,
        error: None,
    })
}

pub async fn handle(
    params: &serde_json::Value,
    ctx: &super::CommandContext,
) -> super::CommandResult {
    let p: TunnelLatencyParams = serde_json::from_value(params.clone())
        .map_err(|e| super::RpcError::new("INVALID_PARAMS", format!("参数解析失败: {}", e)))?;
    if p.host.is_empty() || p.port == 0 {
        return Err(super::RpcError::new(
            "INVALID_PARAMS",
            "host 和 port 必须有效",
        ));
    }
    let account_id = ctx.account_id.clone();
    let generation = super::run_generation(&account_id, &p.run_id);
    let result = tokio::task::spawn_blocking(move || run(&p, &account_id, generation))
        .await
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("任务执行失败: {}", e)))?
        .map_err(|e| super::RpcError::new("EXEC_FAILED", e))?;
    serde_json::to_value(result)
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("序列化失败: {}", e)))
}

fn run(
    p: &TunnelLatencyParams,
    account_id: &str,
    generation: u64,
) -> Result<TunnelLatencyResult, String> {
    let timeout = Duration::from_millis(p.timeout_ms.clamp(100, 30000));
    let addr = format!("{}:{}", p.host, p.port)
        .to_socket_addrs()
        .map_err(|e| format!("解析隧道地址失败: {}", e))?
        .next()
        .ok_or_else(|| "无法解析隧道地址".to_string())?;
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("连接隧道失败: {}", e))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    stream.set_nodelay(true).ok();
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let count = p.count.clamp(1, 20);
    let mut samples = Vec::with_capacity(count);
    for sequence in 0..count {
        if super::is_run_cancelled(account_id, &p.run_id, generation) {
            return Err("测速已强制停止".to_string());
        }
        let started = Instant::now();
        if stream
            .write_all(format!("PING {}\n", sequence).as_bytes())
            .is_err()
            || stream.flush().is_err()
        {
            samples.push(None);
            continue;
        }
        let mut response = String::new();
        loop {
            if super::is_run_cancelled(account_id, &p.run_id, generation) {
                return Err("测速已强制停止".to_string());
            }
            match reader.read_line(&mut response) {
                Ok(n) if n > 0 && response.trim() == format!("PONG {}", sequence) => {
                    samples.push(Some(started.elapsed().as_secs_f64() * 1000.0));
                    break;
                }
                Err(ref e)
                    if (e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut)
                        && started.elapsed() < timeout =>
                {
                    continue
                }
                _ => {
                    samples.push(None);
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    calculate_latency_stats(&samples)
}

#[cfg(test)]
mod tests {
    use super::calculate_latency_stats;

    #[test]
    fn calculates_full_tunnel_latency_stats() {
        let result = calculate_latency_stats(&[Some(10.0), Some(14.0), None, Some(12.0)]).unwrap();
        assert_eq!(result.avg_ms, 12.0);
        assert_eq!(result.jitter_ms, 3.0);
        assert_eq!(result.loss_percent, 25.0);
        assert_eq!(result.received, 3);
    }

    #[test]
    fn rejects_all_lost_probes() {
        assert!(calculate_latency_stats(&[None, None]).is_err());
    }
}

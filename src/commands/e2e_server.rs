//! e2e_setup / e2e_cleanup 命令 - 端对端测试服务端
//!
//! 当其他设备（桌面客户端或 daemon）发起「本机 → 对端」方向端对端测试时，
//! 通过 relay RPC 调用对端的 e2e_setup，让对端：
//!   1. 启动 TCP 测速服务端（监听随机端口）
//!   2. 调用 chmlfrp API 创建临时隧道
//!   3. 运行 frpc 连接节点
//!   4. 返回隧道地址（nodeIp:remotePort）供发起方测速
//!
//! 测试完成后发起方调用 e2e_cleanup 清理资源。

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const TEST_DATA_SIZE: usize = 1024 * 1024; // 1MB 数据块

fn parse_speed_request(request: &str) -> Result<Duration, String> {
    let mut parts = request.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some("SPEEDTEST_TIME"), Some(value), None) => {
            let duration_ms = value
                .parse::<u64>()
                .map_err(|_| "无效的测速时长".to_string())?;
            if !(5_000..=120_000).contains(&duration_ms) {
                return Err("测速时长必须在 5000 到 120000 毫秒之间".to_string());
            }
            Ok(Duration::from_millis(duration_ms))
        }
        _ => Err("不支持的测速命令".to_string()),
    }
}

/// 全局测速服务端状态
static E2E_SERVER_RUNNING: AtomicBool = AtomicBool::new(false);
static E2E_SERVER_PORT: AtomicU16 = AtomicU16::new(0);

struct FrpcProcess {
    child: Child,
    config_path: std::path::PathBuf,
    logs: Arc<Mutex<VecDeque<String>>>,
}

static FRPC_PROCESS: Mutex<Option<FrpcProcess>> = Mutex::new(None);

/// 全局隧道信息（用于 cleanup 时删除）
static E2E_TUNNEL_ID: Mutex<Option<i64>> = Mutex::new(None);

#[derive(Default)]
struct ResourceCoordinator {
    active: Option<ResourceOwner>,
}

struct ResourceOwner {
    account_id: String,
    run_id: String,
    generation: u64,
    ready: bool,
    cleaning: bool,
}

impl ResourceCoordinator {
    fn claim(&mut self, account_id: &str, run_id: &str) -> Result<u64, String> {
        if run_id.is_empty() {
            return Err("runId 不能为空".to_string());
        }
        if self.active.is_some() {
            return Err("已有其他全链路测速正在占用发送端资源".to_string());
        }
        let generation = super::run_generation(account_id, run_id);
        self.active = Some(ResourceOwner {
            account_id: account_id.to_string(),
            run_id: run_id.to_string(),
            generation,
            ready: false,
            cleaning: false,
        });
        Ok(generation)
    }

    fn is_owner(&self, account_id: &str, run_id: &str, generation: u64) -> bool {
        self.active.as_ref().is_some_and(|owner| {
            owner.account_id == account_id
                && owner.run_id == run_id
                && owner.generation == generation
        })
    }

    fn begin_cleanup(&mut self, account_id: &str, run_id: &str, generation: u64) -> bool {
        let Some(owner) = self.active.as_mut() else {
            return false;
        };
        if owner.account_id != account_id
            || owner.run_id != run_id
            || owner.generation != generation
            || !owner.ready
            || owner.cleaning
        {
            return false;
        }
        owner.cleaning = true;
        true
    }

    fn mark_ready(&mut self, account_id: &str, run_id: &str, generation: u64) -> bool {
        let Some(owner) = self.active.as_mut() else {
            return false;
        };
        if owner.account_id != account_id
            || owner.run_id != run_id
            || owner.generation != generation
            || owner.cleaning
        {
            return false;
        }
        owner.ready = true;
        true
    }

    fn abort_cleanup(&mut self, account_id: &str, run_id: &str, generation: u64) -> bool {
        let Some(owner) = self.active.as_mut() else {
            return false;
        };
        if owner.account_id != account_id
            || owner.run_id != run_id
            || owner.generation != generation
            || !owner.cleaning
        {
            return false;
        }
        owner.cleaning = false;
        true
    }

    fn finish_cleanup(&mut self, account_id: &str, run_id: &str, generation: u64) -> bool {
        if !self.is_owner(account_id, run_id, generation) {
            return false;
        }
        self.active = None;
        true
    }

    fn active_generation(&self, account_id: &str, run_id: &str) -> Option<u64> {
        self.active
            .as_ref()
            .filter(|owner| owner.account_id == account_id && owner.run_id == run_id)
            .map(|owner| owner.generation)
    }
}

static RESOURCE_COORDINATOR: once_cell::sync::Lazy<Mutex<ResourceCoordinator>> =
    once_cell::sync::Lazy::new(|| Mutex::new(ResourceCoordinator::default()));

fn claim_run(account_id: &str, run_id: &str) -> Result<u64, String> {
    RESOURCE_COORDINATOR
        .lock()
        .map_err(|e| format!("获取测速资源锁失败: {}", e))?
        .claim(account_id, run_id)
}

fn complete_run(account_id: &str, run_id: &str, generation: u64) {
    if let Ok(mut coordinator) = RESOURCE_COORDINATOR.lock() {
        if coordinator.begin_cleanup(account_id, run_id, generation)
            || coordinator.is_owner(account_id, run_id, generation)
        {
            coordinator.finish_cleanup(account_id, run_id, generation);
        }
    }
    super::finish_run(account_id, run_id, generation);
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct E2eSetupParams {
    /// 节点名称（如 node1）
    node_name: String,
    #[serde(default)]
    run_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct E2eSetupResult {
    success: bool,
    node_ip: String,
    remote_port: u16,
    tunnel_id: i64,
    error: Option<String>,
    protocol_version: u8,
}

/// 启动 TCP 测速服务端（与桌面客户端协议一致）
fn start_e2e_tcp_server() -> Result<u16, String> {
    if E2E_SERVER_RUNNING.load(Ordering::SeqCst) {
        return Ok(E2E_SERVER_PORT.load(Ordering::SeqCst));
    }

    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("绑定端口失败: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("获取端口失败: {}", e))?
        .port();

    E2E_SERVER_PORT.store(port, Ordering::SeqCst);
    E2E_SERVER_RUNNING.store(true, Ordering::SeqCst);

    std::thread::spawn(move || {
        let test_data = vec![0u8; TEST_DATA_SIZE];
        listener.set_nonblocking(true).ok();

        while E2E_SERVER_RUNNING.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    // 为每个连接启动处理线程
                    let data = test_data.clone();
                    std::thread::spawn(move || {
                        // 设置超时
                        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(60))).ok();
                        let mut writer = match stream.try_clone() {
                            Ok(value) => value,
                            Err(_) => return,
                        };
                        let mut reader = BufReader::new(stream);
                        loop {
                            let mut request = String::new();
                            match reader.read_line(&mut request) {
                                Ok(0) => break,
                                Ok(_) => {}
                                Err(_) => break,
                            }
                            if request.starts_with("PING ") {
                                let sequence = request.split_whitespace().nth(1).unwrap_or("0");
                                if writer
                                    .write_all(format!("PONG {}\n", sequence).as_bytes())
                                    .is_err()
                                {
                                    break;
                                }
                                let _ = writer.flush();
                            } else if request.starts_with("SPEEDTEST_TIME ") {
                                match parse_speed_request(&request) {
                                    Ok(duration) => {
                                        let _ = writer
                                            .set_write_timeout(Some(Duration::from_millis(100)));
                                        let deadline = Instant::now() + duration;
                                        while Instant::now() < deadline
                                            && E2E_SERVER_RUNNING.load(Ordering::SeqCst)
                                        {
                                            match writer.write(&data) {
                                                Ok(0) => break,
                                                Ok(_) => {}
                                                Err(error)
                                                    if error.kind()
                                                        == std::io::ErrorKind::WouldBlock
                                                        || error.kind()
                                                            == std::io::ErrorKind::TimedOut => {}
                                                Err(_) => break,
                                            }
                                        }
                                        let _ = writer.shutdown(Shutdown::Both);
                                    }
                                    Err(error) => warn!("无效测速请求: {}", error),
                                }
                                break;
                            }
                        }
                        // 连接关闭由 drop 自动处理
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    });

    Ok(port)
}

/// 停止 TCP 测速服务端
fn stop_e2e_tcp_server() {
    E2E_SERVER_RUNNING.store(false, Ordering::SeqCst);
    E2E_SERVER_PORT.store(0, Ordering::SeqCst);
}

#[cfg(test)]
mod speed_protocol_tests {
    use super::{
        build_frpc_config, is_remote_port_conflict, parse_node_address, parse_speed_request,
        remote_port_candidates, validate_probe_response,
    };
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn parses_time_speed_request() {
        assert_eq!(
            parse_speed_request("SPEEDTEST_TIME 15000\n").unwrap(),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn rejects_time_request_outside_limits() {
        assert!(parse_speed_request("SPEEDTEST_TIME 4000\n").is_err());
        assert!(parse_speed_request("SPEEDTEST_TIME 121000\n").is_err());
    }

    #[test]
    fn falls_back_when_primary_node_address_is_null_or_blank() {
        assert_eq!(
            parse_node_address(&json!({ "ip": null, "realIp": "1.2.3.4" })).unwrap(),
            "1.2.3.4"
        );
        assert_eq!(
            parse_node_address(&json!({ "ip": "  ", "real_IP": "node.example.com" })).unwrap(),
            "node.example.com"
        );
    }

    #[test]
    fn reports_missing_node_address_clearly() {
        assert_eq!(
            parse_node_address(&json!({ "ip": null, "realIp": "" })).unwrap_err(),
            "节点信息中缺少可用地址"
        );
    }

    #[test]
    fn recognizes_only_remote_port_conflicts_as_retryable() {
        assert!(is_remote_port_conflict("创建隧道失败: 该远程端口已被占用"));
        assert!(is_remote_port_conflict("remote port already in use"));
        assert!(!is_remote_port_conflict("创建隧道失败: 无效的登录状态"));
        assert!(!is_remote_port_conflict("请求过于频繁"));
    }

    #[test]
    fn creates_unique_remote_port_candidates_within_range() {
        let ports = remote_port_candidates(20000, 20003, 5);
        assert_eq!(ports.len(), 4);
        assert!(ports.iter().all(|port| (20000..=20003).contains(port)));
        let unique = ports
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), ports.len());
    }

    #[test]
    fn frpc_config_contains_user_credential() {
        let config = build_frpc_config(
            "node.example.com",
            7000,
            "user-token",
            "node-token",
            31000,
            32000,
            "e2etest_1",
        )
        .unwrap();

        assert!(config.contains("user = user-token"));
        assert!(config.contains("token = node-token"));
    }

    #[test]
    fn frpc_config_rejects_newline_in_credentials() {
        let result = build_frpc_config(
            "node.example.com",
            7000,
            "user-token\nadmin_addr = 0.0.0.0",
            "node-token",
            31000,
            32000,
            "e2etest_1",
        );

        assert!(result.is_err());
    }

    #[test]
    fn probe_response_requires_matching_sequence() {
        assert!(validate_probe_response("PONG ready-123\n", "ready-123"));
        assert!(!validate_probe_response("PONG other\n", "ready-123"));
        assert!(!validate_probe_response("HTTP/1.1 200 OK\n", "ready-123"));
    }
}

fn parse_node_address(node_data: &serde_json::Value) -> Result<String, String> {
    ["ip", "realIp", "real_IP"]
        .iter()
        .filter_map(|key| node_data.get(key).and_then(|value| value.as_str()))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "节点信息中缺少可用地址".to_string())
}

fn is_remote_port_conflict(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    message.contains("远程端口") && (message.contains("占用") || message.contains("已被使用"))
        || normalized.contains("remote port")
            && (normalized.contains("already in use") || normalized.contains("occupied"))
}

fn remote_port_candidates(min_port: u16, max_port: u16, max_attempts: usize) -> Vec<u16> {
    let start = min_port.min(max_port);
    let end = min_port.max(max_port);
    let count = u32::from(end) - u32::from(start) + 1;
    let limit = max_attempts.min(count as usize);
    let offset = (uuid::Uuid::new_v4().as_u128() % u128::from(count)) as u32;
    (0..limit)
        .map(|index| u32::from(start) + (offset + index as u32) % count)
        .map(|port| port as u16)
        .collect()
}

/// 调用 chmlfrp API 创建临时隧道
///
/// chmlfrp API 只认 qzhua accessToken，先用 proxy_token 调后端
/// /auth/refresh 换取（带缓存，详见 auth.rs）
async fn create_temp_tunnel(
    backend_url: &str,
    proxy_token: &str,
    node_name: &str,
    local_port: u16,
) -> Result<(i64, String, u16, String, u16, String), String> {
    // 0. 获取 accessToken（chmlfrp API 不认 proxy_token，直接用会报"无效的登录状态"）
    let access_token = super::auth::ensure_access_token(backend_url, proxy_token)
        .await
        .map_err(|e| e.user_message())?;

    // 1. 获取节点信息
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;
    let node_info_url = format!("https://cf-v2.uapis.cn/nodeinfo?node={}", node_name);

    let node_resp = client
        .get(&node_info_url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| format!("获取节点信息失败: {}", e))?;

    let node_status = node_resp.status();

    let node_data: serde_json::Value = node_resp
        .json()
        .await
        .map_err(|e| format!("解析节点信息失败: {}", e))?;

    if !node_status.is_success() {
        let msg = node_data
            .get("msg")
            .and_then(|value| value.as_str())
            .unwrap_or("未知错误");
        return Err(format!("获取节点信息失败（HTTP {}）: {}", node_status, msg));
    }

    if let Some(code) = node_data.get("code").and_then(|value| value.as_i64()) {
        if code != 200 {
            let msg = node_data
                .get("msg")
                .and_then(|value| value.as_str())
                .unwrap_or("未知错误");
            return Err(format!("获取节点信息失败（业务码 {}）: {}", code, msg));
        }
    }

    let node_data = node_data
        .get("data")
        .filter(|value| value.is_object())
        .unwrap_or(&node_data);

    let node_ip = parse_node_address(node_data)?;

    let rport_str = node_data
        .get("rport")
        .and_then(|v| v.as_str())
        .unwrap_or("20000-40000");

    let node_token = node_data
        .get("nodetoken")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let server_port = node_data
        .get("port")
        .and_then(|v| v.as_u64())
        .unwrap_or(7000) as u16;

    // 2. 选择远程端口；仅对明确的端口占用错误更换端口重试
    let (min_port, max_port) = parse_port_range(rport_str);
    let create_url = "https://cf-v2.uapis.cn/create_tunnel";
    let candidates = remote_port_candidates(min_port, max_port, 5);
    let mut selected = None;
    for (attempt, remote_port) in candidates.iter().copied().enumerate() {
        let tunnel_name = format!(
            "e2etest_{}_{}_{}",
            chrono_timestamp(),
            remote_port,
            attempt + 1
        );
        let create_body = serde_json::json!({
            "tunnelname": tunnel_name,
            "node": node_name,
            "localip": "127.0.0.1",
            "porttype": "tcp",
            "localport": local_port,
            "remoteport": remote_port,
            "encryption": false,
            "compression": false,
            "extraparams": "",
        });
        let create_resp = client
            .post(create_url)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .json(&create_body)
            .send()
            .await
            .map_err(|e| format!("创建隧道失败: {}", e))?;
        let create_status = create_resp.status();
        let create_data: serde_json::Value = create_resp
            .json()
            .await
            .map_err(|e| format!("解析创建隧道响应失败: {}", e))?;
        if create_status.is_success()
            && create_data.get("code").and_then(|v| v.as_u64()) == Some(200)
        {
            selected = Some((remote_port, tunnel_name));
            break;
        }

        let msg = create_data
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误");
        let error = format!("创建隧道失败: {}", msg);
        if !is_remote_port_conflict(&error) {
            return Err(if create_status.is_success() {
                error
            } else {
                format!("{}（HTTP {}）", error, create_status)
            });
        }
        warn!(
            "[e2e] 远程端口 {} 已占用，第 {}/{} 次更换端口",
            remote_port,
            attempt + 1,
            candidates.len()
        );
    }
    let (remote_port, tunnel_name) = selected.ok_or_else(|| {
        format!(
            "创建隧道失败: 连续 {} 个远程端口均已被占用",
            candidates.len()
        )
    })?;

    // 4. 查询隧道列表获取隧道 ID
    let tunnels_url = "https://cf-v2.uapis.cn/tunnel";
    let tunnels_resp = client
        .get(tunnels_url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| format!("获取隧道列表失败: {}", e))?;

    let tunnels_data: serde_json::Value = tunnels_resp
        .json()
        .await
        .map_err(|e| format!("解析隧道列表失败: {}", e))?;

    let tunnels = tunnels_data
        .get("data")
        .or_else(|| Some(&tunnels_data))
        .and_then(|v| v.as_array())
        .ok_or("隧道列表格式错误")?;

    let tunnel = tunnels
        .iter()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(&tunnel_name))
        .ok_or("未找到新创建的隧道")?;

    let tunnel_id = tunnel
        .get("id")
        .and_then(|v| v.as_i64())
        .ok_or("隧道ID缺失")?;

    // 解析实际远程端口
    let actual_remote_port = tunnel
        .get("dorp")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u16>().ok())
        .or_else(|| {
            tunnel
                .get("remote_port")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16)
        })
        .unwrap_or(remote_port);

    Ok((
        tunnel_id,
        node_ip,
        actual_remote_port,
        node_token,
        server_port,
        tunnel_name,
    ))
}

/// 删除隧道（chmlfrp API 只认 accessToken，先用 proxy_token 换取）
async fn delete_tunnel(backend_url: &str, proxy_token: &str, tunnel_id: i64) -> Result<(), String> {
    let access_token = super::auth::ensure_access_token(backend_url, proxy_token)
        .await
        .map_err(|e| e.user_message())?;

    let url = format!(
        "https://cf-v2.uapis.cn/delete_tunnel?tunnelid={}",
        tunnel_id
    );
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| format!("删除隧道请求失败: {}", e))?;

    let data: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析删除隧道响应失败: {}", e))?;

    if data.get("code").and_then(|v| v.as_u64()) != Some(200) {
        let msg = data
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误");
        warn!("[e2e] 删除隧道失败: {}", msg);
        return Err(format!("删除隧道失败: {}", msg));
    }

    Ok(())
}

/// 启动 frpc 子进程
fn validate_ini_value(value: &str, field_name: &str) -> Result<(), String> {
    if value.contains(['\n', '\r', '[', ']', '=', '\0']) {
        return Err(format!("字段 {} 包含非法字符", field_name));
    }
    Ok(())
}

fn build_frpc_config(
    node_ip: &str,
    server_port: u16,
    user_token: &str,
    node_token: &str,
    local_port: u16,
    remote_port: u16,
    tunnel_name: &str,
) -> Result<String, String> {
    validate_ini_value(node_ip, "server_addr")?;
    validate_ini_value(user_token, "user")?;
    validate_ini_value(node_token, "token")?;
    validate_ini_value(tunnel_name, "tunnel_name")?;
    Ok(format!(
        "[common]\nserver_addr = {}\nserver_port = {}\nuser = {}\ntoken = {}\nlog_level = info\ntls_enable = false\ntcp_mux = true\npool_count = 5\n\n[{}]\ntype = tcp\nlocal_ip = 127.0.0.1\nlocal_port = {}\nremote_port = {}\n",
        node_ip,
        server_port,
        user_token,
        node_token,
        tunnel_name,
        local_port,
        remote_port
    ))
}

fn capture_frpc_output<R: std::io::Read + Send + 'static>(
    reader: R,
    logs: Arc<Mutex<VecDeque<String>>>,
    secrets: Vec<String>,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            let line = secrets
                .iter()
                .filter(|secret| !secret.is_empty())
                .fold(line, |value, secret| value.replace(secret, "[已脱敏]"));
            let line = line.chars().take(500).collect::<String>();
            if let Ok(mut guard) = logs.lock() {
                if guard.len() >= 30 {
                    guard.pop_front();
                }
                guard.push_back(line);
            }
        }
    });
}

fn start_frpc(
    frpc_path: &std::path::Path,
    data_dir: &str,
    node_ip: &str,
    server_port: u16,
    user_token: &str,
    node_token: &str,
    local_port: u16,
    remote_port: u16,
    tunnel_name: &str,
) -> Result<u32, String> {
    let config_dir = std::path::Path::new(data_dir).join("e2e");
    std::fs::create_dir_all(&config_dir).map_err(|e| format!("创建frpc配置目录失败: {}", e))?;
    let config_path = config_dir.join(format!("e2e_frpc_{}.ini", std::process::id()));
    let config_content = build_frpc_config(
        node_ip,
        server_port,
        user_token,
        node_token,
        local_port,
        remote_port,
        tunnel_name,
    )?;

    std::fs::write(&config_path, &config_content)
        .map_err(|e| format!("写入frpc配置失败: {}", e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("设置frpc配置权限失败: {}", e))?;
    }

    let mut child = std::process::Command::new(frpc_path)
        .arg("-c")
        .arg(&config_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动frpc失败: {}（路径: {}）", e, frpc_path.display()))?;

    let pid = child.id();
    let logs = Arc::new(Mutex::new(VecDeque::new()));
    let secrets = vec![user_token.to_string(), node_token.to_string()];
    if let Some(stdout) = child.stdout.take() {
        capture_frpc_output(stdout, Arc::clone(&logs), secrets.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        capture_frpc_output(stderr, Arc::clone(&logs), secrets);
    }
    let mut guard = FRPC_PROCESS
        .lock()
        .map_err(|e| format!("保存frpc进程失败: {}", e))?;
    *guard = Some(FrpcProcess {
        child,
        config_path,
        logs,
    });

    info!("[e2e] frpc 已启动，PID: {}", pid);
    Ok(pid)
}

fn frpc_exit_error() -> Option<String> {
    let mut guard = FRPC_PROCESS.lock().ok()?;
    let process = guard.as_mut()?;
    let status = match process.child.try_wait() {
        Ok(Some(status)) => status,
        Ok(None) => return None,
        Err(error) => return Some(format!("检查frpc状态失败: {}", error)),
    };
    let logs = process
        .logs
        .lock()
        .ok()
        .map(|logs| logs.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_default();
    Some(if logs.is_empty() {
        format!("frpc 已退出: {}", status)
    } else {
        format!("frpc 已退出: {}，日志: {}", status, logs)
    })
}

fn stop_frpc() {
    if let Ok(mut guard) = FRPC_PROCESS.lock() {
        if let Some(mut process) = guard.take() {
            let pid = process.child.id();
            if process.child.try_wait().ok().flatten().is_none() {
                let _ = process.child.kill();
                let _ = process.child.wait();
            }
            let _ = std::fs::remove_file(process.config_path);
            info!("[e2e] frpc 已停止，PID: {}", pid);
        }
    }
}

fn validate_probe_response(response: &str, sequence: &str) -> bool {
    response.trim() == format!("PONG {}", sequence)
}

async fn probe_tunnel(node_ip: &str, remote_port: u16) -> Result<bool, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let address = format!("{}:{}", node_ip, remote_port);
    let stream = match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(&address),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) | Err(_) => return Ok(false),
    };
    let sequence = format!("ready-{}", uuid::Uuid::new_v4());
    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(format!("PING {}\n", sequence).as_bytes())
        .await
        .map_err(|e| format!("发送隧道探测失败: {}", e))?;
    let mut response = String::new();
    match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::io::BufReader::new(reader).read_line(&mut response),
    )
    .await
    {
        Ok(Ok(bytes)) if bytes > 0 => Ok(validate_probe_response(&response, &sequence)),
        Ok(Ok(_)) | Err(_) => Ok(false),
        Ok(Err(error)) => Err(format!("读取隧道探测失败: {}", error)),
    }
}

/// 解析端口范围
fn parse_port_range(rport: &str) -> (u16, u16) {
    if rport.contains('-') {
        let parts: Vec<&str> = rport.split('-').collect();
        if parts.len() == 2 {
            let start: u16 = parts[0].trim().parse().unwrap_or(20000);
            let end: u16 = parts[1].trim().parse().unwrap_or(40000);
            return (start.min(end), start.max(end));
        }
    }
    (20000, 40000)
}

/// 简单时间戳
fn chrono_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ===== RPC 命令处理 =====

pub async fn handle_setup(
    params: &serde_json::Value,
    ctx: &super::CommandContext,
) -> super::CommandResult {
    let p: E2eSetupParams = serde_json::from_value(params.clone())
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("参数解析失败: {}", e)))?;

    info!("[e2e_setup] 节点: {}", p.node_name);

    let generation = claim_run(&ctx.account_id, &p.run_id)
        .map_err(|error| super::RpcError::new("RESOURCE_BUSY", error))?;

    if super::is_run_cancelled(&ctx.account_id, &p.run_id, generation) {
        complete_run(&ctx.account_id, &p.run_id, generation);
        return Err(super::RpcError::new("CANCELLED", "测速已强制停止"));
    }

    // 1. 获取 proxyToken
    if ctx.proxy_token.is_empty() {
        complete_run(&ctx.account_id, &p.run_id, generation);
        return Err(super::RpcError::new(
            "EXEC_FAILED",
            "当前连接缺少 proxyToken",
        ));
    }
    let proxy_token = ctx.proxy_token.clone();
    let backend_url = ctx.backend_url.clone();

    let user_token = match super::auth::ensure_user_token(&backend_url, &proxy_token).await {
        Ok(token) => token,
        Err(error) => {
            complete_run(&ctx.account_id, &p.run_id, generation);
            return Err(super::RpcError::new(
                "FRPC_AUTH_FAILED",
                error.user_message(),
            ));
        }
    };

    let frpc_path = match crate::frpc::ensure_frpc(&ctx.data_dir).await {
        Ok(path) => path,
        Err(error) => {
            complete_run(&ctx.account_id, &p.run_id, generation);
            return Err(super::RpcError::new("FRPC_PREPARE_FAILED", error));
        }
    };
    info!("[e2e_setup] frpc 已就绪: {}", frpc_path.display());

    // 2. 启动 TCP 测速服务端
    let tcp_port = match start_e2e_tcp_server() {
        Ok(port) => port,
        Err(error) => {
            complete_run(&ctx.account_id, &p.run_id, generation);
            return Err(super::RpcError::new("EXEC_FAILED", error));
        }
    };

    info!("[e2e_setup] TCP 服务端端口: {}", tcp_port);

    // 3. 创建临时隧道
    let (tunnel_id, node_ip, remote_port, node_token, server_port, tunnel_name) =
        match create_temp_tunnel(&backend_url, &proxy_token, &p.node_name, tcp_port).await {
            Ok(result) => result,
            Err(error) => {
                stop_e2e_tcp_server();
                complete_run(&ctx.account_id, &p.run_id, generation);
                return Err(super::RpcError::new("EXEC_FAILED", error));
            }
        };

    if super::is_run_cancelled(&ctx.account_id, &p.run_id, generation) {
        let _ = delete_tunnel(&backend_url, &proxy_token, tunnel_id).await;
        stop_e2e_tcp_server();
        complete_run(&ctx.account_id, &p.run_id, generation);
        return Err(super::RpcError::new("CANCELLED", "测速已强制停止"));
    }

    // 保存隧道 ID 用于 cleanup
    if let Ok(mut guard) = E2E_TUNNEL_ID.lock() {
        *guard = Some(tunnel_id);
    }

    info!(
        "[e2e_setup] 隧道创建成功: {}:{} (tunnel_id={})",
        node_ip, remote_port, tunnel_id
    );

    // 4. 启动 frpc
    if let Err(e) = start_frpc(
        &frpc_path,
        &ctx.data_dir,
        &node_ip,
        server_port,
        &user_token,
        &node_token,
        tcp_port,
        remote_port,
        &tunnel_name,
    ) {
        // frpc 启动失败，清理隧道
        let _ = delete_tunnel(&backend_url, &proxy_token, tunnel_id).await;
        stop_e2e_tcp_server();
        complete_run(&ctx.account_id, &p.run_id, generation);
        return Err(super::RpcError::new("EXEC_FAILED", e));
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if super::is_run_cancelled(&ctx.account_id, &p.run_id, generation) {
            stop_frpc();
            let _ = delete_tunnel(&backend_url, &proxy_token, tunnel_id).await;
            stop_e2e_tcp_server();
            complete_run(&ctx.account_id, &p.run_id, generation);
            return Err(super::RpcError::new("CANCELLED", "测速已强制停止"));
        }
        if let Some(error) = frpc_exit_error() {
            stop_frpc();
            let _ = delete_tunnel(&backend_url, &proxy_token, tunnel_id).await;
            stop_e2e_tcp_server();
            complete_run(&ctx.account_id, &p.run_id, generation);
            return Err(super::RpcError::new("FRPC_START_FAILED", error));
        }
        match probe_tunnel(&node_ip, remote_port).await {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => warn!("[e2e_setup] 隧道探测失败: {}", error),
        }
        if tokio::time::Instant::now() >= deadline {
            stop_frpc();
            let _ = delete_tunnel(&backend_url, &proxy_token, tunnel_id).await;
            stop_e2e_tcp_server();
            complete_run(&ctx.account_id, &p.run_id, generation);
            return Err(super::RpcError::new(
                "TUNNEL_READY_TIMEOUT",
                format!("隧道在 30 秒内未就绪: {}:{}", node_ip, remote_port),
            ));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let ready = RESOURCE_COORDINATOR
        .lock()
        .map(|mut coordinator| coordinator.mark_ready(&ctx.account_id, &p.run_id, generation))
        .unwrap_or(false);
    if !ready {
        stop_frpc();
        let _ = delete_tunnel(&backend_url, &proxy_token, tunnel_id).await;
        stop_e2e_tcp_server();
        complete_run(&ctx.account_id, &p.run_id, generation);
        return Err(super::RpcError::new("CANCELLED", "测速资源已进入清理流程"));
    }

    let result = E2eSetupResult {
        success: true,
        node_ip,
        remote_port,
        tunnel_id,
        error: None,
        protocol_version: 2,
    };

    serde_json::to_value(result)
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("序列化失败: {}", e)))
}

pub async fn handle_cleanup(
    params: &serde_json::Value,
    ctx: &super::CommandContext,
) -> super::CommandResult {
    info!("[e2e_cleanup] 开始清理资源");

    let run_id = params
        .get("runId")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let generation = RESOURCE_COORDINATOR
        .lock()
        .ok()
        .and_then(|coordinator| coordinator.active_generation(&ctx.account_id, run_id));
    let Some(generation) = generation else {
        return Ok(serde_json::json!({ "cleaned": false, "reason": "RUN_NOT_ACTIVE" }));
    };
    let cleaning_started = RESOURCE_COORDINATOR
        .lock()
        .map(|mut coordinator| coordinator.begin_cleanup(&ctx.account_id, run_id, generation))
        .unwrap_or(false);
    if !cleaning_started {
        return Ok(serde_json::json!({ "cleaned": false, "reason": "CLEANUP_IN_PROGRESS" }));
    }

    // 1. 停止 frpc
    stop_frpc();

    // 2. 删除隧道
    let tunnel_id = if let Ok(guard) = E2E_TUNNEL_ID.lock() {
        guard.as_ref().copied()
    } else {
        None
    };

    if let Some(tid) = tunnel_id {
        if let Err(error) = delete_tunnel(&ctx.backend_url, &ctx.proxy_token, tid).await {
            if let Ok(mut coordinator) = RESOURCE_COORDINATOR.lock() {
                coordinator.abort_cleanup(&ctx.account_id, run_id, generation);
            }
            return Err(super::RpcError::new("CLEANUP_FAILED", error));
        }
        if let Ok(mut guard) = E2E_TUNNEL_ID.lock() {
            *guard = None;
        }
    }

    // 3. 停止 TCP 测速服务端
    stop_e2e_tcp_server();

    info!("[e2e_cleanup] 清理完成");

    complete_run(&ctx.account_id, run_id, generation);

    Ok(serde_json::json!({ "cleaned": true }))
}

#[cfg(test)]
mod tests {
    use super::ResourceCoordinator;

    #[test]
    fn cleanup_is_single_owner_operation() {
        let mut coordinator = ResourceCoordinator::default();
        let generation = coordinator.claim("account-1", "run-1").unwrap();
        assert!(coordinator.mark_ready("account-1", "run-1", generation));
        assert!(coordinator.begin_cleanup("account-1", "run-1", generation));
        assert!(!coordinator.begin_cleanup("account-1", "run-1", generation));
    }

    #[test]
    fn late_cleanup_cannot_release_another_run() {
        let mut coordinator = ResourceCoordinator::default();
        let first = coordinator.claim("account-1", "run-1").unwrap();
        assert!(coordinator.mark_ready("account-1", "run-1", first));
        assert!(coordinator.begin_cleanup("account-1", "run-1", first));
        assert!(coordinator.finish_cleanup("account-1", "run-1", first));
        let second = coordinator.claim("account-1", "run-2").unwrap();
        assert!(!coordinator.finish_cleanup("account-1", "run-1", first));
        assert!(coordinator.is_owner("account-1", "run-2", second));
    }

    #[test]
    fn completed_run_id_can_be_reused_sequentially() {
        let mut coordinator = ResourceCoordinator::default();
        let generation = coordinator.claim("account-1", "run-1").unwrap();
        assert!(coordinator.mark_ready("account-1", "run-1", generation));
        assert!(coordinator.begin_cleanup("account-1", "run-1", generation));
        assert!(coordinator.finish_cleanup("account-1", "run-1", generation));
        assert!(coordinator.claim("account-1", "run-1").is_ok());
    }

    #[test]
    fn duplicate_setup_is_rejected() {
        let mut coordinator = ResourceCoordinator::default();
        coordinator.claim("account-1", "run-1").unwrap();
        assert!(coordinator.claim("account-1", "run-1").is_err());
    }

    #[test]
    fn external_cleanup_waits_until_setup_is_ready() {
        let mut coordinator = ResourceCoordinator::default();
        let generation = coordinator.claim("account-1", "run-1").unwrap();
        assert!(!coordinator.begin_cleanup("account-1", "run-1", generation));
        assert!(coordinator.mark_ready("account-1", "run-1", generation));
        assert!(coordinator.begin_cleanup("account-1", "run-1", generation));
    }

    #[test]
    fn failed_cleanup_can_be_retried() {
        let mut coordinator = ResourceCoordinator::default();
        let generation = coordinator.claim("account-1", "run-1").unwrap();
        assert!(coordinator.mark_ready("account-1", "run-1", generation));
        assert!(coordinator.begin_cleanup("account-1", "run-1", generation));
        assert!(coordinator.abort_cleanup("account-1", "run-1", generation));
        assert!(coordinator.begin_cleanup("account-1", "run-1", generation));
    }

    #[test]
    fn another_account_cannot_cleanup_active_resources() {
        let mut coordinator = ResourceCoordinator::default();
        let generation = coordinator.claim("account-1", "run-1").unwrap();
        assert!(coordinator.mark_ready("account-1", "run-1", generation));
        assert!(!coordinator.begin_cleanup("account-2", "run-1", generation));
        assert!(coordinator.is_owner("account-1", "run-1", generation));
    }
}

//! WebSocket Relay 客户端（多租户）
//!
//! 每个 account（proxy_token）建立一个独立 WebSocket 连接。
//! 共享同一个 device_id，但每个连接对应不同的 user_id。
//!
//! 职责：
//! - 连接后端中继，上报设备信息
//! - 30 秒心跳
//! - 断线 3 秒自动重连
//! - 接收 rpc_request，路由到 commands::dispatch 执行，返回 rpc_response
//! - 推送 rpc_progress（speedtest 等长任务）

use crate::commands::{self, CommandContext, ProgressPayload};
use crate::config::{AccountConfig, Config};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use sysinfo::System;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

/// 心跳间隔
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
/// 心跳超时（超过此时间未收到 pong 判定连接异常，主动断开重连）
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(60);
/// 重连延迟
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

/// 收集本机系统信息
fn collect_sys_info() -> (String, String) {
    let mut sys = System::new_all();
    sys.refresh_all();

    // 操作系统信息
    let os_info = {
        let os_type = System::os_version().unwrap_or_default();
        let os_name = System::name().unwrap_or_else(|| "Linux".to_string());
        format!("{} {}", os_name, os_type).trim().to_string()
    };

    // 主机名
    let hostname = System::host_name().unwrap_or_else(|| "unknown".to_string());

    (os_info, hostname)
}

/// 启动多租户 relay 客户端
///
/// 为每个 account 创建独立的 WebSocket 连接，共享同一个 device_id。
pub async fn run_multi_tenant(cfg: Config, config_path: String) -> anyhow::Result<()> {
    // 获取或生成 device_id（所有 account 共享）
    let device_id = crate::config::get_or_create_device_id(&cfg.server.data_dir)?;

    // 收集系统信息
    let (os_info, hostname) = collect_sys_info();
    info!("device_id: {}", device_id);
    info!("os_info: {}", os_info);
    info!("hostname: {}", hostname);

    // 为每个 account 启动独立连接
    let mut handles = Vec::new();

    let session_id = uuid::Uuid::new_v4().to_string();
    for (idx, account) in cfg.accounts.iter().enumerate() {
        let device_id = device_id.clone();
        let os_info = os_info.clone();
        let hostname = hostname.clone();
        let backend_url = cfg.server.backend_url.clone();
        let data_dir = cfg.server.data_dir.clone();
        let config_path = config_path.clone();
        let account = account.clone();

        let reporter_backend_url = backend_url.clone();
        let reporter_data_dir = data_dir.clone();
        let reporter_token = account.proxy_token.clone();
        let start_event = crate::telemetry::UsageEvent::new(
            "app_start",
            serde_json::json!({ "device_type": "daemon" }),
            &session_id,
        );
        if let Err(err) =
            crate::telemetry::enqueue(&reporter_data_dir, &reporter_token, &start_event)
        {
            warn!("[telemetry] 写入启动事件失败: {}", err);
        }
        tokio::spawn(crate::telemetry::run_reporter(
            reporter_backend_url,
            reporter_data_dir,
            reporter_token,
        ));

        let handle = tokio::spawn(async move {
            info!(
                "[account {}] 启动连接: device_name={}",
                idx, account.device_name
            );
            run_single_account(
                idx,
                account,
                device_id,
                os_info,
                hostname,
                backend_url,
                data_dir,
                config_path,
            )
            .await;
        });
        handles.push(handle);
    }

    // 等待所有连接（通常不会退出，除非全部失败）
    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}

/// 运行单个 account 的 WebSocket 连接（含重连）
///
/// 每次重连前重读配置文件，获取该账号（按 idx 定位）的最新 proxy_token，
/// 支持 update_proxy_token 命令热更新令牌后自动用新令牌重连。
#[allow(clippy::too_many_arguments)]
async fn run_single_account(
    idx: usize,
    account: AccountConfig,
    device_id: String,
    os_info: String,
    hostname: String,
    backend_url: String,
    data_dir: String,
    config_path: String,
) {
    let mut current_token = account.proxy_token.clone();
    loop {
        // 重读配置：令牌可能已被 update_proxy_token 命令热更新
        // 配置读取失败时沿用上次的 account 继续重试（可能是临时 IO 错误）
        let account = match load_account_at(&config_path, idx) {
            Ok(Some(acc)) => acc,
            Ok(None) => {
                info!("[account {}] 账号已从配置移除，停止连接任务", idx);
                break;
            }
            Err(e) => {
                warn!("[account {}] 重读配置失败: {}，沿用当前令牌重连", idx, e);
                AccountConfig {
                    proxy_token: current_token.clone(),
                    device_name: account.device_name.clone(),
                }
            }
        };
        if account.proxy_token != current_token {
            info!(
                "[account {}] 检测到 proxy_token 已更新（前 8 位 {}...），使用新令牌重连",
                idx,
                &account.proxy_token[..account.proxy_token.len().min(8)]
            );
            current_token = account.proxy_token.clone();
        }

        match connect_and_run(
            idx,
            &account,
            &device_id,
            &os_info,
            &hostname,
            &backend_url,
            &data_dir,
            &config_path,
        )
        .await
        {
            Ok(()) => {
                info!("[account {}] 连接正常退出", idx);
                break;
            }
            Err(e) => {
                warn!(
                    "[account {}] 连接断开: {}，{} 秒后重连",
                    idx,
                    e,
                    RECONNECT_DELAY.as_secs()
                );
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

/// 读取配置中 idx 位置的账号（idx 为 0-based）
///
/// - `Ok(Some(acc))`：账号存在，返回最新配置
/// - `Ok(None)`：账号已被删除（索引越界）
/// - `Err(e)`：配置文件读取/解析失败（调用方可沿用旧配置重试）
fn load_account_at(config_path: &str, idx: usize) -> anyhow::Result<Option<AccountConfig>> {
    let path = std::path::Path::new(config_path);
    let cfg = crate::config::load_config(path)?;
    Ok(cfg.accounts.get(idx).cloned())
}

/// 连接 WebSocket 并处理消息
#[allow(clippy::too_many_arguments)]
async fn connect_and_run(
    idx: usize,
    account: &AccountConfig,
    device_id: &str,
    os_info: &str,
    hostname: &str,
    backend_url: &str,
    data_dir: &str,
    config_path: &str,
) -> anyhow::Result<()> {
    // 构建 WebSocket URL
    // wss://api.cct.zdzz.top/api/devices/ws?token=xxx&deviceId=xxx&deviceType=daemon&osInfo=xxx&hostname=xxx&interconnect=1
    let ws_url = format!(
        "{}/api/devices/ws?token={}&deviceId={}&deviceType=daemon&osInfo={}&hostname={}&interconnect=1&capabilities={}",
        backend_url,
        urlencoding::encode(&account.proxy_token),
        device_id,
        urlencoding::encode(os_info),
        urlencoding::encode(hostname),
        urlencoding::encode("{\"dns_failover_probe\":1,\"full_chain_test\":2}"),
    );

    info!("[account {}] 连接: {}", idx, backend_url);

    let (ws_stream, _response) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("WebSocket 连接失败: {}", e))?;

    info!("[account {}] 连接成功", idx);

    let (mut write, mut read) = ws_stream.split();

    // 心跳定时器
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    // 最近一次收到 pong 的时间（用于心跳超时检测）
    let mut last_pong = Instant::now();

    // 进度推送通道（speedtest 等长任务使用）
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressPayload>();
    let (response_tx, mut response_rx) = mpsc::unbounded_channel::<String>();
    // 重连信号（update_proxy_token 成功后触发，响应发完后断开并用新令牌重连）
    let (reconnect_tx, mut reconnect_rx) = mpsc::unbounded_channel::<()>();

    // 进度推送上下文（共享给命令处理器）
    let ctx_progress_tx = Arc::new(Mutex::new(Some(progress_tx)));

    loop {
        tokio::select! {
            // 令牌热更新：等待响应发出后断开重连（run_single_account 会用新令牌重连）
            Some(_) = reconnect_rx.recv() => {
                // 给 in-flight 的 rpc_response 留出发送窗口
                tokio::time::sleep(Duration::from_millis(500)).await;
                info!("[account {}] proxy_token 已热更新，断开重连", idx);
                let _ = write.send(Message::Close(None)).await;
                return Err(anyhow::anyhow!("proxy_token 已更新，主动重连"));
            }

            // 心跳
            _ = heartbeat.tick() => {
                // 心跳超时检测：超过 HEARTBEAT_TIMEOUT 未收到 pong，主动断开重连
                if last_pong.elapsed() > HEARTBEAT_TIMEOUT {
                    warn!("[account {}] 心跳超时（{} 秒未收到 pong），主动断开重连",
                          idx, last_pong.elapsed().as_secs());
                    let _ = write.send(Message::Close(None)).await;
                    return Err(anyhow::anyhow!("心跳超时"));
                }
                let ping_msg = serde_json::json!({ "type": "ping" });
                if write.send(Message::Text(ping_msg.to_string())).await.is_err() {
                    return Err(anyhow::anyhow!("发送心跳失败"));
                }
            }

            // 进度推送
            Some(progress) = progress_rx.recv() => {
                let progress_msg = serde_json::json!({
                    "type": "rpc_progress",
                    "requestId": progress.request_id,
                    "progress": progress.progress,
                    "stage": progress.stage,
                    "speedMbps": progress.speed_mbps,
                });
                if write.send(Message::Text(progress_msg.to_string())).await.is_err() {
                    warn!("[account {}] 发送进度失败", idx);
                }
            }

            Some(response) = response_rx.recv() => {
                if write.send(Message::Text(response)).await.is_err() {
                    warn!("[account {}] 发送 RPC 响应失败", idx);
                }
            }

            // 接收消息
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(e) = handle_message(
                            &text,
                            device_id,
                            data_dir,
                            config_path,
                            &account.proxy_token,
                            &idx.to_string(),
                            backend_url,
                            &ctx_progress_tx,
                            &response_tx,
                            &reconnect_tx,
                            &mut last_pong,
                        ).await {
                            warn!("[account {}] 处理消息出错: {}", idx, e);
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        // 忽略二进制消息
                    }
                    Some(Ok(Message::Ping(_))) => {
                        // tungstenite 自动回复 pong
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) => {
                        info!("[account {}] 服务端关闭连接", idx);
                        return Err(anyhow::anyhow!("服务端关闭连接"));
                    }
                    Some(Err(e)) => {
                        return Err(anyhow::anyhow!("WebSocket 读取错误: {}", e));
                    }
                    None => {
                        return Err(anyhow::anyhow!("WebSocket 流结束"));
                    }
                    _ => {}
                }
            }
        }
    }
}

/// 处理收到的 WebSocket 消息
#[allow(clippy::too_many_arguments)]
async fn handle_message(
    text: &str,
    device_id: &str,
    data_dir: &str,
    config_path: &str,
    proxy_token: &str,
    account_id: &str,
    backend_url: &str,
    progress_tx: &Arc<Mutex<Option<mpsc::UnboundedSender<ProgressPayload>>>>,
    response_tx: &mpsc::UnboundedSender<String>,
    reconnect_tx: &mpsc::UnboundedSender<()>,
    last_pong: &mut Instant,
) -> anyhow::Result<()> {
    let msg: serde_json::Value =
        serde_json::from_str(text).map_err(|e| anyhow::anyhow!("JSON 解析失败: {}", e))?;

    let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        "pong" => {
            // 心跳响应，更新最近 pong 时间
            *last_pong = Instant::now();
        }
        "device_online" | "device_offline" => {
            // 设备上下线通知，记录日志即可
            let device_name = msg
                .get("deviceName")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            info!("[relay] {}: {}", msg_type, device_name);
        }
        "rpc_request" => {
            handle_rpc_request(
                &msg,
                device_id,
                data_dir,
                config_path,
                proxy_token,
                account_id,
                backend_url,
                progress_tx,
                response_tx,
                reconnect_tx,
            );
        }
        "rpc_cancel" => {
            let run_id = msg
                .get("runId")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if !run_id.is_empty() {
                commands::cancel_run(account_id, run_id);
            }
        }
        "update_available" => {
            // 后端推送的更新通知（预留：自动更新功能）
            let version = msg
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            info!("[relay] 收到更新通知: v{}", version);

            let ctx = CommandContext {
                device_id: device_id.to_string(),
                data_dir: data_dir.to_string(),
                config_path: config_path.to_string(),
                proxy_token: proxy_token.to_string(),
                account_id: account_id.to_string(),
                backend_url: backend_url.to_string(),
                user_id: None,
                request_id: String::new(),
                progress_tx: progress_tx.clone(),
            };
            // 异步处理，不阻塞消息循环
            tokio::spawn(async move {
                crate::commands::daemon_update::handle_update_notification(&ctx, &version).await;
            });
        }
        _ => {
            warn!("[relay] 未知消息类型: {}", msg_type);
        }
    }

    Ok(())
}

/// 处理 RPC 请求
#[allow(clippy::too_many_arguments)]
fn handle_rpc_request(
    msg: &serde_json::Value,
    device_id: &str,
    data_dir: &str,
    config_path: &str,
    proxy_token: &str,
    account_id: &str,
    backend_url: &str,
    progress_tx: &Arc<Mutex<Option<mpsc::UnboundedSender<ProgressPayload>>>>,
    response_tx: &mpsc::UnboundedSender<String>,
    reconnect_tx: &mpsc::UnboundedSender<()>,
) {
    let request_id = msg
        .get("requestId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let command = msg
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let params = msg
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    info!("[rpc] 收到请求: {} command={}", request_id, command);

    // 构建命令上下文
    let user_id = msg.get("userId").and_then(|v| v.as_i64());

    let ctx = CommandContext {
        device_id: device_id.to_string(),
        data_dir: data_dir.to_string(),
        config_path: config_path.to_string(),
        proxy_token: proxy_token.to_string(),
        account_id: account_id.to_string(),
        backend_url: backend_url.to_string(),
        user_id,
        request_id: request_id.clone(),
        progress_tx: progress_tx.clone(),
    };
    let response_tx = response_tx.clone();
    let reconnect_tx = reconnect_tx.clone();
    let is_token_update = command == "update_proxy_token";
    tokio::spawn(async move {
        let result = commands::dispatch(&command, &params, &ctx).await;
        let succeeded = result.is_ok();
        let response = match result {
            Ok(data) => serde_json::json!({
                "type": "rpc_response",
                "requestId": request_id,
                "success": true,
                "data": data,
                "error": null,
            }),
            Err(err) => serde_json::json!({
                "type": "rpc_response",
                "requestId": request_id,
                "success": false,
                "data": null,
                "error": err,
            }),
        };
        let _ = response_tx.send(response.to_string());
        info!("[rpc] 请求完成: {} command={}", request_id, command);

        // 令牌更新成功后触发重连（连接层会先等响应发出再断开）
        if is_token_update && succeeded {
            let _ = reconnect_tx.send(());
        }
    });
}

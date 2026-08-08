//! 命令处理器模块
//!
//! 被 relay 调用，执行具体的远程命令。
//! 所有命令返回 serde_json::Value，与 API 需求文档 6.1-6.5 对齐。

pub mod ping;
pub mod tcping;
pub mod speedtest;
pub mod delete_my_data;
pub mod daemon_config;
pub mod daemon_service;
pub mod daemon_update;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

/// 命令执行上下文
#[derive(Debug, Clone)]
pub struct CommandContext {
    /// 设备 ID（本机）
    pub device_id: String,
    /// 数据目录
    pub data_dir: String,
    /// 配置文件路径
    pub config_path: String,
    /// 关联的 user_id（从 WebSocket 连接中获取，用于多租户隔离）
    pub user_id: Option<i64>,
    /// 当前 RPC 请求的 requestId（用于进度推送关联）
    pub request_id: String,
    /// 进度回调（用于 speedtest 等长任务）
    pub progress_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<ProgressPayload>>>>,
}

/// 进度推送 payload
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressPayload {
    pub request_id: String,
    pub progress: f64,
    pub stage: String,
    pub speed_mbps: f64,
}

/// RPC 错误
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// 命令处理结果
pub type CommandResult = Result<serde_json::Value, RpcError>;

/// 路由命令到对应处理器
pub async fn dispatch(
    command: &str,
    params: &serde_json::Value,
    ctx: &CommandContext,
) -> CommandResult {
    match command {
        // ===== 测试命令 =====
        "ping" => ping::handle(params).await,
        "tcping" => tcping::handle(params).await,
        "node_latency" => {
            // 组合命令：ping + tcping 并行
            let ping_params = serde_json::json!({
                "host": params.get("node").and_then(|v| v.as_str()).unwrap_or(""),
                "count": params.get("count").and_then(|v| v.as_u64()).unwrap_or(4) as u32,
            });
            let port = params.get("port").and_then(|v| v.as_u64()).unwrap_or(7000) as u16;
            let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(4) as u32;

            let ping_result = ping::handle(&ping_params).await?;
            let tcping_params = serde_json::json!({
                "host": params.get("node").and_then(|v| v.as_str()).unwrap_or(""),
                "port": port,
                "count": count,
            });
            let tcping_result = tcping::handle(&tcping_params).await?;

            Ok(serde_json::json!({
                "ping": ping_result,
                "tcping": tcping_result,
            }))
        }
        "speedtest" => speedtest::handle(params, ctx).await,
        "delete_my_data" => delete_my_data::handle(ctx).await,

        // ===== Daemon 管理命令 =====
        "daemon_get_config" => daemon_config::get_config(ctx).await,
        "daemon_add_account" => daemon_config::add_account(params, ctx).await,
        "daemon_modify_account" => daemon_config::modify_account(params, ctx).await,
        "daemon_delete_account" => daemon_config::delete_account(params, ctx).await,
        "daemon_set_backend_url" => daemon_config::set_backend_url(params, ctx).await,

        "daemon_service_control" => daemon_service::service_control(params, ctx).await,
        "daemon_get_logs" => daemon_service::get_logs(params, ctx).await,

        "daemon_check_update" => daemon_update::check_update(ctx).await,
        "daemon_perform_update" => daemon_update::perform_update(ctx).await,
        "daemon_get_update_settings" => daemon_update::get_update_settings(ctx).await,
        "daemon_set_auto_update" => daemon_update::set_auto_update(params, ctx).await,

        _ => Err(RpcError::new("UNKNOWN_COMMAND", format!("不支持的命令: {}", command))),
    }
}

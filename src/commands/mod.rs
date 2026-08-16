//! 命令处理器模块
//!
//! 被 relay 调用，执行具体的远程命令。
//! 所有命令返回 serde_json::Value，与 API 需求文档 6.1-6.5 对齐。

pub mod auth;
pub mod daemon_config;
pub mod daemon_service;
pub mod daemon_update;
pub mod delete_my_data;
pub mod dns_failover_probe;
pub mod e2e_server;
pub mod ping;
pub mod speedtest;
pub mod tcp_speed_test;
pub mod tcping;
pub mod tunnel_latency_test;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::Mutex;

#[derive(Default)]
struct RunRegistry {
    entries: HashMap<String, RunState>,
    order: VecDeque<String>,
    next_generation: u64,
}

const MAX_RUN_ENTRIES: usize = 4096;

struct RunState {
    generation: u64,
    cancelled: bool,
}

impl RunRegistry {
    fn generation(&mut self, run_id: &str) -> u64 {
        if let Some(state) = self.entries.get(run_id) {
            return state.generation;
        }
        self.next_generation = self.next_generation.saturating_add(1);
        let generation = self.next_generation;
        self.entries.insert(
            run_id.to_string(),
            RunState {
                generation,
                cancelled: false,
            },
        );
        self.order.push_back(run_id.to_string());
        while self.entries.len() > MAX_RUN_ENTRIES {
            if let Some(expired) = self.order.pop_front() {
                self.entries.remove(&expired);
            }
        }
        generation
    }

    fn cancel(&mut self, run_id: &str) {
        let generation = self.generation(run_id);
        self.entries.insert(
            run_id.to_string(),
            RunState {
                generation,
                cancelled: true,
            },
        );
    }

    fn is_cancelled(&self, run_id: &str, generation: u64) -> bool {
        self.entries
            .get(run_id)
            .map(|state| state.generation != generation || state.cancelled)
            .unwrap_or(true)
    }

    fn finish(&mut self, run_id: &str, generation: u64) -> bool {
        if self.entries.get(run_id).map(|state| state.generation) != Some(generation) {
            return false;
        }
        self.entries.remove(run_id);
        self.order.retain(|key| key != run_id);
        true
    }
}

static RUN_REGISTRY: Lazy<StdMutex<RunRegistry>> =
    Lazy::new(|| StdMutex::new(RunRegistry::default()));

fn run_key(account_id: &str, run_id: &str) -> String {
    format!("{}:{}", account_id, run_id)
}

pub fn run_generation(account_id: &str, run_id: &str) -> u64 {
    let key = run_key(account_id, run_id);
    RUN_REGISTRY
        .lock()
        .map(|mut registry| registry.generation(&key))
        .unwrap_or(0)
}

pub fn cancel_run(account_id: &str, run_id: &str) {
    let key = run_key(account_id, run_id);
    if let Ok(mut registry) = RUN_REGISTRY.lock() {
        registry.cancel(&key);
    }
}

pub fn is_run_cancelled(account_id: &str, run_id: &str, generation: u64) -> bool {
    let key = run_key(account_id, run_id);
    RUN_REGISTRY
        .lock()
        .map(|registry| registry.is_cancelled(&key, generation))
        .unwrap_or(true)
}

pub fn finish_run(account_id: &str, run_id: &str, generation: u64) -> bool {
    let key = run_key(account_id, run_id);
    RUN_REGISTRY
        .lock()
        .map(|mut registry| registry.finish(&key, generation))
        .unwrap_or(false)
}

/// 命令执行上下文
#[derive(Debug, Clone)]
pub struct CommandContext {
    /// 设备 ID（本机）
    pub device_id: String,
    /// 数据目录
    pub data_dir: String,
    /// 配置文件路径
    pub config_path: String,
    pub proxy_token: String,
    pub account_id: String,
    /// 后端地址（用于 /auth/refresh 等）
    pub backend_url: String,
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
        "tcp_speed_test" => tcp_speed_test::handle(params, ctx).await,
        "tunnel_latency_test" => tunnel_latency_test::handle(params, ctx).await,
        "dns_failover_probe_v1" => dns_failover_probe::handle(params).await,
        "e2e_setup" => e2e_server::handle_setup(params, ctx).await,
        "e2e_cleanup" => e2e_server::handle_cleanup(params, ctx).await,
        "delete_my_data" => delete_my_data::handle(ctx).await,

        // ===== Daemon 管理命令 =====
        "daemon_get_config" => daemon_config::get_config(ctx).await,
        "daemon_add_account" => daemon_config::add_account(params, ctx).await,
        "daemon_modify_account" => daemon_config::modify_account(params, ctx).await,
        "daemon_delete_account" => daemon_config::delete_account(params, ctx).await,
        "daemon_set_backend_url" => daemon_config::set_backend_url(params, ctx).await,
        "update_proxy_token" => daemon_config::update_proxy_token(params, ctx).await,

        "daemon_service_control" => daemon_service::service_control(params, ctx).await,
        "daemon_get_logs" => daemon_service::get_logs(params, ctx).await,

        "daemon_check_update" => daemon_update::check_update(ctx).await,
        "daemon_perform_update" => daemon_update::perform_update(ctx).await,
        "daemon_get_update_settings" => daemon_update::get_update_settings(ctx).await,
        "daemon_set_auto_update" => daemon_update::set_auto_update(params, ctx).await,

        _ => Err(RpcError::new(
            "UNKNOWN_COMMAND",
            format!("不支持的命令: {}", command),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{RunRegistry, MAX_RUN_ENTRIES};

    #[test]
    fn old_generation_cannot_clear_reused_run() {
        let mut registry = RunRegistry::default();
        let first = registry.generation("run-1");
        assert!(registry.finish("run-1", first));
        let second = registry.generation("run-1");
        assert_ne!(first, second);
        assert!(!registry.finish("run-1", first));
        registry.cancel("run-1");
        assert!(registry.is_cancelled("run-1", second));
    }

    #[test]
    fn old_generation_is_cancelled_after_reuse() {
        let mut registry = RunRegistry::default();
        let first = registry.generation("run-1");
        assert!(registry.finish("run-1", first));
        let second = registry.generation("run-1");
        assert!(registry.is_cancelled("run-1", first));
        assert!(!registry.is_cancelled("run-1", second));
    }

    #[test]
    fn registry_discards_oldest_entries_at_capacity() {
        let mut registry = RunRegistry::default();
        for index in 0..=MAX_RUN_ENTRIES {
            registry.generation(&format!("run-{}", index));
        }
        assert_eq!(registry.entries.len(), MAX_RUN_ENTRIES);
        assert!(!registry.entries.contains_key("run-0"));
    }
}

//! Daemon 服务控制与日志 RPC 命令
//!
//! - service_control: 启动/停止/重启/查询状态
//! - get_logs: 获取最近日志（journalctl）

use crate::commands::{CommandContext, CommandResult, RpcError};
use serde::Deserialize;

const APP_NAME: &str = "chmlfrp-toolbox-daemon";

/// 检测当前用户是否为 root
#[cfg(unix)]
fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}
#[cfg(not(unix))]
fn is_root() -> bool {
    true
}

/// 构造 systemctl 命令（非 root 时自动加 sudo 前缀）
fn build_systemctl_cmd(args: &[&str]) -> std::process::Command {
    if is_root() {
        let mut cmd = std::process::Command::new("systemctl");
        cmd.args(args);
        cmd
    } else {
        let mut cmd = std::process::Command::new("sudo");
        cmd.arg("systemctl");
        cmd.args(args);
        cmd
    }
}

/// 服务控制
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceControlParams {
    pub action: String,
}

pub async fn service_control(params: &serde_json::Value, _ctx: &CommandContext) -> CommandResult {
    let p: ServiceControlParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new("INVALID_PARAMS", e.to_string()))?;

    match p.action.as_str() {
        "status" => {
            let status = get_service_status();
            Ok(serde_json::json!({
                "success": true,
                "status": status,
            }))
        }
        "restart" => {
            // 通过 systemd 重启自身
            // 注意：重启会导致当前进程退出，WebSocket 连接断开
            // 客户端需要在收到响应后等待重连
            let output = build_systemctl_cmd(&["restart", APP_NAME])
                .output()
                .map_err(|e| RpcError::new("SERVICE_CONTROL_FAILED", format!("执行 systemctl 失败: {}", e)))?;

            if output.status.success() {
                Ok(serde_json::json!({
                    "success": true,
                    "message": "重启指令已发送，服务将在几秒后重新连接",
                }))
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(RpcError::new("SERVICE_CONTROL_FAILED", format!("重启失败: {}", stderr)))
            }
        }
        "stop" => {
            let output = build_systemctl_cmd(&["stop", APP_NAME])
                .output()
                .map_err(|e| RpcError::new("SERVICE_CONTROL_FAILED", format!("执行 systemctl 失败: {}", e)))?;

            if output.status.success() {
                Ok(serde_json::json!({
                    "success": true,
                    "message": "停止指令已发送",
                }))
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(RpcError::new("SERVICE_CONTROL_FAILED", format!("停止失败: {}", stderr)))
            }
        }
        "start" => {
            // 启动服务（通常在 stop 后使用）
            let output = build_systemctl_cmd(&["start", APP_NAME])
                .output()
                .map_err(|e| RpcError::new("SERVICE_CONTROL_FAILED", format!("执行 systemctl 失败: {}", e)))?;

            if output.status.success() {
                Ok(serde_json::json!({
                    "success": true,
                    "message": "启动指令已发送",
                }))
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(RpcError::new("SERVICE_CONTROL_FAILED", format!("启动失败: {}", stderr)))
            }
        }
        other => Err(RpcError::new("INVALID_PARAMS", format!("不支持的操作: {}（支持 start/stop/restart/status）", other))),
    }
}

/// 获取服务状态
fn get_service_status() -> serde_json::Value {
    let active_state = std::process::Command::new("systemctl")
        .args(&["is-active", APP_NAME])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let enabled_state = std::process::Command::new("systemctl")
        .args(&["is-enabled", APP_NAME])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    serde_json::json!({
        "activeState": active_state,
        "enabledState": enabled_state,
    })
}

/// 获取日志
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLogsParams {
    /// 日志行数（默认 50）
    pub lines: Option<usize>,
}

pub async fn get_logs(params: &serde_json::Value, _ctx: &CommandContext) -> CommandResult {
    let p: GetLogsParams = serde_json::from_value(params.clone())
        .unwrap_or(GetLogsParams { lines: None });

    let lines = p.lines.unwrap_or(50).min(500);

    // journalctl 读取日志需要 root 权限（部分系统）
    let output = if is_root() {
        std::process::Command::new("journalctl")
            .args(&["-u", APP_NAME, "--no-pager", "-n", &lines.to_string()])
            .output()
            .map_err(|e| RpcError::new("LOG_FETCH_FAILED", format!("执行 journalctl 失败: {}", e)))?
    } else {
        std::process::Command::new("sudo")
            .args(&["journalctl", "-u", APP_NAME, "--no-pager", "-n", &lines.to_string()])
            .output()
            .map_err(|e| RpcError::new("LOG_FETCH_FAILED", format!("执行 journalctl 失败: {}", e)))?
    };

    let logs = String::from_utf8_lossy(&output.stdout).to_string();

    Ok(serde_json::json!({
        "success": true,
        "logs": logs,
        "lines": lines,
    }))
}

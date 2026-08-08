//! Daemon 更新管理 RPC 命令
//!
//! - check_update: 检查 GitHub Releases 是否有新版本
//! - perform_update: 下载并安装新版本（dpkg + systemctl restart）
//! - get_update_settings: 获取更新设置（自动更新开关）
//! - set_auto_update: 设置自动更新开关
//! - handle_update_notification: 处理后端推送的更新通知（预留）

use crate::commands::{CommandContext, CommandResult, RpcError};
use crate::config;
use serde::Deserialize;
use std::path::Path;
use tracing::{info, warn};

const GITHUB_REPO: &str = "zhengddzz/chmlfrp-toolbox-daemon";
const GITHUB_API: &str = "https://api.github.com/repos/zhengddzz/chmlfrp-toolbox-daemon/releases/latest";
const APP_NAME: &str = "chmlfrp-toolbox-daemon";

/// 获取当前 Daemon 版本
fn get_current_version() -> String {
    // 从 Cargo.toml 编译时注入的版本号
    env!("CARGO_PKG_VERSION").to_string()
}

/// 检查更新
pub async fn check_update(_ctx: &CommandContext) -> CommandResult {
    let current = get_current_version();
    info!("[update] 检查更新，当前版本: v{}", current);

    let client = reqwest::Client::builder()
        .user_agent(format!("{}/{}", APP_NAME, current))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| RpcError::new("HTTP_CLIENT_FAILED", e.to_string()))?;

    let resp = client
        .get(GITHUB_API)
        .send()
        .await
        .map_err(|e| RpcError::new("FETCH_RELEASE_FAILED", format!("获取 Release 信息失败: {}", e)))?;

    if !resp.status().is_success() {
        return Err(RpcError::new(
            "FETCH_RELEASE_FAILED",
            format!("GitHub API 返回: {}", resp.status()),
        ));
    }

    let release: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| RpcError::new("PARSE_RELEASE_FAILED", e.to_string()))?;

    let tag_name = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();

    let release_name = release
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let release_notes = release
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let published_at = release
        .get("published_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // 查找适合当前架构的 deb 包
    let arch = std::env::consts::ARCH;
    let arch_suffix = match arch {
        "x86_64" => "amd64.deb",
        "aarch64" => "arm64.deb",
        _ => "amd64.deb",
    };

    let download_url = release
        .get("assets")
        .and_then(|v| v.as_array())
        .and_then(|assets| {
            assets.iter().find_map(|asset| {
                let name = asset.get("name").and_then(|v| v.as_str())?;
                let url = asset.get("browser_download_url").and_then(|v| v.as_str())?;
                if name.ends_with(arch_suffix) {
                    Some(url.to_string())
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();

    let has_update = version_gt(&tag_name, &current);

    Ok(serde_json::json!({
        "success": true,
        "currentVersion": current,
        "latestVersion": tag_name,
        "releaseName": release_name,
        "releaseNotes": release_notes,
        "publishedAt": published_at,
        "downloadUrl": download_url,
        "hasUpdate": has_update,
    }))
}

/// 执行更新（下载 deb + dpkg 安装 + 重启服务）
pub async fn perform_update(ctx: &CommandContext) -> CommandResult {
    info!("[update] 开始执行更新流程");

    // 1. 检查更新信息
    let update_info = check_update(ctx).await?;
    let has_update = update_info
        .get("hasUpdate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !has_update {
        return Ok(serde_json::json!({
            "success": false,
            "message": "当前已是最新版本，无需更新",
        }));
    }

    let download_url = update_info
        .get("downloadUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if download_url.is_empty() {
        return Err(RpcError::new("NO_DOWNLOAD_URL", "未找到适合当前架构的安装包"));
    }

    let latest_version = update_info
        .get("latestVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // 2. 下载 deb 包
    let tmp_dir = "/tmp";
    let deb_path = format!("{}/{}_update.deb", tmp_dir, APP_NAME);

    let client = reqwest::Client::builder()
        .user_agent(format!("{}/{}", APP_NAME, get_current_version()))
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| RpcError::new("HTTP_CLIENT_FAILED", e.to_string()))?;

    let resp = client
        .get(download_url)
        .send()
        .await
        .map_err(|e| RpcError::new("DOWNLOAD_FAILED", format!("下载安装包失败: {}", e)))?;

    if !resp.status().is_success() {
        return Err(RpcError::new(
            "DOWNLOAD_FAILED",
            format!("下载返回: {}", resp.status()),
        ));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| RpcError::new("DOWNLOAD_FAILED", format!("读取响应体失败: {}", e)))?;

    std::fs::write(&deb_path, &bytes)
        .map_err(|e| RpcError::new("DOWNLOAD_FAILED", format!("写入临时文件失败: {}", e)))?;

    info!("[update] 安装包已下载: {} ({} bytes)", deb_path, bytes.len());

    // 3. dpkg 安装
    let output = std::process::Command::new("dpkg")
        .args(&["-i", &deb_path])
        .output()
        .map_err(|e| RpcError::new("INSTALL_FAILED", format!("执行 dpkg 失败: {}", e)))?;

    // 清理临时文件
    let _ = std::fs::remove_file(&deb_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // 尝试自动修复依赖
        let fix_output = std::process::Command::new("apt-get")
            .args(&["install", "-f", "-y"])
            .output();

        if let Ok(fo) = fix_output {
            if !fo.status.success() {
                return Err(RpcError::new(
                    "INSTALL_FAILED",
                    format!("dpkg 安装失败且依赖修复失败: {}", stderr),
                ));
            }
        } else {
            return Err(RpcError::new(
                "INSTALL_FAILED",
                format!("dpkg 安装失败: {}", stderr),
            ));
        }
    }

    info!("[update] 安装完成，正在重启服务...");

    // 4. 重启服务（注意：这会导致当前进程退出）
    // 使用单独的子进程执行重启，确保 RPC 响应能先返回
    std::process::Command::new("sh")
        .args(&[
            "-c",
            &format!("sleep 1 && systemctl restart {}", APP_NAME),
        ])
        .spawn()
        .map_err(|e| RpcError::new("RESTART_FAILED", format!("启动重启进程失败: {}", e)))?;

    Ok(serde_json::json!({
        "success": true,
        "message": format!("已更新到 v{}，服务正在重启...", latest_version),
        "newVersion": latest_version,
    }))
}

/// 获取更新设置
pub async fn get_update_settings(ctx: &CommandContext) -> CommandResult {
    let path = Path::new(&ctx.config_path);
    let cfg = config::load_config(path)
        .map_err(|e| RpcError::new("CONFIG_LOAD_FAILED", e.to_string()))?;

    Ok(serde_json::json!({
        "success": true,
        "autoUpdate": cfg.update.auto_update,
        "currentVersion": get_current_version(),
    }))
}

/// 设置自动更新开关
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAutoUpdateParams {
    pub enabled: bool,
}

pub async fn set_auto_update(params: &serde_json::Value, ctx: &CommandContext) -> CommandResult {
    let p: SetAutoUpdateParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new("INVALID_PARAMS", e.to_string()))?;

    let path = Path::new(&ctx.config_path);
    config::set_auto_update(path, p.enabled)
        .map_err(|e| RpcError::new("CONFIG_SAVE_FAILED", e.to_string()))?;

    Ok(serde_json::json!({
        "success": true,
        "message": if p.enabled { "已开启自动更新" } else { "已关闭自动更新" },
    }))
}

/// 比较版本号：返回 true 表示 v1 > v2
fn version_gt(v1: &str, v2: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.split('.')
            .filter_map(|p| p.parse::<u32>().ok())
            .collect()
    };
    let a = parse(v1);
    let b = parse(v2);
    for i in 0..a.len().max(b.len()) {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        if av > bv {
            return true;
        }
        if av < bv {
            return false;
        }
    }
    false
}

/// 处理后端推送的更新通知（预留接口）
///
/// 当后端通过 WebSocket 推送 `update_available` 消息时调用。
/// 如果开启了自动更新，自动执行 perform_update；否则仅记录日志。
pub async fn handle_update_notification(ctx: &CommandContext, version: &str) {
    let path = Path::new(&ctx.config_path);
    let cfg = match config::load_config(path) {
        Ok(c) => c,
        Err(e) => {
            warn!("[update] 收到更新通知但读取配置失败: {}", e);
            return;
        }
    };

    if cfg.update.auto_update {
        info!("[update] 收到更新通知 v{}，自动更新已开启，开始执行更新...", version);
        if let Err(e) = perform_update(ctx).await {
            warn!("[update] 自动更新失败: {}", e.message);
        }
    } else {
        info!("[update] 收到更新通知 v{}，自动更新未开启，忽略", version);
    }
}

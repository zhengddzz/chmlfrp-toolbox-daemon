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
///
/// 注意：下载使用 spawn_blocking 避免阻塞 tokio 运行时（防止心跳超时）。
/// 安装步骤通过 sudo 执行（安装脚本已配置 sudoers 免密规则）。
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

    // 判断包格式（deb 或 rpm）和是否需要 sudo
    let is_deb = download_url.ends_with(".deb");
    let is_rpm = download_url.ends_with(".rpm");
    let pkg_ext = if is_deb { "deb" } else if is_rpm { "rpm" } else { "deb" };

    // 当前用户是否为 root（root 不需要 sudo）
    #[cfg(unix)]
    let is_root = unsafe { libc::geteuid() } == 0;
    #[cfg(not(unix))]
    let is_root = true; // 非 Unix 环境无需 sudo

    // 2. 下载安装包（使用 spawn_blocking 避免阻塞心跳）
    let tmp_dir = "/tmp";
    let pkg_path = format!("{}/{}_update.{}", tmp_dir, APP_NAME, pkg_ext);
    let download_url_owned = download_url.to_string();
    let current_ver = get_current_version();
    let download_path = pkg_path.clone();

    let download_result = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(format!("{}/{}", APP_NAME, current_ver))
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| format!("HTTP 客户端构建失败: {}", e))?;

        let resp = client
            .get(&download_url_owned)
            .send()
            .map_err(|e| format!("下载安装包失败: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("下载返回: {}", resp.status()));
        }

        let bytes = resp
            .bytes()
            .map_err(|e| format!("读取响应体失败: {}", e))?;

        std::fs::write(&download_path, &bytes)
            .map_err(|e| format!("写入临时文件失败: {}", e))?;

        Ok(bytes.len())
    })
    .await
    .map_err(|e| RpcError::new("DOWNLOAD_FAILED", format!("下载任务异常: {}", e)))?;

    let downloaded_bytes = download_result
        .map_err(|e| RpcError::new("DOWNLOAD_FAILED", e))?;

    info!("[update] 安装包已下载: {} ({} bytes)", pkg_path, downloaded_bytes);

    // 3. 安装（使用 sudo，安装脚本已配置 sudoers 免密）
    let mut install_output = if is_deb {
        // deb 包：sudo dpkg -i
        let mut cmd = if is_root {
            std::process::Command::new("dpkg")
        } else {
            let mut c = std::process::Command::new("sudo");
            c.arg("dpkg");
            c
        };
        cmd.args(&["-i", &pkg_path]);
        cmd
    } else {
        // rpm 包：sudo rpm -U --force
        let mut cmd = if is_root {
            std::process::Command::new("rpm")
        } else {
            let mut c = std::process::Command::new("sudo");
            c.arg("rpm");
            c
        };
        cmd.args(&["-U", "--force", &pkg_path]);
        cmd
    };

    let output = install_output
        .output()
        .map_err(|e| RpcError::new("INSTALL_FAILED", format!("执行安装命令失败: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        warn!("[update] 安装失败: stderr={}, stdout={}", stderr, stdout);

        // deb 包尝试自动修复依赖
        if is_deb {
            info!("[update] 尝试修复依赖...");
            let fix_output = if is_root {
                std::process::Command::new("apt-get")
                    .args(&["install", "-f", "-y"])
                    .output()
            } else {
                std::process::Command::new("sudo")
                    .args(&["apt-get", "install", "-f", "-y"])
                    .output()
            };

            if let Ok(fo) = fix_output {
                if !fo.status.success() {
                    // 清理临时文件
                    let _ = std::fs::remove_file(&pkg_path);
                    return Err(RpcError::new(
                        "INSTALL_FAILED",
                        format!("dpkg 安装失败且依赖修复失败: {}", stderr),
                    ));
                }
            } else {
                let _ = std::fs::remove_file(&pkg_path);
                return Err(RpcError::new(
                    "INSTALL_FAILED",
                    format!("dpkg 安装失败: {}", stderr),
                ));
            }
        } else {
            let _ = std::fs::remove_file(&pkg_path);
            return Err(RpcError::new(
                "INSTALL_FAILED",
                format!("rpm 安装失败: {}", stderr),
            ));
        }
    }

    // 清理临时文件
    let _ = std::fs::remove_file(&pkg_path);

    info!("[update] 安装完成，正在重启服务...");

    // 4. 重启服务（使用 sudo systemctl restart）
    // 使用单独的子进程执行重启，确保 RPC 响应能先返回
    let restart_cmd = if is_root {
        format!("sleep 1 && systemctl restart {}", APP_NAME)
    } else {
        format!("sleep 1 && sudo systemctl restart {}", APP_NAME)
    };

    std::process::Command::new("sh")
        .args(&["-c", &restart_cmd])
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

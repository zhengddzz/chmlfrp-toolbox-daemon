//! Daemon 更新管理 RPC 命令
//!
//! - check_update: 检查 u.zdzz.top 更新源是否有新版本
//! - perform_update: 下载并安装新版本（dpkg + systemctl restart）
//! - get_update_settings: 获取更新设置（自动更新开关）
//! - set_auto_update: 设置自动更新开关
//! - handle_update_notification: 处理后端推送的更新通知
//!
//! 更新源：https://u.zdzz.top/api/toolbox-daemon
//! 进度推送：通过 ctx.progress_tx 实时推送每个步骤的日志（stage 字段携带日志文本）

use crate::commands::{CommandContext, CommandResult, ProgressPayload, RpcError};
use crate::config;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

const UPDATE_API: &str = "https://u.zdzz.top/api/toolbox-daemon";
const APP_NAME: &str = "chmlfrp-toolbox-daemon";

/// 获取当前 Daemon 版本
fn get_current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 异步推送进度（在 async 上下文中使用）
async fn send_progress(ctx: &CommandContext, progress: f64, stage: &str) {
    let tx = ctx.progress_tx.lock().await;
    if let Some(sender) = tx.as_ref() {
        let _ = sender.send(ProgressPayload {
            request_id: ctx.request_id.clone(),
            progress,
            stage: stage.to_string(),
            speed_mbps: 0.0,
        });
    }
}

/// 同步推送进度（在 spawn_blocking 中使用，try_lock 避免阻塞）
fn push_progress(
    tx: &Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<ProgressPayload>>>>,
    request_id: &str,
    progress: f64,
    stage: &str,
) {
    if let Ok(guard) = tx.try_lock() {
        if let Some(sender) = guard.as_ref() {
            let _ = sender.send(ProgressPayload {
                request_id: request_id.to_string(),
                progress,
                stage: stage.to_string(),
                speed_mbps: 0.0,
            });
        }
    }
}

/// 检查更新（请求 u.zdzz.top 更新源）
pub async fn check_update(_ctx: &CommandContext) -> CommandResult {
    let current = get_current_version();
    info!("[update] 检查更新，当前版本: v{}", current);

    let client = reqwest::Client::builder()
        .user_agent(format!("{}/{}", APP_NAME, current))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| RpcError::new("HTTP_CLIENT_FAILED", e.to_string()))?;

    let resp = client
        .get(UPDATE_API)
        .send()
        .await
        .map_err(|e| RpcError::new("FETCH_UPDATE_FAILED", format!("获取更新信息失败: {}", e)))?;

    if !resp.status().is_success() {
        return Err(RpcError::new(
            "FETCH_UPDATE_FAILED",
            format!("更新服务返回: {}", resp.status()),
        ));
    }

    let update_info: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| RpcError::new("PARSE_UPDATE_FAILED", e.to_string()))?;

    let latest_version = update_info
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let release_name = update_info
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let release_notes = update_info
        .get("releaseNotes")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let published_at = update_info
        .get("releaseDate")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // 从 platforms.linux 数组中查找适合当前架构的 deb/rpm 包
    let arch = std::env::consts::ARCH;
    let arch_key = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => "x64",
    };

    let linux_packages = update_info
        .get("platforms")
        .and_then(|p| p.get("linux"))
        .and_then(|l| l.as_array());

    // 优先匹配 deb 包，其次 rpm 包
    let (download_url, sha256) = linux_packages
        .and_then(|packages| {
            // 第一轮：精确匹配架构 + deb 格式
            packages.iter().find_map(|pkg| {
                let format = pkg.get("format").and_then(|v| v.as_str())?;
                let pkg_arch = pkg.get("arch").and_then(|v| v.as_str()).unwrap_or("");
                let url = pkg.get("url").and_then(|v| v.as_str())?;
                let hash = pkg.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
                if format == "deb" && pkg_arch == arch_key {
                    Some((url.to_string(), hash.to_string()))
                } else {
                    None
                }
            }).or_else(|| {
                // 第二轮：精确匹配架构 + rpm 格式
                packages.iter().find_map(|pkg| {
                    let format = pkg.get("format").and_then(|v| v.as_str())?;
                    let pkg_arch = pkg.get("arch").and_then(|v| v.as_str()).unwrap_or("");
                    let url = pkg.get("url").and_then(|v| v.as_str())?;
                    let hash = pkg.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
                    if format == "rpm" && pkg_arch == arch_key {
                        Some((url.to_string(), hash.to_string()))
                    } else {
                        None
                    }
                })
            }).or_else(|| {
                // 第三轮：任意架构 + deb 格式
                packages.iter().find_map(|pkg| {
                    let format = pkg.get("format").and_then(|v| v.as_str())?;
                    let url = pkg.get("url").and_then(|v| v.as_str())?;
                    let hash = pkg.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
                    if format == "deb" {
                        Some((url.to_string(), hash.to_string()))
                    } else {
                        None
                    }
                })
            })
        })
        .unwrap_or_default();

    let has_update = version_gt(&latest_version, &current);

    Ok(serde_json::json!({
        "success": true,
        "currentVersion": current,
        "latestVersion": latest_version,
        "releaseName": release_name,
        "releaseNotes": release_notes,
        "publishedAt": published_at,
        "downloadUrl": download_url,
        "sha256": sha256,
        "hasUpdate": has_update,
    }))
}

/// 执行更新（下载 deb + SHA-256 校验 + dpkg 安装 + 重启服务）
///
/// 通过 ctx.progress_tx 实时推送每个步骤的详细日志。
/// 下载使用 spawn_blocking + 分块读取，避免阻塞 tokio 运行时并支持实时进度。
/// 安装步骤通过 sudo -n 执行（安装脚本已配置 sudoers 免密规则）。
pub async fn perform_update(ctx: &CommandContext) -> CommandResult {
    info!("[update] 开始执行更新流程");

    send_progress(ctx, 5.0, "正在检查版本信息...").await;

    // 1. 检查更新信息
    let update_info = check_update(ctx).await?;
    let has_update = update_info
        .get("hasUpdate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !has_update {
        send_progress(ctx, 100.0, "当前已是最新版本，无需更新").await;
        return Ok(serde_json::json!({
            "success": false,
            "message": "当前已是最新版本，无需更新",
        }));
    }

    let download_url = update_info
        .get("downloadUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let expected_sha256 = update_info
        .get("sha256")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let latest_version = update_info
        .get("latestVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if download_url.is_empty() {
        send_progress(ctx, 100.0, "未找到适合当前架构的安装包").await;
        return Err(RpcError::new("NO_DOWNLOAD_URL", "未找到适合当前架构的安装包"));
    }

    send_progress(ctx, 10.0, &format!("发现新版本 v{}，准备下载...", latest_version)).await;

    // 判断包格式（deb 或 rpm）
    let is_deb = download_url.ends_with(".deb");
    let is_rpm = download_url.ends_with(".rpm");
    let pkg_ext = if is_deb { "deb" } else if is_rpm { "rpm" } else { "deb" };

    // 当前用户是否为 root（root 不需要 sudo）
    #[cfg(unix)]
    let is_root = unsafe { libc::geteuid() } == 0;
    #[cfg(not(unix))]
    let is_root = true;

    // 2. 下载安装包（分块下载 + 实时进度推送 + SHA-256 计算）
    let tmp_dir = "/tmp";
    let pkg_path = format!("{}/{}_update.{}", tmp_dir, APP_NAME, pkg_ext);
    let download_url_owned = download_url.to_string();
    let current_ver = get_current_version();
    let download_path = pkg_path.clone();
    let progress_tx = ctx.progress_tx.clone();
    let request_id = ctx.request_id.clone();

    send_progress(ctx, 15.0, &format!("正在下载 {} 安装包...", pkg_ext)).await;

    let download_result = tokio::task::spawn_blocking(move || -> Result<(usize, String), String> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(format!("{}/{}", APP_NAME, current_ver))
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| format!("HTTP 客户端构建失败: {}", e))?;

        let mut resp = client
            .get(&download_url_owned)
            .send()
            .map_err(|e| format!("下载安装包失败: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("下载返回: {}", resp.status()));
        }

        let total_size = resp.content_length().unwrap_or(0);
        let mut file = std::fs::File::create(&download_path)
            .map_err(|e| format!("创建临时文件失败: {}", e))?;

        let mut downloaded: u64 = 0;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 65536]; // 64KB 缓冲区
        loop {
            let n = resp
                .read(&mut buffer)
                .map_err(|e| format!("读取数据失败: {}", e))?;
            if n == 0 {
                break;
            }
            file.write_all(&buffer[..n])
                .map_err(|e| format!("写入文件失败: {}", e))?;
            hasher.update(&buffer[..n]);
            downloaded += n as u64;
            if total_size > 0 {
                let pct = 15.0 + (downloaded as f64 / total_size as f64) * 35.0; // 15-50
                let size_mb = downloaded as f64 / 1024.0 / 1024.0;
                let total_mb = total_size as f64 / 1024.0 / 1024.0;
                push_progress(
                    &progress_tx,
                    &request_id,
                    pct,
                    &format!("下载中... {:.1}/{:.1} MB ({:.0}%)", size_mb, total_mb, (pct - 15.0) / 35.0 * 100.0),
                );
            }
        }

        let sha256_hash = hasher.finalize();
        let sha256_hex = format!("{:x}", sha256_hash);

        Ok((downloaded as usize, sha256_hex))
    })
    .await
    .map_err(|e| RpcError::new("DOWNLOAD_FAILED", format!("下载任务异常: {}", e)))?;

    let (downloaded_bytes, actual_sha256) = download_result
        .map_err(|e| RpcError::new("DOWNLOAD_FAILED", e))?;

    let size_kb = downloaded_bytes / 1024;
    send_progress(ctx, 55.0, &format!("下载完成，大小: {} KB", size_kb)).await;
    info!("[update] 安装包已下载: {} ({} bytes)", pkg_path, downloaded_bytes);

    // 3. SHA-256 校验
    if !expected_sha256.is_empty() {
        send_progress(ctx, 60.0, "正在校验 SHA-256...").await;
        if actual_sha256 != expected_sha256 {
            let _ = std::fs::remove_file(&pkg_path);
            send_progress(ctx, 100.0, "SHA-256 校验失败").await;
            return Err(RpcError::new(
                "SHA256_MISMATCH",
                format!(
                    "SHA-256 校验失败: 期望 {}，实际 {}",
                    expected_sha256, actual_sha256
                ),
            ));
        }
        send_progress(ctx, 65.0, "SHA-256 校验通过").await;
        info!("[update] SHA-256 校验通过");
    } else {
        info!("[update] 更新源未提供 SHA-256，跳过校验");
    }

    // 4. 安装（使用 sudo -n，安装脚本已配置 sudoers 免密）
    let install_cmd_desc = if is_deb { "dpkg -i" } else { "rpm -U --force" };
    send_progress(ctx, 70.0, &format!("正在安装 ({}...)...", install_cmd_desc)).await;

    let mut install_cmd = if is_deb {
        let mut cmd = if is_root {
            std::process::Command::new("dpkg")
        } else {
            let mut c = std::process::Command::new("sudo");
            c.arg("-n").arg("dpkg");
            c
        };
        cmd.args(&["-i", &pkg_path]);
        cmd
    } else {
        let mut cmd = if is_root {
            std::process::Command::new("rpm")
        } else {
            let mut c = std::process::Command::new("sudo");
            c.arg("-n").arg("rpm");
            c
        };
        cmd.args(&["-U", "--force", &pkg_path]);
        cmd
    };

    let output = install_cmd
        .output()
        .map_err(|e| RpcError::new("INSTALL_FAILED", format!("执行安装命令失败: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        warn!("[update] 安装失败: stderr={}, stdout={}", stderr, stdout);

        // deb 包尝试自动修复依赖
        if is_deb {
            send_progress(ctx, 75.0, "安装失败，尝试修复依赖...").await;
            info!("[update] 尝试修复依赖...");

            let fix_output = if is_root {
                std::process::Command::new("apt-get")
                    .args(&["install", "-f", "-y"])
                    .output()
            } else {
                std::process::Command::new("sudo")
                    .arg("-n")
                    .args(&["apt-get", "install", "-f", "-y"])
                    .output()
            };

            if let Ok(fo) = fix_output {
                if !fo.status.success() {
                    let _ = std::fs::remove_file(&pkg_path);
                    send_progress(ctx, 100.0, &format!("安装失败: {}", stderr.trim())).await;
                    return Err(RpcError::new(
                        "INSTALL_FAILED",
                        format!("dpkg 安装失败且依赖修复失败: {}", stderr),
                    ));
                }
            } else {
                let _ = std::fs::remove_file(&pkg_path);
                send_progress(ctx, 100.0, &format!("安装失败: {}", stderr.trim())).await;
                return Err(RpcError::new(
                    "INSTALL_FAILED",
                    format!("dpkg 安装失败: {}", stderr),
                ));
            }
        } else {
            let _ = std::fs::remove_file(&pkg_path);
            send_progress(ctx, 100.0, &format!("安装失败: {}", stderr.trim())).await;
            return Err(RpcError::new(
                "INSTALL_FAILED",
                format!("rpm 安装失败: {}", stderr),
            ));
        }
    }

    // 清理临时文件
    let _ = std::fs::remove_file(&pkg_path);

    send_progress(ctx, 85.0, "安装完成").await;
    info!("[update] 安装完成，正在重启服务...");

    // 5. 重启服务（使用 sudo -n systemctl restart，避免 polkit 拦截）
    send_progress(ctx, 90.0, "正在重启服务...").await;

    let restart_cmd = if is_root {
        format!("sleep 1 && systemctl restart {}", APP_NAME)
    } else {
        format!("sleep 1 && sudo -n systemctl restart {}", APP_NAME)
    };

    std::process::Command::new("sh")
        .args(&["-c", &restart_cmd])
        .spawn()
        .map_err(|e| RpcError::new("RESTART_FAILED", format!("启动重启进程失败: {}", e)))?;

    send_progress(
        ctx,
        100.0,
        &format!("已更新到 v{}，服务正在重启...", latest_version),
    )
    .await;

    Ok(serde_json::json!({
        "success": true,
        "message": format!("已更新到 v{}，服务正在重启...", latest_version),
        "newVersion": latest_version,
    }))
}

/// 获取更新设置
///
/// auto_update 优先读 data_dir/update_settings.json（override），
/// fallback 到主配置文件 /etc/chmlfrp-toolbox-daemon/config.toml 中的 [update].auto_update。
pub async fn get_update_settings(ctx: &CommandContext) -> CommandResult {
    let path = Path::new(&ctx.config_path);
    let auto_update = config::get_effective_auto_update(path, &ctx.data_dir);

    Ok(serde_json::json!({
        "success": true,
        "autoUpdate": auto_update,
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

    // 写入 data_dir/update_settings.json（daemon 用户可写），
    // 避免直接修改 /etc/chmlfrp-toolbox-daemon/config.toml 导致 Read-only file system 错误
    config::save_auto_update_override(&ctx.data_dir, p.enabled)
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

/// 处理后端推送的更新通知
///
/// 当后端通过 WebSocket 推送 `update_available` 消息时调用。
/// 如果开启了自动更新，自动执行 perform_update；否则仅记录日志。
pub async fn handle_update_notification(ctx: &CommandContext, version: &str) {
    let path = Path::new(&ctx.config_path);
    let auto_update = config::get_effective_auto_update(path, &ctx.data_dir);

    if auto_update {
        info!("[update] 收到更新通知 v{}，自动更新已开启，开始执行更新...", version);
        if let Err(e) = perform_update(ctx).await {
            warn!("[update] 自动更新失败: {}", e.message);
        }
    } else {
        info!("[update] 收到更新通知 v{}，自动更新未开启，忽略", version);
    }
}

//! Daemon 更新管理 RPC 命令
//!
//! - check_update: 检查 u.zdzz.top 更新源是否有新版本
//! - perform_update: 下载并安装新版本（systemd-run + dpkg + systemctl restart）
//! - get_update_settings: 获取更新设置（自动更新开关）
//! - set_auto_update: 设置自动更新开关
//! - handle_update_notification: 处理后端推送的更新通知
//!
//! 更新源：https://u.zdzz.top/api/toolbox-daemon
//! 进度推送：通过 ctx.progress_tx 实时推送每个步骤的日志（stage 字段携带日志文本）
//! 安装方式：systemd-run 在沙箱外以 root 执行 dpkg（服务沙箱 ProtectSystem=strict
//! 会使 sudo 提权后的子进程仍处于只读 mount namespace，导致 dpkg 报错）

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

/// 更新流程全局互斥锁：自动更新通知与手动更新共享，
/// 防止并发任务争抢同一安装包路径 / 解压目录 / 包管理器锁
static UPDATE_MUTEX: once_cell::sync::Lazy<tokio::sync::Mutex<()>> =
    once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(()));

/// 将 Rust ARCH 常量映射为更新源的架构标识；未知架构返回 None（禁止回退 x64）
fn arch_key_for(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("x64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

/// 检查可执行文件是否存在于 PATH 中
fn binary_exists(name: &str) -> bool {
    std::env::var("PATH")
        .map(|path_env| {
            path_env.split(':').any(|dir| {
                !dir.is_empty() && Path::new(dir).join(name).exists()
            })
        })
        .unwrap_or(false)
}

/// 检测当前系统首选包格式：存在 dpkg → deb；否则存在 rpm → rpm；均无 → None
fn detect_preferred_format() -> Option<&'static str> {
    if binary_exists("dpkg") {
        Some("deb")
    } else if binary_exists("rpm") {
        Some("rpm")
    } else {
        None
    }
}

/// 从更新源 linux 包列表中严格匹配架构与包格式的安装包
///
/// 不做跨架构兜底、不做跨格式兜底：RPM 系发行版绝不下载 deb，反之亦然，
/// 否则会出现 CentOS 下载 deb 后调用不存在的 dpkg 导致更新失败。
fn select_package(
    packages: &[serde_json::Value],
    arch_key: &str,
    format: &str,
) -> Option<(String, String)> {
    packages.iter().find_map(|pkg| {
        let pkg_format = pkg.get("format")?.as_str()?;
        let pkg_arch = pkg.get("arch").and_then(|v| v.as_str()).unwrap_or("");
        let url = pkg.get("url")?.as_str()?;
        let hash = pkg.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
        if pkg_format == format && pkg_arch == arch_key {
            Some((url.to_string(), hash.to_string()))
        } else {
            None
        }
    })
}

/// 获取当前 Daemon 版本
fn get_current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 构建 root 特权命令：优先通过 systemd-run 逃逸服务沙箱
///
/// 服务启用 ProtectSystem=strict 后，sudo 提权的子进程仍处于服务的
/// 只读 mount namespace 中，dpkg 写 /var/lib/dpkg 会报
/// "Read-only file system"。systemd-run 通过 D-Bus 在系统 manager 中
/// 启动 transient unit，运行于系统默认 mount namespace，不受服务沙箱限制。
/// systemd-run 不可用（无 systemd 的容器环境）时回退为直接执行。
///
/// 注意：参数顺序与 install.sh 生成的 sudoers 规则逐字对应，勿随意调整。
fn build_escalated_cmd(program: &str, args: &[String], is_root: bool) -> std::process::Command {
    let use_systemd_run = std::process::Command::new("systemd-run")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let mut cmd = if use_systemd_run {
        if is_root {
            let mut c = std::process::Command::new("systemd-run");
            c.args(["--wait", "--pipe", "--quiet", program]);
            c
        } else {
            let mut c = std::process::Command::new("sudo");
            c.arg("-n")
                .arg("systemd-run")
                .args(["--wait", "--pipe", "--quiet", program]);
            c
        }
    } else if is_root {
        std::process::Command::new(program)
    } else {
        let mut c = std::process::Command::new("sudo");
        c.arg("-n").arg(program);
        c
    };
    cmd.args(args);
    cmd
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

    let resp =
        client.get(UPDATE_API).send().await.map_err(|e| {
            RpcError::new("FETCH_UPDATE_FAILED", format!("获取更新信息失败: {}", e))
        })?;

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

    // 严格匹配当前架构与当前包管理器（dpkg→deb / rpm→rpm），
    // 禁止跨架构回退（如 arm64 机器装 x64 包）与跨发行版回退（如 CentOS 装 deb）
    let Some(arch_key) = arch_key_for(std::env::consts::ARCH) else {
        return Err(RpcError::new(
            "UNSUPPORTED_ARCH",
            format!("不支持的 CPU 架构: {}，请使用安装脚本手动更新", std::env::consts::ARCH),
        ));
    };

    let Some(preferred_format) = detect_preferred_format() else {
        return Err(RpcError::new(
            "UNSUPPORTED_PACKAGE_MANAGER",
            "当前系统未找到 dpkg 或 rpm，无法远程更新，请使用安装脚本手动更新",
        ));
    };

    let linux_packages = update_info
        .get("platforms")
        .and_then(|p| p.get("linux"))
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default();

    let (download_url, sha256) = select_package(&linux_packages, arch_key, preferred_format)
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
        "packageFormat": preferred_format,
        "packageAvailable": !download_url.is_empty(),
        "hasUpdate": has_update,
    }))
}

/// 执行更新（下载 deb + SHA-256 校验 + dpkg 安装 + 重启服务）
///
/// 通过 ctx.progress_tx 实时推送每个步骤的详细日志。
/// 下载使用 spawn_blocking + 分块读取，避免阻塞 tokio 运行时并支持实时进度。
/// 安装步骤通过 systemd-run 在沙箱外以 root 执行（安装脚本已配置 sudoers 免密规则）。
pub async fn perform_update(ctx: &CommandContext) -> CommandResult {
    info!("[update] 开始执行更新流程");

    // 全局互斥：自动更新通知与手动更新可能同时触发，
    // 已有更新任务在执行时直接拒绝，不排队等待（避免重复下载与包管理器锁争抢）
    let Ok(_update_guard) = UPDATE_MUTEX.try_lock() else {
        return Err(RpcError::new(
            "UPDATE_IN_PROGRESS",
            "已有更新任务正在执行，请等待其完成后再试",
        ));
    };

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
        return Err(RpcError::new(
            "NO_DOWNLOAD_URL",
            "未找到适合当前架构的安装包",
        ));
    }

    send_progress(
        ctx,
        10.0,
        &format!("发现新版本 v{}，准备下载...", latest_version),
    )
    .await;

    // 判断包格式（deb 或 rpm）
    let is_deb = download_url.ends_with(".deb");
    let is_rpm = download_url.ends_with(".rpm");
    let pkg_ext = if is_deb {
        "deb"
    } else if is_rpm {
        "rpm"
    } else {
        "deb"
    };

    // 当前用户是否为 root（root 不需要 sudo）
    #[cfg(unix)]
    let is_root = unsafe { libc::geteuid() } == 0;
    #[cfg(not(unix))]
    let is_root = true;

    // 2. 下载安装包（分块下载 + 实时进度推送 + SHA-256 计算）
    // 注意：服务启用 PrivateTmp 后，daemon 的 /tmp 对沙箱外进程不可见，
    // 必须下载到数据目录（ReadWritePaths 声明可写，systemd-run 启动的安装进程可见）
    let updates_dir = format!("{}/updates", ctx.data_dir.trim_end_matches('/'));
    let _ = std::fs::create_dir_all(&updates_dir);
    let pkg_path = format!("{}/{}_update.{}", updates_dir, APP_NAME, pkg_ext);
    let download_url_owned = download_url.to_string();
    let current_ver = get_current_version();
    let download_path = pkg_path.clone();
    let progress_tx = ctx.progress_tx.clone();
    let request_id = ctx.request_id.clone();

    send_progress(ctx, 15.0, &format!("正在下载 {} 安装包...", pkg_ext)).await;

    let download_result =
        tokio::task::spawn_blocking(move || -> Result<(usize, String), String> {
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
                        &format!(
                            "下载中... {:.1}/{:.1} MB ({:.0}%)",
                            size_mb,
                            total_mb,
                            (pct - 15.0) / 35.0 * 100.0
                        ),
                    );
                }
            }

            let sha256_hash = hasher.finalize();
            let sha256_hex = format!("{:x}", sha256_hash);

            Ok((downloaded as usize, sha256_hex))
        })
        .await
        .map_err(|e| RpcError::new("DOWNLOAD_FAILED", format!("下载任务异常: {}", e)))?;

    let (downloaded_bytes, actual_sha256) =
        download_result.map_err(|e| RpcError::new("DOWNLOAD_FAILED", e))?;

    let size_kb = downloaded_bytes / 1024;
    send_progress(ctx, 55.0, &format!("下载完成，大小: {} KB", size_kb)).await;
    info!(
        "[update] 安装包已下载: {} ({} bytes)",
        pkg_path, downloaded_bytes
    );

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

    // 4. 安装：
    //    优先使用安全更新助手（root 管理的固定入口，sudoers 仅放行该助手，
    //    由助手校验路径/文件名/SHA-256/包名/包架构并复制到 root 暂存区防 TOCTOU）；
    //    旧环境未部署助手时回退 dpkg/rpm 直装（建议重跑安装脚本启用安全模式）
    const SECURE_UPDATE_HELPER: &str = "/usr/lib/chmlfrp-toolbox-daemon/secure-update-helper.sh";

    if Path::new(SECURE_UPDATE_HELPER).exists() {
        send_progress(ctx, 70.0, "正在通过安全更新助手安装...").await;
        // 更新源提供了 sha256 时以源声明值为准（助手同时校验下载完整性与 TOCTOU），
        // 否则使用本地计算的哈希（仅防 TOCTOU）
        let sha_for_helper = if expected_sha256.is_empty() {
            actual_sha256.as_str()
        } else {
            expected_sha256
        };
        let helper_args: Vec<String> = vec![pkg_path.clone(), sha_for_helper.to_string()];
        let helper_output = build_escalated_cmd(SECURE_UPDATE_HELPER, &helper_args, is_root)
            .output()
            .map_err(|e| RpcError::new("INSTALL_FAILED", format!("执行安全更新助手失败: {}", e)))?;

        if !helper_output.status.success() {
            let stderr = String::from_utf8_lossy(&helper_output.stderr);
            warn!("[update] 安全更新助手安装失败: {}", stderr);
            let _ = std::fs::remove_file(&pkg_path);
            send_progress(ctx, 100.0, &format!("安装失败: {}", stderr.trim())).await;
            return Err(RpcError::new(
                "INSTALL_FAILED",
                format!("安全更新助手安装失败: {}", stderr.trim()),
            ));
        }
        info!("[update] 安全更新助手安装成功");
    } else {
        warn!(
            "[update] 未找到安全更新助手 {}，回退 dpkg/rpm 直装模式（建议重新运行安装脚本启用安全更新）",
            SECURE_UPDATE_HELPER
        );

        let install_cmd_desc = if is_deb { "dpkg -i" } else { "rpm -U --force" };
        send_progress(ctx, 70.0, &format!("正在安装 ({}...)...", install_cmd_desc)).await;

        let install_args: Vec<String> = if is_deb {
            vec!["-i".to_string(), pkg_path.clone()]
        } else {
            vec![
                "-U".to_string(),
                "--force".to_string(),
                pkg_path.clone(),
            ]
        };
        let mut install_cmd = build_escalated_cmd(
            if is_deb { "dpkg" } else { "rpm" },
            &install_args,
            is_root,
        );

        let output = install_cmd
            .output()
            .map_err(|e| RpcError::new("INSTALL_FAILED", format!("执行安装命令失败: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            warn!("[update] 安装失败: stderr={}, stdout={}", stderr, stdout);

            // deb 包尝试自动修复依赖；仍失败则降级为解包直接替换二进制（容器/受限环境）
            if is_deb {
                send_progress(ctx, 75.0, "安装失败，尝试修复依赖...").await;
                info!("[update] 尝试修复依赖...");

                let fix_args: Vec<String> = ["install", "-f", "-y"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let fix_output = build_escalated_cmd("apt-get", &fix_args, is_root).output();

                let fix_ok = matches!(&fix_output, Ok(fo) if fo.status.success());
                if fix_ok && Path::new("/usr/bin").join(APP_NAME).exists() {
                    // 依赖修复成功且二进制已就位
                } else {
                    send_progress(ctx, 78.0, "修复依赖失败，尝试降级安装（解包替换二进制）...").await;
                    info!("[update] 依赖修复失败，降级为手动解压安装...");
                    match manual_install_deb(&pkg_path, &updates_dir, is_root) {
                        Ok(()) => {
                            send_progress(ctx, 85.0, "降级安装成功").await;
                        }
                        Err(manual_err) => {
                            let _ = std::fs::remove_file(&pkg_path);
                            send_progress(ctx, 100.0, &format!("安装失败: {}", manual_err)).await;
                            return Err(RpcError::new(
                                "INSTALL_FAILED",
                                format!(
                                    "dpkg 安装失败（{}）且降级安装失败: {}",
                                    stderr.trim(),
                                    manual_err
                                ),
                            ));
                        }
                    }
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
    }

    // 清理临时文件
    let _ = std::fs::remove_file(&pkg_path);

    send_progress(ctx, 85.0, "安装完成").await;
    info!("[update] 安装完成，正在重启服务...");

    // 5. 重启服务（使用 sudo -n systemctl restart，避免 polkit 拦截）
    // 先尝试 daemon-reload 使新安装的 service 文件（如 ReadWritePaths 变更）生效；
    // 旧 sudoers 无 daemon-reload 规则时 sudo -n 失败，静默跳过，不影响重启
    send_progress(ctx, 90.0, "正在重启服务...").await;

    let restart_cmd = if is_root {
        format!(
            "sleep 1; systemctl daemon-reload 2>/dev/null; systemctl restart {}",
            APP_NAME
        )
    } else {
        format!(
            "sleep 1; sudo -n systemctl daemon-reload 2>/dev/null; sudo -n systemctl restart {}",
            APP_NAME
        )
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

/// dpkg 失败时的降级安装：dpkg-deb -x 解包 + install 直接替换二进制
///
/// 适用于容器/受限环境（/var/lib/dpkg 只读导致 dpkg -i 无法写数据库）。
/// 解包目录必须位于数据目录（updates_base）：PrivateTmp 沙箱使 daemon 的
/// /tmp 对沙箱外进程不可见，而复制二进制到 /usr/bin 需要 systemd-run
/// 在沙箱外以 root 执行（sudoers 已配置免密规则）。
fn manual_install_deb(pkg_path: &str, updates_base: &str, is_root: bool) -> Result<(), String> {
    let extract_dir = format!("{}/extract", updates_base);
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir).map_err(|e| format!("创建解压目录失败: {}", e))?;

    // 1. 解包（无需写 dpkg 数据库）
    let extract_out = std::process::Command::new("dpkg-deb")
        .args(["-x", pkg_path, &extract_dir])
        .output()
        .map_err(|e| format!("执行 dpkg-deb 失败: {}", e))?;
    if !extract_out.status.success() {
        return Err(format!(
            "dpkg-deb 解包失败: {}",
            String::from_utf8_lossy(&extract_out.stderr)
        ));
    }

    // 2. 复制二进制
    // 目标优先级：当前运行的二进制路径（保证与 systemd ExecStart 一致，重启后新版本生效）
    // → /usr/bin → /usr/local/bin
    let bin_src = format!("{}/usr/bin/{}", extract_dir, APP_NAME);
    if !Path::new(&bin_src).exists() {
        return Err(format!("deb 包中未找到二进制文件 usr/bin/{}", APP_NAME));
    }

    let mut dest_candidates: Vec<String> = Vec::new();
    if let Ok(exe) = std::fs::read_link("/proc/self/exe") {
        let exe_str = exe.to_string_lossy().to_string();
        if exe_str.ends_with(APP_NAME) {
            dest_candidates.push(exe_str);
        }
    }
    for dir in ["/usr/bin", "/usr/local/bin"] {
        let p = format!("{}/{}", dir, APP_NAME);
        if !dest_candidates.contains(&p) {
            dest_candidates.push(p);
        }
    }

    let mut last_err = String::new();
    for dest in &dest_candidates {
        let install_args: Vec<String> = [
            "-m".to_string(),
            "755".to_string(),
            bin_src.clone(),
            dest.clone(),
        ]
        .to_vec();
        match build_escalated_cmd("install", &install_args, is_root).output() {
            Ok(out) if out.status.success() => {
                info!("[update] 降级安装完成，二进制已替换到 {}", dest);
                let _ = std::fs::remove_dir_all(extract_dir);
                return Ok(());
            }
            Ok(out) => {
                last_err = format!(
                    "写入 {} 失败: {}",
                    dest,
                    String::from_utf8_lossy(&out.stderr)
                );
                warn!("[update] 降级安装 {}", last_err);
            }
            Err(e) => {
                last_err = format!("写入 {} 失败: {}", dest, e);
                warn!("[update] 降级安装 {}", last_err);
            }
        }
    }

    let _ = std::fs::remove_dir_all(&extract_dir);
    Err(format!(
        "降级安装失败：{}（sudoers 可能缺少 systemd-run install 规则，请重新运行安装脚本）",
        last_err
    ))
}

/// 比较版本号：返回 true 表示 v1 > v2
fn version_gt(v1: &str, v2: &str) -> bool {
    let parse =
        |s: &str| -> Vec<u32> { s.split('.').filter_map(|p| p.parse::<u32>().ok()).collect() };
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
        info!(
            "[update] 收到更新通知 v{}，自动更新已开启，开始执行更新...",
            version
        );
        if let Err(e) = perform_update(ctx).await {
            warn!("[update] 自动更新失败: {}", e.message);
        }
    } else {
        info!("[update] 收到更新通知 v{}，自动更新未开启，忽略", version);
    }
}

#[cfg(test)]
mod tests {
    use super::{arch_key_for, select_package};
    use serde_json::json;

    fn sample_packages() -> Vec<serde_json::Value> {
        vec![
            json!({ "format": "deb", "arch": "x64", "url": "https://x64.deb", "sha256": "hash-x64-deb" }),
            json!({ "format": "deb", "arch": "arm64", "url": "https://arm64.deb", "sha256": "hash-arm64-deb" }),
            json!({ "format": "rpm", "arch": "x64", "url": "https://x64.rpm", "sha256": "hash-x64-rpm" }),
            json!({ "format": "rpm", "arch": "arm64", "url": "https://arm64.rpm", "sha256": "hash-arm64-rpm" }),
        ]
    }

    #[test]
    fn maps_supported_architectures_only() {
        assert_eq!(arch_key_for("x86_64"), Some("x64"));
        assert_eq!(arch_key_for("aarch64"), Some("arm64"));
        // 未知架构不再回退 x64，避免下载不可执行包
        assert_eq!(arch_key_for("riscv64"), None);
        assert_eq!(arch_key_for("mips"), None);
    }

    #[test]
    fn selects_exact_arch_and_format_package() {
        let packages = sample_packages();
        assert_eq!(
            select_package(&packages, "x64", "deb").unwrap(),
            ("https://x64.deb".to_string(), "hash-x64-deb".to_string())
        );
        assert_eq!(
            select_package(&packages, "arm64", "rpm").unwrap(),
            ("https://arm64.rpm".to_string(), "hash-arm64-rpm".to_string())
        );
    }

    #[test]
    fn never_falls_back_across_arch_or_format() {
        let packages = sample_packages();
        // RPM 系发行版绝不能拿到 deb 包
        assert!(select_package(&packages, "x64", "rpm").is_some());
        // 只提供 deb 时，RPM 系应返回 None 而不是回退 deb
        let deb_only: Vec<serde_json::Value> = vec![
            json!({ "format": "deb", "arch": "x64", "url": "https://x64.deb", "sha256": "hash" }),
        ];
        assert!(select_package(&deb_only, "x64", "rpm").is_none());
        // 架构缺失时不能回退其他架构
        let arm_only: Vec<serde_json::Value> = vec![
            json!({ "format": "deb", "arch": "arm64", "url": "https://arm64.deb", "sha256": "hash" }),
        ];
        assert!(select_package(&arm_only, "x64", "deb").is_none());
    }

    #[test]
    fn skips_packages_missing_url_or_format() {
        let packages = vec![
            json!({ "format": "deb", "arch": "x64" }),
            json!({ "format": "rpm", "arch": "x64", "url": "", "sha256": "hash" }),
            json!({ "format": "deb", "arch": "x64", "url": "https://ok.deb", "sha256": "ok" }),
        ];
        assert_eq!(
            select_package(&packages, "x64", "deb").unwrap(),
            ("https://ok.deb".to_string(), "ok".to_string())
        );
    }
}

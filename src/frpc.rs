use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tracing::info;

const FRPC_INFO_URL: &str = "https://cf-v1.uapis.cn/download/frpc/frpc_info.json";

#[derive(Debug, Deserialize)]
struct FrpcInfoResponse {
    msg: String,
    state: String,
    code: u32,
    data: FrpcInfoData,
}

#[derive(Debug, Deserialize)]
struct FrpcInfoData {
    downloads: Vec<FrpcDownload>,
}

#[derive(Debug, Deserialize)]
struct FrpcDownload {
    hash: String,
    platform: String,
    link: String,
    size: u64,
}

fn select_download<'a>(downloads: &'a [FrpcDownload], platform: &str) -> Option<&'a FrpcDownload> {
    downloads.iter().find(|item| item.platform == platform)
}

fn linux_platform(arch: &str) -> Option<&'static str> {
    match arch {
        "x86" => Some("linux_386"),
        "x86_64" => Some("linux_amd64"),
        "arm" => Some("linux_arm"),
        "aarch64" => Some("linux_arm64"),
        _ => None,
    }
}

fn managed_frpc_path(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("bin").join("frpc")
}

fn existing_system_frpc() -> Option<PathBuf> {
    for candidate in ["/usr/local/bin/frpc", "/usr/bin/frpc", "/opt/frpc/frpc"] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }

    let output = std::process::Command::new("which")
        .arg("frpc")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    path.is_file().then_some(path)
}

pub async fn ensure_frpc(data_dir: &str) -> Result<PathBuf, String> {
    let managed_path = managed_frpc_path(data_dir);
    if managed_path
        .metadata()
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
    {
        set_executable(&managed_path)?;
        return Ok(managed_path);
    }
    if let Some(path) = existing_system_frpc() {
        return Ok(path);
    }

    let platform = linux_platform(std::env::consts::ARCH)
        .ok_or_else(|| format!("当前架构不支持自动下载 frpc: {}", std::env::consts::ARCH))?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(600))
        .user_agent(format!(
            "chmlfrp-toolbox-daemon/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .map_err(|e| format!("创建 frpc 下载客户端失败: {}", e))?;

    let response = client
        .get(FRPC_INFO_URL)
        .send()
        .await
        .map_err(|e| format!("获取 frpc 下载信息失败: {}", e))?;
    if !response.status().is_success() {
        return Err(format!(
            "获取 frpc 下载信息失败: HTTP {}",
            response.status()
        ));
    }
    let info: FrpcInfoResponse = response
        .json()
        .await
        .map_err(|e| format!("解析 frpc 下载信息失败: {}", e))?;
    if info.code != 200 || info.state != "success" {
        return Err(format!("frpc 下载接口返回错误: {}", info.msg));
    }
    let download = select_download(&info.data.downloads, platform)
        .ok_or_else(|| format!("未找到当前平台的 frpc 下载项: {}", platform))?;
    info!(
        "[frpc] 未找到本地 frpc，开始自动下载 {}（{} bytes）",
        platform, download.size
    );

    let parent = managed_path
        .parent()
        .ok_or_else(|| "无法确定 frpc 保存目录".to_string())?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| format!("创建 frpc 保存目录失败: {}", e))?;
    let temp_path = managed_path.with_extension("download");
    let download_result = download_to_temp(&client, download, &temp_path).await;
    if let Err(error) = download_result {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error);
    }

    set_executable(&temp_path)?;
    if managed_path.exists() {
        tokio::fs::remove_file(&managed_path)
            .await
            .map_err(|e| format!("清理旧 frpc 失败: {}", e))?;
    }
    tokio::fs::rename(&temp_path, &managed_path)
        .await
        .map_err(|e| format!("保存 frpc 失败: {}", e))?;
    info!("[frpc] 自动下载并校验完成: {}", managed_path.display());
    Ok(managed_path)
}

async fn download_to_temp(
    client: &reqwest::Client,
    download: &FrpcDownload,
    temp_path: &Path,
) -> Result<(), String> {
    let mut response = client
        .get(&download.link)
        .send()
        .await
        .map_err(|e| format!("下载 frpc 失败: {}", e))?;
    if !response.status().is_success() {
        return Err(format!("下载 frpc 失败: HTTP {}", response.status()));
    }

    let mut file = tokio::fs::File::create(temp_path)
        .await
        .map_err(|e| format!("创建 frpc 临时文件失败: {}", e))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("读取 frpc 下载数据失败: {}", e))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入 frpc 临时文件失败: {}", e))?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
    }
    file.flush()
        .await
        .map_err(|e| format!("刷新 frpc 临时文件失败: {}", e))?;

    if downloaded == 0 {
        return Err("下载 frpc 失败: 未收到数据".to_string());
    }
    if download.size > 0 && downloaded != download.size {
        return Err(format!(
            "frpc 文件大小校验失败: 期望 {}，实际 {}",
            download.size, downloaded
        ));
    }
    if !download.hash.is_empty() {
        let actual_hash = format!("{:x}", hasher.finalize());
        if !actual_hash.eq_ignore_ascii_case(&download.hash) {
            return Err(format!(
                "frpc SHA-256 校验失败: 期望 {}，实际 {}",
                download.hash, actual_hash
            ));
        }
    }
    Ok(())
}

fn set_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("设置 frpc 执行权限失败: {}", e))?;
    }
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_supported_linux_architectures() {
        assert_eq!(linux_platform("x86_64"), Some("linux_amd64"));
        assert_eq!(linux_platform("aarch64"), Some("linux_arm64"));
        assert_eq!(linux_platform("arm"), Some("linux_arm"));
        assert_eq!(linux_platform("mips"), None);
    }

    #[test]
    fn stores_frpc_under_daemon_data_directory() {
        assert_eq!(
            managed_frpc_path("/var/lib/chmlfrp-toolbox-daemon"),
            PathBuf::from("/var/lib/chmlfrp-toolbox-daemon/bin/frpc")
        );
    }

    #[test]
    fn selects_matching_platform_download() {
        let downloads = vec![
            FrpcDownload {
                hash: "arm-hash".to_string(),
                platform: "linux_arm64".to_string(),
                link: "https://example.com/arm64".to_string(),
                size: 10,
            },
            FrpcDownload {
                hash: "amd-hash".to_string(),
                platform: "linux_amd64".to_string(),
                link: "https://example.com/amd64".to_string(),
                size: 20,
            },
        ];
        let selected = select_download(&downloads, "linux_amd64").expect("应匹配下载项");
        assert_eq!(selected.link, "https://example.com/amd64");
        assert_eq!(selected.hash, "amd-hash");
        assert_eq!(selected.size, 20);
    }
}

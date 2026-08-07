//! 配置加载与管理
//!
//! 配置文件结构（TOML）：
//! ```toml
//! [server]
//! backend_url = "wss://api.cct.zdzz.top"
//! data_dir = "/var/lib/chmlfrp-toolbox-daemon"
//!
//! [[accounts]]
//! proxy_token = "xxx"
//! device_name = "西安服务器"
//! ```

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 后端 WebSocket 地址
    pub backend_url: String,
    /// 数据目录（device_id、SQLite 数据库）
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    /// 代理令牌（proxyToken）
    pub proxy_token: String,
    /// 设备显示名
    #[serde(default)]
    pub device_name: String,
}

fn default_data_dir() -> String {
    "/var/lib/chmlfrp-toolbox-daemon".to_string()
}

/// 加载配置文件
pub fn load_config(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        anyhow::bail!(
            "配置文件不存在: {}，请运行 `chmlfrp-toolbox-daemon init-config` 生成模板",
            path.display()
        );
    }
    let content = fs::read_to_string(path)?;
    let cfg: Config = toml::from_str(&content)?;

    if cfg.accounts.is_empty() {
        anyhow::bail!("配置文件中未配置任何账号，请在 [[accounts]] 添加 proxy_token");
    }

    // 校验 token 不为空
    for (i, acc) in cfg.accounts.iter().enumerate() {
        if acc.proxy_token.trim().is_empty() {
            anyhow::bail!("第 {} 个账号的 proxy_token 为空", i + 1);
        }
    }

    Ok(cfg)
}

/// 生成默认配置模板
pub fn generate_template(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let template = Config {
        server: ServerConfig {
            backend_url: "wss://api.cct.zdzz.top".to_string(),
            data_dir: default_data_dir(),
        },
        accounts: vec![AccountConfig {
            proxy_token: "在此填入你的_proxyToken".to_string(),
            device_name: "我的服务器".to_string(),
        }],
    };
    let toml_str = toml::to_string_pretty(&template)?;
    let header = "# ChmlFrp 工具箱 Daemon 配置文件\n\
                  # 请将 proxy_token 替换为你的实际代理令牌\n\
                  # 多租户：添加多个 [[accounts]] 即可支持多个账号\n\n";
    fs::write(path, format!("{}{}", header, toml_str))?;
    info!("配置模板已写入: {}", path.display());
    Ok(())
}

/// 读取或生成 device_id（持久化到 data_dir/device_id）
pub fn get_or_create_device_id(data_dir: &str) -> anyhow::Result<String> {
    let dir = Path::new(data_dir);
    fs::create_dir_all(dir)?;
    let id_file = dir.join("device_id");

    // 尝试读取已有 device_id
    if id_file.exists() {
        let id = fs::read_to_string(&id_file)?;
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Ok(id);
        }
    }

    // 生成新 device_id
    let id = uuid::Uuid::new_v4().to_string();
    fs::write(&id_file, &id)?;
    // 设置文件权限为 600（仅所有者可读写）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&id_file)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&id_file, perms)?;
    }
    info!("生成新 device_id: {}", id);
    Ok(id)
}

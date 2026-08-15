//! 配置加载与管理
//!
//! 配置文件结构（TOML）：
//! ```toml
//! [server]
//! backend_url = "wss://api.cct.zdzz.top"
//! data_dir = "/var/lib/chmlfrp-toolbox-daemon"
//!
//! [update]
//! auto_update = false
//!
//! [log]
//! retention_days = 7
//!
//! [[accounts]]
//! proxy_token = "xxx"
//! device_name = "西安服务器"
//! ```

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub update: UpdateConfig,
    #[serde(default)]
    pub log: LogConfig,
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

/// 更新配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// 是否启用自动更新（由后端推送触发）
    #[serde(default)]
    pub auto_update: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self { auto_update: false }
    }
}

/// 日志配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// 日志文件保留天数（超过自动删除，0 表示永不清理）
    #[serde(default = "default_retention_days")]
    pub retention_days: u64,
}

fn default_retention_days() -> u64 {
    7
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            retention_days: default_retention_days(),
        }
    }
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

/// 默认配置文件路径
pub fn default_config_path() -> PathBuf {
    PathBuf::from("/etc/chmlfrp-toolbox-daemon/config.toml")
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

/// 保存配置到文件（原子写入：先写临时文件再 rename）
pub fn save_config(path: &Path, cfg: &Config) -> anyhow::Result<()> {
    let toml_str = toml::to_string_pretty(cfg)?;
    let header = "# ChmlFrp 工具箱 Daemon 配置文件\n\
                  # 由远程管理自动生成/修改\n\n";
    let content = format!("{}{}", header, toml_str);

    // 原子写入：先写到临时文件，再 rename
    let tmp_path = path.with_extension("toml.tmp");
    fs::write(&tmp_path, &content)?;

    // 设置文件权限为 660（允许 daemon 用户组读写）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o660))?;
    }

    fs::rename(&tmp_path, path)?;
    info!("配置已保存: {}", path.display());
    Ok(())
}

/// 添加账号
pub fn add_account(path: &Path, token: String, name: String) -> anyhow::Result<()> {
    let mut cfg = load_config(path)?;
    // 检查 token 是否已存在
    if cfg.accounts.iter().any(|a| a.proxy_token == token) {
        anyhow::bail!("该 proxyToken 已存在");
    }
    cfg.accounts.push(AccountConfig {
        proxy_token: token,
        device_name: name,
    });
    save_config(path, &cfg)
}

/// 修改账号（按索引，1-based）
pub fn modify_account(
    path: &Path,
    index: usize,
    token: Option<String>,
    name: Option<String>,
) -> anyhow::Result<()> {
    let mut cfg = load_config(path)?;
    if index == 0 || index > cfg.accounts.len() {
        anyhow::bail!(
            "账号序号无效: {}（共 {} 个账号）",
            index,
            cfg.accounts.len()
        );
    }
    let acc = &mut cfg.accounts[index - 1];
    if let Some(t) = token {
        acc.proxy_token = t;
    }
    if let Some(n) = name {
        acc.device_name = n;
    }
    save_config(path, &cfg)
}

/// 删除账号（按索引，1-based）
pub fn delete_account(path: &Path, index: usize) -> anyhow::Result<()> {
    let mut cfg = load_config(path)?;
    if index == 0 || index > cfg.accounts.len() {
        anyhow::bail!(
            "账号序号无效: {}（共 {} 个账号）",
            index,
            cfg.accounts.len()
        );
    }
    cfg.accounts.remove(index - 1);
    save_config(path, &cfg)
}

/// 修改后端地址
pub fn set_backend_url(path: &Path, url: String) -> anyhow::Result<()> {
    let mut cfg = load_config(path)?;
    cfg.server.backend_url = url;
    save_config(path, &cfg)
}

/// 设置自动更新开关
pub fn set_auto_update(path: &Path, enabled: bool) -> anyhow::Result<()> {
    let mut cfg = load_config(path)?;
    cfg.update.auto_update = enabled;
    save_config(path, &cfg)
}

// ===== auto_update 独立存储（避免写入 /etc 只读文件系统） =====

/// auto_update override 文件路径（位于 data_dir 下，daemon 用户可写）
fn auto_update_override_path(data_dir: &str) -> PathBuf {
    PathBuf::from(data_dir).join("update_settings.json")
}

/// 读取 auto_update override（data_dir/update_settings.json）
///
/// 返回 Some(bool) 表示 override 文件存在且已设置；None 表示未设置，应 fallback 到主配置。
pub fn load_auto_update_override(data_dir: &str) -> Option<bool> {
    let path = auto_update_override_path(data_dir);
    if !path.exists() {
        return None;
    }
    let content = fs::read_to_string(&path).ok()?;
    #[derive(Deserialize)]
    struct OverrideFile {
        auto_update: bool,
    }
    let parsed: OverrideFile = serde_json::from_str(&content).ok()?;
    Some(parsed.auto_update)
}

/// 保存 auto_update override 到 data_dir/update_settings.json
///
/// data_dir 通常是 /var/lib/chmlfrp-toolbox-daemon/，daemon 用户有写入权限，
/// 避免直接修改 /etc/chmlfrp-toolbox-daemon/config.toml 导致 Read-only file system 错误。
pub fn save_auto_update_override(data_dir: &str, enabled: bool) -> anyhow::Result<()> {
    let dir = Path::new(data_dir);
    fs::create_dir_all(dir)?;

    let path = auto_update_override_path(data_dir);
    let content = serde_json::json!({ "auto_update": enabled }).to_string();
    fs::write(&path, content)?;

    // 设置文件权限为 600（仅 daemon 用户可读写）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }

    info!(
        "auto_update override 已保存: {} = {}",
        path.display(),
        enabled
    );
    Ok(())
}

/// 获取 auto_update 最终值：优先读 data_dir override，fallback 到主配置
pub fn get_effective_auto_update(config_path: &Path, data_dir: &str) -> bool {
    if let Some(val) = load_auto_update_override(data_dir) {
        return val;
    }
    // fallback 到主配置（可能不存在或读取失败，默认 false）
    load_config(config_path)
        .map(|cfg| cfg.update.auto_update)
        .unwrap_or(false)
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
        update: UpdateConfig::default(),
        log: LogConfig::default(),
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

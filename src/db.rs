//! SQLite 多租户存储
//!
//! 按 user_id 分库：`<data_dir>/users/<user_id>.db`
//! 当前阶段表结构最小化，后续功能扩展时再加表。
//!
//! delete_my_data 删除整个 `<user_id>.db` 文件。

use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// 初始化数据目录
pub fn init_db_dir(data_dir: &str) -> anyhow::Result<()> {
    let users_dir = Path::new(data_dir).join("users");
    fs::create_dir_all(&users_dir)?;
    Ok(())
}

/// 获取指定 user_id 的数据库路径
pub fn user_db_path(data_dir: &str, user_id: i64) -> PathBuf {
    Path::new(data_dir)
        .join("users")
        .join(format!("{}.db", user_id))
}

/// 初始化用户数据库（创建表）
pub fn init_user_db(data_dir: &str, user_id: i64) -> anyhow::Result<()> {
    let db_path = user_db_path(data_dir, user_id);
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = rusqlite::Connection::open(&db_path)?;

    // 设备元信息表（当前阶段最小化）
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS device_meta (
            key   TEXT PRIMARY KEY,
            value TEXT
        );
        CREATE TABLE IF NOT EXISTS test_logs (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            test_type TEXT NOT NULL,
            target    TEXT,
            result    TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );",
    )?;

    // 设置文件权限 600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&db_path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&db_path, perms)?;
    }

    info!("[db] 用户 {} 数据库已初始化", user_id);
    Ok(())
}

/// 删除指定 user_id 的所有数据（删除整个数据库文件）
pub fn delete_user_data(data_dir: &str, user_id: i64) -> anyhow::Result<()> {
    let db_path = user_db_path(data_dir, user_id);
    if db_path.exists() {
        fs::remove_file(&db_path)?;
        info!("[db] 用户 {} 数据已删除", user_id);
    } else {
        warn!("[db] 用户 {} 无数据可删除", user_id);
    }
    Ok(())
}

/// 记录测试日志（可选）
pub fn log_test(
    data_dir: &str,
    user_id: i64,
    test_type: &str,
    target: &str,
    result: &str,
) -> anyhow::Result<()> {
    let db_path = user_db_path(data_dir, user_id);
    if !db_path.exists() {
        init_user_db(data_dir, user_id)?;
    }
    let conn = rusqlite::Connection::open(&db_path)?;
    conn.execute(
        "INSERT INTO test_logs (test_type, target, result) VALUES (?1, ?2, ?3)",
        rusqlite::params![test_type, target, result],
    )?;
    Ok(())
}

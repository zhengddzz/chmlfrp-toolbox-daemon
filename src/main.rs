//! ChmlFrp 社区工具箱 Daemon - 服务器端远程管理守护进程
//!
//! 通过 WebSocket 连接后端中继，被桌面客户端远程执行延迟/带宽测试。
//! 支持多租户：一个 Daemon 可同时被多个 qzhua 账号绑定。

mod commands;
mod config;
mod db;
mod frpc;
mod logs;
mod relay;
mod telemetry;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;

/// ChmlFrp 社区工具箱 Daemon
#[derive(Parser)]
#[command(name = "chmlfrp-toolbox-daemon", version, about)]
struct Cli {
    /// 配置文件路径
    #[arg(
        short,
        long,
        global = true,
        default_value = "/etc/chmlfrp-toolbox-daemon/config.toml"
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 前台运行（前台日志输出，适合调试和 systemd 服务）
    Start,
    /// 查看连接状态
    Status,
    /// 生成默认配置文件模板
    InitConfig {
        /// 输出路径
        #[arg(default_value = "/etc/chmlfrp-toolbox-daemon/config.toml")]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Start => {
            let cfg = config::load_config(&cli.config)?;

            // 初始化日志：stdout + 按天滚动文件（data_dir/logs/daemon.log.*）
            // guard 必须保活到进程退出，drop 后文件日志停止写入
            let _log_guard = logs::init_tracing(Some(&cfg.server.data_dir));

            info!("ChmlFrp 工具箱 Daemon 启动中...");
            info!("配置文件: {}", cli.config.display());
            info!("后端地址: {}", cfg.server.backend_url);
            info!("账号数量: {}", cfg.accounts.len());

            // 初始化数据库目录
            db::init_db_dir(&cfg.server.data_dir)?;

            // 日志自动清理：删除超过保留天数的日志文件（每天执行一次）
            logs::start_cleanup_task(cfg.server.data_dir.clone(), cfg.log.retention_days);

            // 启动 relay 客户端（多租户，每个 token 一个连接）
            let config_path = cli.config.to_string_lossy().to_string();
            relay::run_multi_tenant(cfg, config_path).await?;
        }
        Command::Status => {
            // 无 data_dir 可用，仅输出到 stdout
            let _log_guard = logs::init_tracing(None);
            let cfg = config::load_config(&cli.config)?;
            info!("配置文件: {}", cli.config.display());
            info!("后端地址: {}", cfg.server.backend_url);
            info!("配置账号数: {}", cfg.accounts.len());
            // TODO: 通过本地 socket 查询运行状态
            println!("Daemon 状态查询功能待实现（需通过本地管理 socket）");
        }
        Command::InitConfig { output } => {
            let _log_guard = logs::init_tracing(None);
            config::generate_template(&output)?;
            info!("配置模板已生成: {}", output.display());
            println!("请编辑 {} 添加你的账号 token", output.display());
        }
    }

    Ok(())
}

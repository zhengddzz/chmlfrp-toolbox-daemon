//! Daemon 配置与账号管理 RPC 命令
//!
//! 提供远程读取/修改 Daemon 配置文件的能力：
//! - get_config: 获取当前配置（后端地址、账号列表、自动更新开关）
//! - add_account: 添加账号
//! - modify_account: 修改账号（token / 名称）
//! - delete_account: 删除账号
//! - set_backend_url: 修改后端地址

use crate::commands::{CommandContext, CommandResult, RpcError};
use crate::config;
use serde::Deserialize;
use std::path::Path;

/// 获取当前配置
pub async fn get_config(ctx: &CommandContext) -> CommandResult {
    let path = Path::new(&ctx.config_path);
    let cfg = config::load_config(path)
        .map_err(|e| RpcError::new("CONFIG_LOAD_FAILED", e.to_string()))?;

    // 返回时隐藏完整 token，只返回前 8 位 + ...
    let accounts: Vec<serde_json::Value> = cfg.accounts
        .iter()
        .enumerate()
        .map(|(i, acc)| {
            let token_preview = if acc.proxy_token.len() > 8 {
                format!("{}...", &acc.proxy_token[..8])
            } else {
                acc.proxy_token.clone()
            };
            serde_json::json!({
                "index": i + 1,
                "deviceName": acc.device_name,
                "tokenPreview": token_preview,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "backendUrl": cfg.server.backend_url,
        "dataDir": cfg.server.data_dir,
        "autoUpdate": cfg.update.auto_update,
        "accounts": accounts,
    }))
}

/// 添加账号
#[derive(Deserialize)]
pub struct AddAccountParams {
    #[serde(rename = "proxyToken")]
    pub proxy_token: String,
    #[serde(rename = "deviceName")]
    pub device_name: String,
}

pub async fn add_account(params: &serde_json::Value, ctx: &CommandContext) -> CommandResult {
    let p: AddAccountParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new("INVALID_PARAMS", e.to_string()))?;

    if p.proxy_token.trim().is_empty() {
        return Err(RpcError::new("INVALID_PARAMS", "proxyToken 不能为空"));
    }

    let path = Path::new(&ctx.config_path);
    config::add_account(path, p.proxy_token, p.device_name)
        .map_err(|e| RpcError::new("CONFIG_SAVE_FAILED", e.to_string()))?;

    Ok(serde_json::json!({ "success": true, "message": "账号已添加，重启服务后生效" }))
}

/// 修改账号
#[derive(Deserialize)]
pub struct ModifyAccountParams {
    pub index: usize,
    #[serde(rename = "proxyToken")]
    pub proxy_token: Option<String>,
    #[serde(rename = "deviceName")]
    pub device_name: Option<String>,
}

pub async fn modify_account(params: &serde_json::Value, ctx: &CommandContext) -> CommandResult {
    let p: ModifyAccountParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new("INVALID_PARAMS", e.to_string()))?;

    let path = Path::new(&ctx.config_path);
    config::modify_account(path, p.index, p.proxy_token, p.device_name)
        .map_err(|e| RpcError::new("CONFIG_SAVE_FAILED", e.to_string()))?;

    Ok(serde_json::json!({ "success": true, "message": "账号已修改，重启服务后生效" }))
}

/// 删除账号
#[derive(Deserialize)]
pub struct DeleteAccountParams {
    pub index: usize,
}

pub async fn delete_account(params: &serde_json::Value, ctx: &CommandContext) -> CommandResult {
    let p: DeleteAccountParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new("INVALID_PARAMS", e.to_string()))?;

    let path = Path::new(&ctx.config_path);
    config::delete_account(path, p.index)
        .map_err(|e| RpcError::new("CONFIG_SAVE_FAILED", e.to_string()))?;

    Ok(serde_json::json!({ "success": true, "message": "账号已删除，重启服务后生效" }))
}

/// 修改后端地址
#[derive(Deserialize)]
pub struct SetBackendUrlParams {
    #[serde(rename = "backendUrl")]
    pub backend_url: String,
}

pub async fn set_backend_url(params: &serde_json::Value, ctx: &CommandContext) -> CommandResult {
    let p: SetBackendUrlParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new("INVALID_PARAMS", e.to_string()))?;

    if p.backend_url.trim().is_empty() {
        return Err(RpcError::new("INVALID_PARAMS", "backendUrl 不能为空"));
    }

    let path = Path::new(&ctx.config_path);
    config::set_backend_url(path, p.backend_url)
        .map_err(|e| RpcError::new("CONFIG_SAVE_FAILED", e.to_string()))?;

    Ok(serde_json::json!({ "success": true, "message": "后端地址已修改，重启服务后生效" }))
}

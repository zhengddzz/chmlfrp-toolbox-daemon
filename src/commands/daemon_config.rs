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
    let accounts: Vec<serde_json::Value> = cfg
        .accounts
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

/// 远程重新授权：用新的 proxy_token 替换当前连接账号的令牌
///
/// 流程（桌面客户端「重新授权」按钮触发）：
/// 1. 用新 token 调后端 /auth/refresh 校验有效性
/// 2. 按 ctx.proxy_token（旧 token）定位配置条目并替换
/// 3. 清空 accessToken 缓存，返回成功
/// 4. relay 层收到成功响应后断开连接，用新 token 自动重连
#[derive(Deserialize)]
pub struct UpdateProxyTokenParams {
    #[serde(rename = "proxyToken")]
    pub proxy_token: String,
}

pub async fn update_proxy_token(params: &serde_json::Value, ctx: &CommandContext) -> CommandResult {
    let p: UpdateProxyTokenParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new("INVALID_PARAMS", e.to_string()))?;

    let new_token = p.proxy_token.trim().to_string();
    if new_token.is_empty() {
        return Err(RpcError::new("INVALID_PARAMS", "proxyToken 不能为空"));
    }
    if new_token == ctx.proxy_token {
        return Err(RpcError::new(
            "TOKEN_UNCHANGED",
            "新令牌与当前令牌相同，无需更新",
        ));
    }

    // 1. 校验新 token 有效性（调后端 /auth/refresh）
    if let Err(e) = super::auth::validate_proxy_token(&ctx.backend_url, &new_token).await {
        return Err(RpcError::new(&e.code, format!("新令牌校验失败: {}", e.message)));
    }

    // 2. 替换配置中的旧 token（account_id 为 0-based 索引字符串）
    let fallback_index = ctx
        .account_id
        .parse::<usize>()
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let path = Path::new(&ctx.config_path);
    config::replace_account_token(path, &ctx.proxy_token, &new_token, fallback_index)
        .map_err(|e| RpcError::new("CONFIG_SAVE_FAILED", e.to_string()))?;

    // 3. 清空新旧 token 的 accessToken 缓存
    super::auth::invalidate_cache(&ctx.proxy_token).await;
    super::auth::invalidate_cache(&new_token).await;

    tracing::info!("[update_proxy_token] 令牌已热更新，即将使用新令牌重连");

    Ok(serde_json::json!({
        "success": true,
        "message": "令牌已更新，正在使用新令牌重连"
    }))
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

//! accessToken 刷新模块（与桌面客户端同款流程）
//!
//! chmlfrp API（cf-v2.uapis.cn）只认 qzhua accessToken（约 30 分钟有效期），
//! daemon 配置中持有的是后端 proxy_token（7 天有效期），不能直接调 chmlfrp API。
//! 本模块用 proxy_token 调后端 `POST /auth/refresh` 换取 accessToken：
//!
//! - 内存缓存（按 proxy_token 哈希区分账号），剩余有效期 > 60s 直接复用
//! - 并发单飞：同一 token 的并发刷新复用同一次请求（后端限流 5 次/分钟）
//! - 错误分类：
//!   - PROXY_TOKEN_INVALID / REFRESH_TOKEN_EXPIRED：令牌已过期，需重新授权
//!   - RATE_LIMITED：触发限流，稍后重试
//!   - REFRESH_FAILED / 网络错误：临时故障

use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// accessToken 提前刷新阈值（剩余有效期低于此值即刷新，与桌面端/后端缓存窗口一致）
const REFRESH_AHEAD: Duration = Duration::from_secs(60);
/// HTTP 超时
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// 刷新错误（携带后端错误码，便于上层区分「需重新授权」vs「临时故障」）
#[derive(Debug)]
pub struct RefreshError {
    pub code: String,
    pub message: String,
}

impl RefreshError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// 是否需要重新授权（proxy_token 已失效）
    pub fn is_token_expired(&self) -> bool {
        self.code == "PROXY_TOKEN_INVALID" || self.code == "REFRESH_TOKEN_EXPIRED"
    }

    /// 转为用户可读提示
    pub fn user_message(&self) -> String {
        if self.is_token_expired() {
            return format!(
                "{}（请在设备管理-远程管理中重新授权，更新 daemon 令牌）",
                self.message
            );
        }
        if self.code == "RATE_LIMITED" {
            return format!("{}（触发限流，请稍后重试）", self.message);
        }
        self.message.clone()
    }
}

/// 缓存条目
struct CachedToken {
    access_token: String,
    /// 过期时间点
    expires_at: Instant,
}

impl CachedToken {
    fn remaining(&self) -> Duration {
        self.expires_at.saturating_duration_since(Instant::now())
    }

    fn is_usable(&self) -> bool {
        self.remaining() > REFRESH_AHEAD
    }
}

/// /auth/refresh 成功响应
#[derive(Debug, serde::Deserialize)]
struct RefreshResponse {
    success: bool,
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "expiresIn")]
    expires_in: Option<u64>,
}

/// /auth/refresh 失败响应
#[derive(Debug, serde::Deserialize)]
struct RefreshErrorResponse {
    code: Option<String>,
    message: Option<String>,
}

/// 全局缓存（token_hash -> 缓存条目）+ 单飞锁
struct TokenCache {
    entries: HashMap<String, CachedToken>,
}

static TOKEN_CACHE: Lazy<Arc<Mutex<TokenCache>>> =
    Lazy::new(|| Arc::new(Mutex::new(TokenCache { entries: HashMap::new() })));

fn token_key(proxy_token: &str) -> String {
    // 与 telemetry.rs 一致：以 sha256 摘要做 key，不在内存 map 中散落明文
    format!("{:x}", Sha256::digest(proxy_token.as_bytes()))
}

/// 获取可用的 accessToken（缓存优先，过期自动刷新）
///
/// # Errors
/// 刷新失败时返回 [`RefreshError`]，调用方用 `user_message()` 展示。
pub async fn ensure_access_token(
    backend_url: &str,
    proxy_token: &str,
) -> Result<String, RefreshError> {
    let key = token_key(proxy_token);

    // 1. 缓存命中
    {
        let cache = TOKEN_CACHE.lock().await;
        if let Some(entry) = cache.entries.get(&key) {
            if entry.is_usable() {
                return Ok(entry.access_token.clone());
            }
        }
    }

    // 2. 单飞刷新（锁粒度为全局，避免同 token 并发重复请求触发后端限流）
    let mut cache = TOKEN_CACHE.lock().await;
    // 双重检查：等锁期间可能已被其他请求刷新
    if let Some(entry) = cache.entries.get(&key) {
        if entry.is_usable() {
            return Ok(entry.access_token.clone());
        }
    }

    let (access_token, ttl_secs) = refresh_via_backend(backend_url, proxy_token).await?;

    cache.entries.insert(
        key,
        CachedToken {
            access_token: access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl_secs),
        },
    );
    Ok(access_token)
}

/// 调后端 /auth/refresh 换 accessToken，返回 (accessToken, 有效期秒数)
async fn refresh_via_backend(
    backend_url: &str,
    proxy_token: &str,
) -> Result<(String, u64), RefreshError> {
    let url = format!(
        "{}/auth/refresh",
        backend_url.trim_end_matches('/')
    );

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| RefreshError::new("REFRESH_FAILED", format!("创建 HTTP 客户端失败: {}", e)))?;

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", proxy_token))
        .send()
        .await
        .map_err(|e| RefreshError::new("REFRESH_FAILED", format!("刷新令牌请求失败: {}", e)))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| RefreshError::new("REFRESH_FAILED", format!("读取刷新响应失败: {}", e)))?;

    if !status.is_success() {
        let err: RefreshErrorResponse = serde_json::from_str(&body).unwrap_or(RefreshErrorResponse {
            code: None,
            message: None,
        });
        let code = err.code.unwrap_or_else(|| "UNKNOWN".to_string());
        let message = err
            .message
            .unwrap_or_else(|| format!("刷新令牌失败 (HTTP {})", status));
        return Err(RefreshError::new(&code, message));
    }

    let data: RefreshResponse = serde_json::from_str(&body)
        .map_err(|e| RefreshError::new("REFRESH_FAILED", format!("解析刷新响应失败: {}", e)))?;

    if !data.success || data.access_token.is_empty() {
        return Err(RefreshError::new("REFRESH_FAILED", "刷新令牌返回异常"));
    }

    // expiresIn 缺省按 25 分钟估（qzhua access_token 约 30 分钟，保守取整）
    let ttl = data.expires_in.unwrap_or(25 * 60).clamp(60, 3600);
    Ok((data.access_token, ttl))
}

/// 校验 proxy_token 是否有效（用于 update_proxy_token 命令，不写缓存）
pub async fn validate_proxy_token(
    backend_url: &str,
    proxy_token: &str,
) -> Result<(), RefreshError> {
    refresh_via_backend(backend_url, proxy_token).await.map(|_| ())
}

/// 清除指定 proxy_token 的 accessToken 缓存（令牌被更新/删除时调用）
pub async fn invalidate_cache(proxy_token: &str) {
    let mut cache = TOKEN_CACHE.lock().await;
    cache.entries.remove(&token_key(proxy_token));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_token_usable_before_threshold() {
        let token = CachedToken {
            access_token: "at".to_string(),
            expires_at: Instant::now() + Duration::from_secs(120),
        };
        assert!(token.is_usable());
        assert!(token.remaining() > REFRESH_AHEAD);
    }

    #[test]
    fn cached_token_expired_within_threshold() {
        // 剩余 30s < 60s 阈值：应触发刷新
        let token = CachedToken {
            access_token: "at".to_string(),
            expires_at: Instant::now() + Duration::from_secs(30),
        };
        assert!(!token.is_usable());
    }

    #[test]
    fn cached_token_fully_expired() {
        let token = CachedToken {
            access_token: "at".to_string(),
            expires_at: Instant::now() - Duration::from_secs(1),
        };
        assert!(!token.is_usable());
        assert_eq!(token.remaining(), Duration::ZERO);
    }

    #[test]
    fn token_key_is_hashed_and_stable() {
        let a = token_key("secret-token");
        let b = token_key("secret-token");
        assert_eq!(a, b);
        assert_ne!(a, "secret-token");
        assert_ne!(a, token_key("other-token"));
    }

    #[test]
    fn token_expired_error_detection() {
        let expired = RefreshError::new("PROXY_TOKEN_INVALID", "代理令牌已过期");
        assert!(expired.is_token_expired());
        let expired2 = RefreshError::new("REFRESH_TOKEN_EXPIRED", "刷新令牌已过期");
        assert!(expired2.is_token_expired());
        let limited = RefreshError::new("RATE_LIMITED", "请求过于频繁");
        assert!(!limited.is_token_expired());
        assert!(limited.user_message().contains("稍后重试"));
    }
}

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::warn;

const BATCH_SIZE: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEvent {
    pub event_id: String,
    pub event_type: String,
    pub event_version: u32,
    pub event_data: Value,
    pub app_version: String,
    pub platform: String,
    pub session_id: String,
    pub client_time: String,
}

impl UsageEvent {
    pub fn new(event_type: &str, event_data: Value, session_id: &str) -> Self {
        Self {
            event_id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            event_version: 1,
            event_data,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            platform: std::env::consts::OS.to_string(),
            session_id: session_id.to_string(),
            client_time: chrono::Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReportResponse {
    success: bool,
}

pub fn account_queue_path(data_dir: &str, proxy_token: &str) -> PathBuf {
    let digest = Sha256::digest(proxy_token.as_bytes());
    Path::new(data_dir)
        .join("telemetry")
        .join(format!("{:x}.db", digest))
}

pub fn init_queue(data_dir: &str, proxy_token: &str) -> anyhow::Result<()> {
    let path = account_queue_path(data_dir, proxy_token);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_queue (
            event_id TEXT PRIMARY KEY,
            payload TEXT NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );",
    )?;
    Ok(())
}

pub fn enqueue(data_dir: &str, proxy_token: &str, event: &UsageEvent) -> anyhow::Result<()> {
    init_queue(data_dir, proxy_token)?;
    let conn = Connection::open(account_queue_path(data_dir, proxy_token))?;
    conn.execute(
        "INSERT OR IGNORE INTO usage_queue (event_id, payload) VALUES (?1, ?2)",
        params![event.event_id, serde_json::to_string(event)?],
    )?;
    Ok(())
}

fn load_batch(data_dir: &str, proxy_token: &str) -> anyhow::Result<Vec<UsageEvent>> {
    init_queue(data_dir, proxy_token)?;
    let conn = Connection::open(account_queue_path(data_dir, proxy_token))?;
    let mut stmt = conn.prepare("SELECT payload FROM usage_queue ORDER BY created_at LIMIT ?1")?;
    let rows = stmt.query_map([BATCH_SIZE as i64], |row| row.get::<_, String>(0))?;
    let mut events = Vec::new();
    for row in rows {
        events.push(serde_json::from_str(&row?)?);
    }
    Ok(events)
}

fn remove_batch(data_dir: &str, proxy_token: &str, events: &[UsageEvent]) -> anyhow::Result<()> {
    let mut conn = Connection::open(account_queue_path(data_dir, proxy_token))?;
    let tx = conn.transaction()?;
    for event in events {
        tx.execute(
            "DELETE FROM usage_queue WHERE event_id = ?1",
            [&event.event_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub async fn run_reporter(backend_url: String, data_dir: String, proxy_token: String) {
    let endpoint = format!(
        "{}/api/usage/daemon-report",
        backend_url
            .trim_end_matches('/')
            .replacen("wss://", "https://", 1)
            .replacen("ws://", "http://", 1)
    );
    let client = reqwest::Client::new();
    loop {
        match load_batch(&data_dir, &proxy_token) {
            Ok(events) if !events.is_empty() => {
                let result = client
                    .post(&endpoint)
                    .bearer_auth(&proxy_token)
                    .json(&serde_json::json!({ "events": events }))
                    .send()
                    .await;
                match result {
                    Ok(response) if response.status().is_success() => {
                        let accepted = response
                            .json::<ReportResponse>()
                            .await
                            .map(|r| r.success)
                            .unwrap_or(false);
                        if accepted {
                            if let Err(err) = remove_batch(&data_dir, &proxy_token, &events) {
                                warn!("[telemetry] 清理已上报事件失败: {}", err);
                            }
                        }
                    }
                    Ok(response) => warn!("[telemetry] 上报失败: HTTP {}", response.status()),
                    Err(err) => warn!("[telemetry] 上报失败: {}", err),
                }
            }
            Ok(_) => {}
            Err(err) => warn!("[telemetry] 读取队列失败: {}", err),
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_queue_path_does_not_contain_token() {
        let path = account_queue_path("data", "secret-token");
        assert!(!path.to_string_lossy().contains("secret-token"));
    }
}

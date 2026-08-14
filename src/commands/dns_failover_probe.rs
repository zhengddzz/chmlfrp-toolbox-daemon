use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbeParams {
    execution_id: String,
    methods: Vec<String>,
    tunnels: Vec<ProbeTunnel>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbeTunnel {
    id: String,
    node_host: String,
    node_port: u16,
    tunnel_state: Option<bool>,
    node_state: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MethodResult {
    passed: bool,
    duration_ms: u64,
    error_code: Option<String>,
}

fn default_timeout_ms() -> u64 {
    15_000
}

fn parse_params(params: &serde_json::Value) -> Result<ProbeParams, super::RpcError> {
    let parsed: ProbeParams = serde_json::from_value(params.clone())
        .map_err(|e| super::RpcError::new("INVALID_PARAMS", format!("参数解析失败: {}", e)))?;
    if parsed.execution_id.trim().is_empty() {
        return Err(super::RpcError::new(
            "INVALID_PARAMS",
            "executionId 不能为空",
        ));
    }
    Ok(parsed)
}

fn state_result(value: Option<bool>, duration_ms: u64) -> MethodResult {
    MethodResult {
        passed: value.unwrap_or(false),
        duration_ms,
        error_code: if value.is_some() {
            None
        } else {
            Some("UNAVAILABLE".to_string())
        },
    }
}

pub async fn handle(params: &serde_json::Value) -> super::CommandResult {
    let request = parse_params(params)?;
    let mut results = Vec::with_capacity(request.tunnels.len());
    for tunnel in request.tunnels {
        let mut methods = HashMap::new();
        for method in &request.methods {
            let started_at = Instant::now();
            let result = match method.as_str() {
                "tunnel_state" => {
                    state_result(tunnel.tunnel_state, started_at.elapsed().as_millis() as u64)
                }
                "node_state" => {
                    state_result(tunnel.node_state, started_at.elapsed().as_millis() as u64)
                }
                "tcping" => {
                    let tcping_params = serde_json::json!({
                        "host": tunnel.node_host,
                        "port": tunnel.node_port,
                        "count": 1,
                        "timeoutSecs": std::cmp::max(1, request.timeout_ms.div_ceil(1000)),
                    });
                    match super::tcping::handle(&tcping_params).await {
                        Ok(value) => {
                            let loss = value.get("loss").and_then(|v| v.as_u64()).unwrap_or(1);
                            MethodResult {
                                passed: loss == 0,
                                duration_ms: started_at.elapsed().as_millis() as u64,
                                error_code: if loss == 0 {
                                    None
                                } else {
                                    Some("TIMEOUT".to_string())
                                },
                            }
                        }
                        Err(_) => MethodResult {
                            passed: false,
                            duration_ms: started_at.elapsed().as_millis() as u64,
                            error_code: Some("EXEC_FAILED".to_string()),
                        },
                    }
                }
                _ => MethodResult {
                    passed: false,
                    duration_ms: 0,
                    error_code: Some("UNSUPPORTED_METHOD".to_string()),
                },
            };
            methods.insert(method.clone(), result);
        }
        results.push(serde_json::json!({ "tunnelId": tunnel.id, "methods": methods }));
    }
    Ok(serde_json::json!({ "executionId": request.execution_id, "results": results }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_execution_id() {
        let params = serde_json::json!({
            "executionId": "",
            "methods": ["tcping"],
            "tunnels": []
        });
        assert!(parse_params(&params).is_err());
    }

    #[test]
    fn missing_remote_state_is_unavailable() {
        let result = state_result(None, 1);
        assert!(!result.passed);
        assert_eq!(result.error_code.as_deref(), Some("UNAVAILABLE"));
    }

    #[test]
    fn supplied_remote_state_is_preserved() {
        let result = state_result(Some(true), 1);
        assert!(result.passed);
        assert_eq!(result.error_code, None);
    }
}

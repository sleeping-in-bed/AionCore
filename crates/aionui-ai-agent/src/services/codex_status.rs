use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aionui_api_types::{CodexRateLimitWindow, CodexRateLimitsSnapshot, CodexStatusResponse};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock};

use crate::error::AgentError;
use crate::registry::AgentRegistry;

const CACHE_TTL: Duration = Duration::from_secs(30);
const QUERY_TIMEOUT: Duration = Duration::from_secs(8);

struct CachedStatus {
    fetched_at: Instant,
    status: CodexStatusResponse,
}

pub struct CodexStatusService {
    registry: Arc<AgentRegistry>,
    data_dir: PathBuf,
    cache: RwLock<Option<CachedStatus>>,
    refresh_lock: Mutex<()>,
}

impl CodexStatusService {
    pub fn new(registry: Arc<AgentRegistry>, data_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            registry,
            data_dir,
            cache: RwLock::new(None),
            refresh_lock: Mutex::new(()),
        })
    }

    pub async fn get_status(&self) -> Result<CodexStatusResponse, AgentError> {
        if let Some(status) = self.read_cached().await {
            return Ok(status);
        }

        let _guard = self.refresh_lock.lock().await;
        if let Some(status) = self.read_cached().await {
            return Ok(status);
        }

        let status = self.query_status_uncached().await;
        *self.cache.write().await = Some(CachedStatus {
            fetched_at: Instant::now(),
            status: status.clone(),
        });
        Ok(status)
    }

    async fn read_cached(&self) -> Option<CodexStatusResponse> {
        let guard = self.cache.read().await;
        guard
            .as_ref()
            .filter(|cached| cached.fetched_at.elapsed() <= CACHE_TTL)
            .map(|cached| cached.status.clone())
    }

    async fn query_status_uncached(&self) -> CodexStatusResponse {
        let command = match self.resolve_codex_command().await {
            Ok(command) => command,
            Err(error) => {
                return unavailable_status(format!("Codex CLI is unavailable: {}", error));
            }
        };

        match tokio::time::timeout(QUERY_TIMEOUT, self.query_with_command(&command)).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => unavailable_status(error),
            Err(_) => unavailable_status("Timed out while querying Codex status".to_owned()),
        }
    }

    async fn resolve_codex_command(&self) -> Result<String, AgentError> {
        if let Ok(path) = which::which("codex") {
            return Ok(path.display().to_string());
        }

        if let Some(agent) = self.registry.find_builtin_by_backend("codex").await {
            if let Some(path) = agent.resolved_command {
                return Ok(path.display().to_string());
            }
            if let Some(command) = agent.command {
                return Ok(command);
            }
        }

        which::which("codex")
            .map(|path| path.display().to_string())
            .map_err(|error| AgentError::internal(format!("resolve codex command: {error}")))
    }

    async fn query_with_command(&self, command: &str) -> Result<CodexStatusResponse, String> {
        let mut child = Command::new(command)
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&self.data_dir)
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("spawn codex app-server: {error}"))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "codex app-server stdin unavailable".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "codex app-server stdout unavailable".to_owned())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "codex app-server stderr unavailable".to_owned())?;

        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut stderr_buf = String::new();
            let _ = reader.read_to_string(&mut stderr_buf).await;
            stderr_buf
        });

        let mut stdout_reader = BufReader::new(stdout).lines();

        write_json_line(
            &mut stdin,
            &JsonRpcRequest {
                method: "initialize",
                id: Some(1),
                params: Some(serde_json::json!({
                    "clientInfo": {
                        "name": "aionui",
                        "title": "AionUi",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                })),
            },
        )
        .await
        .map_err(|error| format!("write initialize: {error}"))?;

        wait_for_response(&mut stdout_reader, 1)
            .await
            .map_err(|error| format!("initialize failed: {error}"))?;

        write_json_line(
            &mut stdin,
            &JsonRpcRequest {
                method: "initialized",
                id: None,
                params: Some(serde_json::json!({})),
            },
        )
        .await
        .map_err(|error| format!("write initialized: {error}"))?;

        write_json_line(
            &mut stdin,
            &JsonRpcRequest {
                method: "account/read",
                id: Some(2),
                params: Some(serde_json::json!({ "refreshToken": false })),
            },
        )
        .await
        .map_err(|error| format!("write account/read: {error}"))?;

        write_json_line(
            &mut stdin,
            &JsonRpcRequest {
                method: "account/rateLimits/read",
                id: Some(3),
                params: Some(serde_json::json!({})),
            },
        )
        .await
        .map_err(|error| format!("write account/rateLimits/read: {error}"))?;

        let account_response = wait_for_response(&mut stdout_reader, 2).await;
        let rate_limits_response = wait_for_response(&mut stdout_reader, 3).await;

        drop(stdin);
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        let _ = child.kill().await;

        let stderr_output = stderr_task.await.unwrap_or_default();
        if let Err(error) = &account_response {
            return Err(augment_error(error, &stderr_output));
        }
        if let Err(error) = &rate_limits_response {
            return Err(augment_error(error, &stderr_output));
        }

        let account_response = account_response.expect("account response checked above");
        let rate_limits_response = rate_limits_response.expect("rate limits response checked above");
        Ok(build_status_response(&account_response, &rate_limits_response))
    }
}

#[derive(Serialize)]
struct JsonRpcRequest<'a> {
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

async fn write_json_line(stdin: &mut tokio::process::ChildStdin, payload: &impl Serialize) -> Result<(), String> {
    let mut encoded = serde_json::to_vec(payload).map_err(|error| format!("serialize json-rpc payload: {error}"))?;
    encoded.push(b'\n');
    stdin
        .write_all(&encoded)
        .await
        .map_err(|error| format!("write json-rpc payload: {error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("flush json-rpc payload: {error}"))
}

async fn wait_for_response(
    stdout_reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    expected_id: u64,
) -> Result<Value, String> {
    loop {
        let next_line = tokio::time::timeout(QUERY_TIMEOUT, stdout_reader.next_line())
            .await
            .map_err(|_| format!("timeout waiting for response id {expected_id}"))?;
        let line = next_line
            .map_err(|error| format!("read stdout: {error}"))?
            .ok_or_else(|| format!("codex app-server closed stdout before response id {expected_id}"))?;
        let payload: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                continue;
            }
        };

        let Some(id) = payload.get("id").and_then(Value::as_u64) else {
            continue;
        };
        if id != expected_id {
            continue;
        }

        if let Some(error) = payload.get("error") {
            return Err(json_rpc_error_message(error));
        }

        return Ok(payload);
    }
}

fn json_rpc_error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| error.to_string())
}

fn build_status_response(account_response: &Value, rate_limits_response: &Value) -> CodexStatusResponse {
    let account_result = account_response.get("result").unwrap_or(account_response);
    let rate_limits_result = rate_limits_response.get("result").unwrap_or(rate_limits_response);
    let account = account_result.get("account");
    let auth_mode = account
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let plan_type = account
        .and_then(|value| value.get("planType"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let requires_openai_auth = account_result
        .get("requiresOpenaiAuth")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| auth_mode.is_some());

    let rate_limits = rate_limits_result
        .get("rateLimits")
        .and_then(parse_rate_limits_snapshot);

    CodexStatusResponse {
        available: true,
        checked_at_ms: now_ms(),
        requires_openai_auth,
        auth_mode,
        plan_type,
        rate_limits,
        error: None,
    }
}

fn parse_rate_limits_snapshot(value: &Value) -> Option<CodexRateLimitsSnapshot> {
    let primary = value.get("primary").and_then(parse_rate_limit_window);
    let secondary = value.get("secondary").and_then(parse_rate_limit_window);
    let rate_limit_reached_type = value
        .get("rateLimitReachedType")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    if primary.is_none() && secondary.is_none() && rate_limit_reached_type.is_none() {
        return None;
    }

    Some(CodexRateLimitsSnapshot {
        primary,
        secondary,
        rate_limit_reached_type,
    })
}

fn parse_rate_limit_window(value: &Value) -> Option<CodexRateLimitWindow> {
    Some(CodexRateLimitWindow {
        used_percent: value.get("usedPercent")?.as_f64()?,
        window_duration_mins: value.get("windowDurationMins")?.as_i64()?,
        resets_at: value.get("resetsAt")?.as_i64()?,
    })
}

fn unavailable_status(error: String) -> CodexStatusResponse {
    CodexStatusResponse {
        available: false,
        checked_at_ms: now_ms(),
        requires_openai_auth: true,
        auth_mode: None,
        plan_type: None,
        rate_limits: None,
        error: Some(error),
    }
}

fn augment_error(error: &str, stderr_output: &str) -> String {
    let stderr_trimmed = stderr_output.trim();
    if stderr_trimmed.is_empty() {
        return error.to_owned();
    }
    format!("{error} | stderr: {stderr_trimmed}")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_full_codex_status_snapshot() {
        let account_response = json!({
            "id": 2,
            "result": {
                "account": {
                    "type": "chatgpt",
                    "planType": "pro"
                },
                "requiresOpenaiAuth": true
            }
        });
        let rate_limits_response = json!({
            "id": 3,
            "result": {
                "rateLimits": {
                    "primary": {
                        "usedPercent": 12.5,
                        "windowDurationMins": 300,
                        "resetsAt": 1730947200
                    },
                    "secondary": {
                        "usedPercent": 73.0,
                        "windowDurationMins": 10080,
                        "resetsAt": 1731552000
                    },
                    "rateLimitReachedType": null
                }
            }
        });

        let response = build_status_response(&account_response, &rate_limits_response);

        assert!(response.available);
        assert_eq!(response.auth_mode.as_deref(), Some("chatgpt"));
        assert_eq!(response.plan_type.as_deref(), Some("pro"));
        assert!(response.requires_openai_auth);
        assert_eq!(
            response.rate_limits.as_ref().and_then(|limits| limits.primary.as_ref()),
            Some(&CodexRateLimitWindow {
                used_percent: 12.5,
                window_duration_mins: 300,
                resets_at: 1730947200,
            })
        );
        assert_eq!(
            response
                .rate_limits
                .as_ref()
                .and_then(|limits| limits.secondary.as_ref()),
            Some(&CodexRateLimitWindow {
                used_percent: 73.0,
                window_duration_mins: 10080,
                resets_at: 1731552000,
            })
        );
    }

    #[test]
    fn parses_account_without_chatgpt_limits() {
        let account_response = json!({
            "id": 2,
            "result": {
                "account": {
                    "type": "apiKey"
                },
                "requiresOpenaiAuth": true
            }
        });
        let rate_limits_response = json!({
            "id": 3,
            "result": {
                "rateLimits": {
                    "primary": null,
                    "secondary": null,
                    "rateLimitReachedType": null
                }
            }
        });

        let response = build_status_response(&account_response, &rate_limits_response);

        assert!(response.available);
        assert_eq!(response.auth_mode.as_deref(), Some("apiKey"));
        assert!(response.rate_limits.is_none());
    }

    #[test]
    fn json_rpc_error_prefers_message_field() {
        let message = json_rpc_error_message(&json!({
            "code": 123,
            "message": "nope"
        }));
        assert_eq!(message, "nope");
    }

    #[test]
    fn unavailable_status_sets_error_and_unavailable_flag() {
        let response = unavailable_status("boom".to_owned());
        assert!(!response.available);
        assert_eq!(response.error.as_deref(), Some("boom"));
        assert!(response.rate_limits.is_none());
    }
}

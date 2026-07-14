use crate::model::{Credits, RateWindow, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use chrono::{DateTime, TimeZone, Utc};
use std::path::PathBuf;
use tracing::debug;

pub struct CodexProvider;

impl CodexProvider {
    pub fn new() -> Self {
        Self
    }
}

// ── credential helpers ────────────────────────────────────────────────────────

fn auth_path() -> Option<PathBuf> {
    let base = std::env::var("CODEX_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
    Some(base.join("auth.json"))
}

#[derive(Debug)]
struct Credentials {
    access_token: String,
    account_id: Option<String>,
}

fn read_credentials() -> anyhow::Result<Credentials> {
    let path = auth_path().context("cannot determine CODEX_HOME or home directory")?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).with_context(|| "auth.json is not valid JSON")?;

    let access_token = json
        .get("tokens")
        .and_then(|t| t.get("access_token"))
        .and_then(|v| v.as_str())
        .context("auth.json missing tokens.access_token")?
        .to_string();

    let account_id = json
        .get("tokens")
        .and_then(|t| t.get("account_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(Credentials {
        access_token,
        account_id,
    })
}

// HTTP transport shared with the Claude provider.
use super::claude::http_get;

// ── response parsing ──────────────────────────────────────────────────────────

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn plan_name(plan_type: Option<&str>) -> Option<String> {
    plan_type.map(|s| match s.to_lowercase().as_str() {
        "plus" => "Plus".to_string(),
        "pro" => "Pro".to_string(),
        "team" => "Team".to_string(),
        "enterprise" => "Enterprise".to_string(),
        other => capitalize(other),
    })
}

const SECONDS_PER_DAY: i64 = 24 * 3600;
const SECONDS_SESSION_THRESHOLD: i64 = 6 * 3600;
const SECONDS_MONTHLY_THRESHOLD: i64 = 8 * SECONDS_PER_DAY;

/// Label a limit window from its duration (not primary/secondary field name).
fn label_from_duration(limit_window_seconds: Option<i64>) -> String {
    match limit_window_seconds {
        Some(s) if s > 0 && s <= SECONDS_SESSION_THRESHOLD => "Session".to_string(),
        Some(s) if s <= SECONDS_MONTHLY_THRESHOLD => "Weekly".to_string(),
        Some(_) => "Monthly".to_string(),
        None => "Limit".to_string(),
    }
}

fn parse_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn window_resets_at(w: &serde_json::Value, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Some(ts) = w.get("reset_at").and_then(|v| v.as_i64()) {
        return Utc.timestamp_opt(ts, 0).single();
    }
    let after = w.get("reset_after_seconds").and_then(|v| v.as_i64())?;
    Some(now + chrono::Duration::seconds(after))
}

fn parse_rate_window(
    w: &serde_json::Value,
    label: String,
    now: DateTime<Utc>,
) -> Option<RateWindow> {
    if w.is_null() {
        return None;
    }
    let used_percent = w.get("used_percent").and_then(parse_f64)?;
    let limit_window_seconds = w.get("limit_window_seconds").and_then(|v| v.as_i64());
    Some(RateWindow {
        label,
        used_percent: used_percent.clamp(0.0, 100.0),
        resets_at: window_resets_at(w, now),
        caption: limit_window_seconds.and_then(window_duration_caption),
    })
}

fn window_duration_caption(seconds: i64) -> Option<String> {
    if seconds <= 0 {
        return None;
    }
    if seconds % SECONDS_PER_DAY == 0 {
        let days = seconds / SECONDS_PER_DAY;
        return Some(format!("{days}-day window"));
    }
    if seconds % 3600 == 0 {
        let hours = seconds / 3600;
        return Some(format!("{hours} h window"));
    }
    None
}

fn push_core_windows(
    rate_limit: Option<&serde_json::Value>,
    now: DateTime<Utc>,
) -> Vec<RateWindow> {
    let mut collected: Vec<(i64, RateWindow)> = Vec::new();
    let Some(rate_limit) = rate_limit else {
        return Vec::new();
    };

    for field in ["primary_window", "secondary_window"] {
        let Some(w) = rate_limit.get(field) else {
            continue;
        };
        let limit_window_seconds = w.get("limit_window_seconds").and_then(|v| v.as_i64());
        let label = label_from_duration(limit_window_seconds);
        let Some(window) = parse_rate_window(w, label, now) else {
            continue;
        };
        let sort_key = limit_window_seconds.unwrap_or(i64::MAX);
        collected.push((sort_key, window));
    }

    collected.sort_by_key(|(seconds, _)| *seconds);
    collected.into_iter().map(|(_, window)| window).collect()
}

fn is_spark_limit(entry: &serde_json::Value) -> bool {
    [entry.get("limit_name"), entry.get("metered_feature")]
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .any(|s| s.to_lowercase().contains("spark"))
}

fn spark_window_label(limit_window_seconds: Option<i64>, fallback: &str) -> String {
    match limit_window_seconds {
        Some(s) if s > 0 && s <= SECONDS_SESSION_THRESHOLD => "Codex Spark 5-hour".to_string(),
        Some(s) if s >= 6 * SECONDS_PER_DAY => "Codex Spark Weekly".to_string(),
        _ => fallback.to_string(),
    }
}

fn extra_limit_title(entry: &serde_json::Value) -> String {
    entry
        .get("limit_name")
        .or_else(|| entry.get("metered_feature"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(capitalize)
        .unwrap_or_else(|| "Codex extra limit".to_string())
}

fn push_additional_windows(json: &serde_json::Value, now: DateTime<Utc>) -> Vec<RateWindow> {
    let Some(entries) = json
        .get("additional_rate_limits")
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };

    let mut windows = Vec::new();
    let mut seen_spark = [false; 2];

    for entry in entries {
        let Some(rate_limit) = entry.get("rate_limit") else {
            continue;
        };
        let spark = is_spark_limit(entry);

        for field in ["primary_window", "secondary_window"] {
            let Some(w) = rate_limit.get(field) else {
                continue;
            };
            let limit_window_seconds = w.get("limit_window_seconds").and_then(|v| v.as_i64());
            let label = if spark {
                let kind = match limit_window_seconds {
                    Some(s) if s > 0 && s <= SECONDS_SESSION_THRESHOLD => 0,
                    Some(s) if s >= 6 * SECONDS_PER_DAY => 1,
                    _ if field == "primary_window" => 0,
                    _ => 1,
                };
                if seen_spark[kind] {
                    continue;
                }
                seen_spark[kind] = true;
                spark_window_label(limit_window_seconds, "Codex Spark")
            } else {
                extra_limit_title(entry)
            };
            let Some(window) = parse_rate_window(w, label, now) else {
                continue;
            };
            windows.push(window);
        }
    }

    windows
}

fn parse_credits(c: &serde_json::Value) -> Option<Credits> {
    let has_credits = c
        .get("has_credits")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let unlimited = c
        .get("unlimited")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !has_credits || unlimited {
        return None;
    }
    let balance = c.get("balance").and_then(parse_f64)?;
    Some(Credits::from_balance(balance, None))
}

fn parse_usage_response(body: &str) -> anyhow::Result<UsageSnapshot> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("usage response is not JSON")?;

    let now = Utc::now();
    let plan_type = json.get("plan_type").and_then(|v| v.as_str());
    let account = json
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut windows = push_core_windows(json.get("rate_limit"), now);
    windows.extend(push_additional_windows(&json, now));

    let credits = json.get("credits").and_then(parse_credits);

    debug!("codex: parsed {} windows", windows.len());

    Ok(UsageSnapshot {
        plan: plan_name(plan_type),
        account,
        windows,
        credits,
        fetched_at: Some(now),
    })
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for CodexProvider {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn is_configured(&self) -> bool {
        let Some(path) = auth_path() else {
            return false;
        };
        if !path.exists() {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            return false;
        };
        json.get("tokens")
            .and_then(|t| t.get("access_token"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        fetch_usage().await
    }
}

async fn fetch_usage() -> anyhow::Result<UsageSnapshot> {
    let creds = read_credentials()?;

    let auth_header = format!("Bearer {}", creds.access_token);
    let mut headers_owned: Vec<(String, String)> = vec![
        ("Authorization".to_string(), auth_header),
        ("Accept".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), "CodexBar".to_string()),
    ];

    if let Some(ref account_id) = creds.account_id {
        headers_owned.push(("ChatGPT-Account-Id".to_string(), account_id.clone()));
    }

    let header_refs: Vec<(&str, &str)> = headers_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let (status, body) =
        http_get("https://chatgpt.com/backend-api/wham/usage", &header_refs).await?;

    match status {
        200 => parse_usage_response(&body),
        401 | 403 => {
            bail!("Codex session unauthorized — run `codex` to re-authenticate.")
        }
        other => bail!("Codex usage: HTTP {}", other),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const HAPPY_PATH_RESPONSE: &str = r#"{
        "plan_type": "plus",
        "rate_limit": {
            "primary_window": {
                "used_percent": 35,
                "reset_at": 1737043200,
                "limit_window_seconds": 10800
            },
            "secondary_window": {
                "used_percent": 72,
                "reset_at": 1737648000,
                "limit_window_seconds": 604800
            }
        },
        "credits": {
            "has_credits": true,
            "unlimited": false,
            "balance": 12.50
        }
    }"#;

    const PRO_MONTHLY_RESPONSE: &str = r#"{
        "plan_type": "pro",
        "rate_limit": {
            "primary_window": {
                "used_percent": 10,
                "reset_at": 1737043200,
                "limit_window_seconds": 3600
            },
            "secondary_window": {
                "used_percent": 45,
                "reset_at": 1739721600,
                "limit_window_seconds": 2592000
            }
        },
        "credits": {
            "has_credits": false,
            "unlimited": false,
            "balance": 0.0
        }
    }"#;

    const PARTIAL_NULL_RESPONSE: &str = r#"{
        "plan_type": "plus",
        "rate_limit": {
            "primary_window": {
                "used_percent": 88,
                "reset_at": 1737043200,
                "limit_window_seconds": 10800
            },
            "secondary_window": null
        }
    }"#;

    const UNLIMITED_CREDITS_RESPONSE: &str = r#"{
        "plan_type": "pro",
        "rate_limit": {
            "primary_window": {
                "used_percent": 20,
                "reset_at": 1737043200,
                "limit_window_seconds": 3600
            }
        },
        "credits": {
            "has_credits": true,
            "unlimited": true,
            "balance": 9999.0
        }
    }"#;

    #[test]
    fn test_happy_path() {
        let snap = parse_usage_response(HAPPY_PATH_RESPONSE).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Plus"));
        assert_eq!(snap.windows.len(), 2);

        let session = &snap.windows[0];
        assert_eq!(session.label, "Session");
        assert!((session.used_percent - 35.0).abs() < 0.01);
        assert!(session.resets_at.is_some());

        let weekly = &snap.windows[1];
        assert_eq!(weekly.label, "Weekly");
        assert!((weekly.used_percent - 72.0).abs() < 0.01);

        let credits = snap.credits.unwrap();
        assert!((credits.balance - 12.50).abs() < 0.01);
        assert!(credits.currency.is_none());
    }

    #[test]
    fn test_pro_monthly_secondary_window() {
        let snap = parse_usage_response(PRO_MONTHLY_RESPONSE).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Pro"));
        assert_eq!(snap.windows.len(), 2);
        assert_eq!(snap.windows[0].label, "Session");
        // secondary_window with limit_window_seconds = 2592000 (30 days) > 8 days → Monthly
        assert_eq!(snap.windows[1].label, "Monthly");
        // credits.has_credits is false → None
        assert!(snap.credits.is_none());
    }

    #[test]
    fn test_partial_null_secondary_window() {
        let snap = parse_usage_response(PARTIAL_NULL_RESPONSE).unwrap();
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "Session");
        assert!((snap.windows[0].used_percent - 88.0).abs() < 0.01);
        assert!(snap.credits.is_none());
    }

    #[test]
    fn test_unlimited_credits_not_shown() {
        let snap = parse_usage_response(UNLIMITED_CREDITS_RESPONSE).unwrap();
        // unlimited = true → credits should be None
        assert!(snap.credits.is_none());
    }

    #[test]
    fn test_plan_capitalization() {
        assert_eq!(plan_name(Some("plus")).as_deref(), Some("Plus"));
        assert_eq!(plan_name(Some("pro")).as_deref(), Some("Pro"));
        assert_eq!(plan_name(Some("team")).as_deref(), Some("Team"));
        assert_eq!(plan_name(Some("enterprise")).as_deref(), Some("Enterprise"));
        assert_eq!(plan_name(Some("business")).as_deref(), Some("Business"));
        assert_eq!(plan_name(None), None);
    }

    #[test]
    fn test_label_from_duration() {
        assert_eq!(label_from_duration(Some(3600)), "Session");
        assert_eq!(
            label_from_duration(Some(SECONDS_SESSION_THRESHOLD)),
            "Session"
        );
        assert_eq!(
            label_from_duration(Some(SECONDS_SESSION_THRESHOLD + 1)),
            "Weekly"
        );
        assert_eq!(label_from_duration(Some(7 * SECONDS_PER_DAY)), "Weekly");
        assert_eq!(
            label_from_duration(Some(SECONDS_MONTHLY_THRESHOLD + 1)),
            "Monthly"
        );
        assert_eq!(label_from_duration(None), "Limit");
    }

    #[test]
    fn test_plus_weekly_only_primary_window() {
        let body = r#"{
            "plan_type": "plus",
            "email": "user@example.com",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 10,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 601137,
                    "reset_at": 1784666295
                },
                "secondary_window": null
            },
            "additional_rate_limits": null,
            "credits": { "has_credits": false, "unlimited": false, "balance": "0" }
        }"#;
        let snap = parse_usage_response(body).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Plus"));
        assert_eq!(snap.account.as_deref(), Some("user@example.com"));
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "Weekly");
        assert_eq!(snap.windows[0].caption.as_deref(), Some("7-day window"));
    }

    #[test]
    fn test_additional_spark_limits() {
        let body = r#"{
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": { "used_percent": 22, "reset_at": 1766948068, "limit_window_seconds": 18000 },
                "secondary_window": { "used_percent": 43, "reset_at": 1767407914, "limit_window_seconds": 604800 }
            },
            "additional_rate_limits": [
                {
                    "limit_name": "GPT-5.3-Codex-Spark",
                    "metered_feature": "gpt_5_3_codex_spark",
                    "rate_limit": {
                        "primary_window": { "used_percent": 30, "reset_at": 1766948068, "limit_window_seconds": 18000 },
                        "secondary_window": { "used_percent": 100, "reset_at": 1767407914, "limit_window_seconds": 604800 }
                    }
                }
            ]
        }"#;
        let snap = parse_usage_response(body).unwrap();
        assert_eq!(snap.windows.len(), 4);
        assert_eq!(snap.windows[0].label, "Session");
        assert_eq!(snap.windows[1].label, "Weekly");
        assert_eq!(snap.windows[2].label, "Codex Spark 5-hour");
        assert_eq!(snap.windows[3].label, "Codex Spark Weekly");
    }

    #[test]
    fn test_reset_after_seconds_fallback() {
        let body = r#"{
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 5,
                    "limit_window_seconds": 10800,
                    "reset_after_seconds": 7200
                }
            }
        }"#;
        let snap = parse_usage_response(body).unwrap();
        assert!(snap.windows[0].resets_at.is_some());
    }

    #[test]
    fn test_session_vs_weekly_window_label() {
        assert_eq!(label_from_duration(Some(3600)), "Session");
        assert_eq!(label_from_duration(Some(SECONDS_PER_DAY)), "Weekly");
    }

    #[test]
    fn test_percent_clamping() {
        let body = r#"{
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 150,
                    "reset_at": 1737043200,
                    "limit_window_seconds": 3600
                }
            }
        }"#;
        let snap = parse_usage_response(body).unwrap();
        assert_eq!(snap.windows[0].used_percent, 100.0);
    }

    #[test]
    fn test_unknown_fields_tolerated() {
        let body = r#"{
            "plan_type": "plus",
            "future_field": "ignored",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 50,
                    "reset_at": 1737043200,
                    "limit_window_seconds": 3600,
                    "new_unknown": true
                }
            }
        }"#;
        let snap = parse_usage_response(body).unwrap();
        assert_eq!(snap.windows.len(), 1);
    }
}

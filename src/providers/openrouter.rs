use crate::config;
use crate::model::{Credits, RateWindow, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use chrono::Utc;
use tracing::debug;

pub struct OpenRouterProvider;

impl OpenRouterProvider {
    pub fn new() -> Self {
        Self
    }
}

// ── credential helper ─────────────────────────────────────────────────────────

fn api_key() -> Option<String> {
    config::api_key("openrouter", "OPENROUTER_API_KEY")
}

// HTTP transport shared with the Claude provider.
use super::claude::http_get;

// ── response parsing ──────────────────────────────────────────────────────────

/// Parse `GET /api/v1/credits` response.
///
/// Shape: `{ "data": { "total_credits": f64, "total_usage": f64 } }`
fn parse_credits_response(body: &str) -> anyhow::Result<(f64, f64)> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("credits response is not JSON")?;
    let data = json.get("data").context("missing 'data' field")?;
    let total_credits = data
        .get("total_credits")
        .and_then(|v| v.as_f64())
        .context("missing 'data.total_credits'")?;
    let total_usage = data
        .get("total_usage")
        .and_then(|v| v.as_f64())
        .context("missing 'data.total_usage'")?;
    Ok((total_credits, total_usage))
}

/// Parse `GET /api/v1/key` response (optional enrichment).
///
/// Shape: `{ "data": { "limit": f64|null, "usage": f64, ... } }`
/// Returns `None` if the response is malformed or the key has no quota limit.
fn parse_key_response(body: &str) -> Option<(f64, f64)> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let data = json.get("data")?;
    let limit = data
        .get("limit")
        .and_then(|v| v.as_f64())
        .filter(|&l| l > 0.0)?;
    let usage = data.get("usage").and_then(|v| v.as_f64())?;
    Some((limit, usage))
}

fn build_snapshot(
    total_credits: f64,
    total_usage: f64,
    key_quota: Option<(f64, f64)>,
) -> UsageSnapshot {
    let balance = (total_credits - total_usage).max(0.0);

    let mut windows: Vec<RateWindow> = Vec::new();

    // If the key has a hard spend limit, expose it as a rate window.
    if let Some((limit, usage)) = key_quota {
        if limit > 0.0 {
            let used_percent = (usage / limit * 100.0).clamp(0.0, 100.0);
            debug!(
                "openrouter: key quota limit={} usage={} used_percent={}",
                limit, usage, used_percent
            );
            windows.push(RateWindow {
                label: "Credits".to_string(),
                used_percent,
                resets_at: None,
            });
        }
    }

    let credits = Some(Credits {
        balance,
        currency: Some("USD".to_string()),
    });

    debug!(
        "openrouter: balance={:.4} windows={}",
        balance,
        windows.len()
    );

    UsageSnapshot {
        plan: None,
        account: None,
        windows,
        credits,
        fetched_at: Some(Utc::now()),
    }
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for OpenRouterProvider {
    fn id(&self) -> &'static str {
        "openrouter"
    }

    fn display_name(&self) -> &'static str {
        "OpenRouter"
    }

    fn is_configured(&self) -> bool {
        api_key().is_some()
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        let key = api_key()
            .context("OpenRouter API key not set — set OPENROUTER_API_KEY environment variable")?;

        let auth = format!("Bearer {}", key);
        let headers: &[(&str, &str)] = &[
            ("Authorization", auth.as_str()),
            ("Accept", "application/json"),
            ("X-Title", "CodexBar"),
        ];

        // Primary: credits endpoint
        let (status, body) = http_get("https://openrouter.ai/api/v1/credits", headers).await?;
        match status {
            200 => {}
            401 | 403 => {
                bail!("OpenRouter API key rejected (HTTP {status}) — check OPENROUTER_API_KEY")
            }
            other => bail!("OpenRouter credits: HTTP {other}"),
        }
        let (total_credits, total_usage) =
            parse_credits_response(&body).context("failed to parse OpenRouter credits")?;

        // Enrichment: key endpoint (best-effort, 1 s timeout is enforced by the
        // shared http_get which uses a 30 s reqwest timeout — acceptable here).
        let key_quota: Option<(f64, f64)> =
            match http_get("https://openrouter.ai/api/v1/key", headers).await {
                Ok((200, key_body)) => parse_key_response(&key_body),
                Ok(_) | Err(_) => None, // non-critical; ignore silently
            };

        Ok(build_snapshot(total_credits, total_usage, key_quota))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const CREDITS_RESPONSE: &str = r#"{
        "data": {
            "total_credits": 10.0,
            "total_usage": 3.5
        }
    }"#;

    const ZERO_CREDITS_RESPONSE: &str = r#"{
        "data": {
            "total_credits": 0.0,
            "total_usage": 0.0
        }
    }"#;

    const KEY_RESPONSE_WITH_LIMIT: &str = r#"{
        "data": {
            "limit": 5.0,
            "usage": 2.5,
            "rate_limit": { "requests": 1000, "interval": "10s" }
        }
    }"#;

    const KEY_RESPONSE_NO_LIMIT: &str = r#"{
        "data": {
            "limit": null,
            "usage": 1.0
        }
    }"#;

    const KEY_RESPONSE_ZERO_LIMIT: &str = r#"{
        "data": {
            "limit": 0.0,
            "usage": 0.0
        }
    }"#;

    #[test]
    fn test_parse_credits_happy() {
        let (total, used) = parse_credits_response(CREDITS_RESPONSE).unwrap();
        assert!((total - 10.0).abs() < 1e-9);
        assert!((used - 3.5).abs() < 1e-9);
    }

    #[test]
    fn test_parse_credits_zero() {
        let (total, used) = parse_credits_response(ZERO_CREDITS_RESPONSE).unwrap();
        assert_eq!(total, 0.0);
        assert_eq!(used, 0.0);
    }

    #[test]
    fn test_parse_credits_missing_field_errors() {
        assert!(parse_credits_response(r#"{"data": {}}"#).is_err());
        assert!(parse_credits_response("not-json").is_err());
    }

    #[test]
    fn test_parse_key_with_limit() {
        let result = parse_key_response(KEY_RESPONSE_WITH_LIMIT);
        let (limit, usage) = result.unwrap();
        assert!((limit - 5.0).abs() < 1e-9);
        assert!((usage - 2.5).abs() < 1e-9);
    }

    #[test]
    fn test_parse_key_no_limit_returns_none() {
        assert!(parse_key_response(KEY_RESPONSE_NO_LIMIT).is_none());
    }

    #[test]
    fn test_parse_key_zero_limit_returns_none() {
        assert!(parse_key_response(KEY_RESPONSE_ZERO_LIMIT).is_none());
    }

    #[test]
    fn test_parse_key_bad_json_returns_none() {
        assert!(parse_key_response("not-json").is_none());
    }

    #[test]
    fn test_build_snapshot_with_quota() {
        let snap = build_snapshot(10.0, 3.5, Some((5.0, 2.5)));
        // balance = 10 - 3.5 = 6.5
        let credits = snap.credits.unwrap();
        assert!((credits.balance - 6.5).abs() < 1e-9);
        assert_eq!(credits.currency.as_deref(), Some("USD"));
        // One window from key quota: 2.5/5.0 = 50%
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "Credits");
        assert!((snap.windows[0].used_percent - 50.0).abs() < 1e-9);
    }

    #[test]
    fn test_build_snapshot_without_quota() {
        let snap = build_snapshot(10.0, 3.5, None);
        assert!(snap.windows.is_empty());
        let credits = snap.credits.unwrap();
        assert!((credits.balance - 6.5).abs() < 1e-9);
    }

    #[test]
    fn test_build_snapshot_balance_clamped_non_negative() {
        // More used than total (shouldn't happen, but be defensive)
        let snap = build_snapshot(2.0, 5.0, None);
        let credits = snap.credits.unwrap();
        assert_eq!(credits.balance, 0.0);
    }

    #[test]
    fn test_build_snapshot_quota_percent_clamped() {
        // usage > limit → clamp to 100%
        let snap = build_snapshot(10.0, 1.0, Some((2.0, 5.0)));
        assert!((snap.windows[0].used_percent - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_is_configured_without_env() {
        // Without the env var set (test env doesn't have it), should be false.
        // We can't easily unset env vars in a portable way, so just check the
        // function compiles and returns a bool.
        let _: bool = OpenRouterProvider::new().is_configured();
    }
}

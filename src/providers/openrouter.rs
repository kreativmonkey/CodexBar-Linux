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
        .or_else(super::opencode_auth::openrouter_key)
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

/// Parsed subset of `GET /api/v1/key`.
#[derive(Debug, Clone, PartialEq)]
struct KeyInfo {
    label: Option<String>,
    limit: Option<f64>,
    usage: f64,
    usage_daily: f64,
    usage_weekly: f64,
    usage_monthly: f64,
}

/// Parse `GET /api/v1/key` response.
fn parse_key_response(body: &str) -> Option<KeyInfo> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let data = json.get("data")?;
    Some(KeyInfo {
        label: data
            .get("label")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        limit: data
            .get("limit")
            .and_then(|v| v.as_f64())
            .filter(|&l| l > 0.0),
        usage: data.get("usage").and_then(|v| v.as_f64()).unwrap_or(0.0),
        usage_daily: data
            .get("usage_daily")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        usage_weekly: data
            .get("usage_weekly")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        usage_monthly: data
            .get("usage_monthly")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
    })
}

fn account_used_percent(total_credits: f64, total_usage: f64) -> f64 {
    if total_credits > 0.0 {
        (total_usage / total_credits * 100.0).clamp(0.0, 100.0)
    } else if total_usage > 0.0 {
        100.0
    } else {
        0.0
    }
}

fn spend_vs_remaining_percent(spend: f64, remaining: f64) -> f64 {
    if remaining > 0.0 {
        (spend / remaining * 100.0).clamp(0.0, 100.0)
    } else if spend > 0.0 {
        100.0
    } else {
        0.0
    }
}

fn period_window(label: &str, spend: f64, remaining: f64) -> RateWindow {
    RateWindow::new(label, spend_vs_remaining_percent(spend, remaining))
        .with_caption(format!("${spend:.2} / ${remaining:.2} left"))
}

fn build_snapshot(
    total_credits: f64,
    total_usage: f64,
    key_info: Option<KeyInfo>,
) -> UsageSnapshot {
    let balance = (total_credits - total_usage).max(0.0);
    let mut windows: Vec<RateWindow> = Vec::new();

    if total_credits > 0.0 || total_usage > 0.0 {
        let used_percent = account_used_percent(total_credits, total_usage);
        debug!(
            "openrouter: account credits={} usage={} used_percent={}",
            total_credits, total_usage, used_percent
        );
        windows.push(
            RateWindow::new("Credits", used_percent)
                .with_caption(format!("${total_usage:.2} / ${total_credits:.2}")),
        );
    }

    if let Some(ref key) = key_info {
        windows.push(period_window("Today", key.usage_daily, balance));
        windows.push(period_window("This week", key.usage_weekly, balance));
        windows.push(period_window("This month", key.usage_monthly, balance));

        if let Some(limit) = key.limit {
            let used_percent = (key.usage / limit * 100.0).clamp(0.0, 100.0);
            debug!(
                "openrouter: key limit={} usage={} used_percent={}",
                limit, key.usage, used_percent
            );
            windows.push(
                RateWindow::new("Key limit", used_percent)
                    .with_caption(format!("${:.2} / ${:.2}", key.usage, limit)),
            );
        }
    }

    let credits = Some(Credits {
        balance,
        currency: Some("USD".to_string()),
        used: Some(total_usage),
        limit: total_credits.gt(&0.0).then_some(total_credits),
    });

    debug!(
        "openrouter: balance={:.4} windows={}",
        balance,
        windows.len()
    );

    UsageSnapshot {
        plan: key_info.and_then(|k| k.label),
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
        let key = api_key().context(
            "OpenRouter API key not set — set OPENROUTER_API_KEY, add \
             [keys] openrouter to config.toml, or run `opencode` → /connect → OpenRouter",
        )?;

        let auth = format!("Bearer {}", key);
        let headers: &[(&str, &str)] = &[
            ("Authorization", auth.as_str()),
            ("Accept", "application/json"),
            ("X-Title", "CodexBar"),
        ];

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

        let key_info = match http_get("https://openrouter.ai/api/v1/key", headers).await {
            Ok((200, key_body)) => parse_key_response(&key_body),
            Ok(_) | Err(_) => None,
        };

        Ok(build_snapshot(total_credits, total_usage, key_info))
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

    const KEY_RESPONSE_WITH_LIMIT: &str = r#"{
        "data": {
            "label": "prod-key",
            "limit": 5.0,
            "usage": 2.5,
            "usage_daily": 0.5,
            "usage_weekly": 1.0,
            "usage_monthly": 2.0
        }
    }"#;

    const KEY_RESPONSE_NO_LIMIT: &str = r#"{
        "data": {
            "label": "dev-key",
            "limit": null,
            "usage": 0.03,
            "usage_daily": 0.03,
            "usage_weekly": 0.03,
            "usage_monthly": 0.03
        }
    }"#;

    #[test]
    fn test_parse_credits_happy() {
        let (total, used) = parse_credits_response(CREDITS_RESPONSE).unwrap();
        assert!((total - 10.0).abs() < 1e-9);
        assert!((used - 3.5).abs() < 1e-9);
    }

    #[test]
    fn test_build_snapshot_account_and_period_bars() {
        let key = parse_key_response(KEY_RESPONSE_NO_LIMIT).unwrap();
        let snap = build_snapshot(60.0, 51.484388232, Some(key));
        assert_eq!(snap.windows.len(), 4);
        assert_eq!(snap.windows[0].label, "Credits");
        assert!((snap.windows[0].used_percent - 85.8).abs() < 0.1);
        assert_eq!(snap.windows[1].label, "Today");
        assert_eq!(snap.windows[2].label, "This week");
        assert_eq!(snap.windows[3].label, "This month");
        assert!(snap.windows[1]
            .caption
            .as_deref()
            .unwrap()
            .contains("$0.03"));
    }

    #[test]
    fn test_build_snapshot_includes_key_limit_bar() {
        let key = parse_key_response(KEY_RESPONSE_WITH_LIMIT).unwrap();
        let snap = build_snapshot(10.0, 3.5, Some(key));
        assert_eq!(snap.windows.len(), 5);
        assert_eq!(snap.windows[4].label, "Key limit");
        assert!((snap.windows[4].used_percent - 50.0).abs() < 1e-9);
    }

    #[test]
    fn test_period_percent_uses_remaining_balance() {
        // $0.03 of $8.52 remaining ≈ 0.35%
        let pct = spend_vs_remaining_percent(0.03, 8.515611768);
        assert!((pct - 0.35).abs() < 0.1);
    }

    #[test]
    fn test_build_snapshot_without_key_info() {
        let snap = build_snapshot(10.0, 3.5, None);
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "Credits");
    }
}

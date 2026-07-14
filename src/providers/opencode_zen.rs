use crate::config;
use crate::model::{Credits, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use chrono::Utc;
use tracing::debug;

use super::claude::http_get;

pub struct OpenCodeZenProvider;

impl OpenCodeZenProvider {
    pub fn new() -> Self {
        Self
    }
}

const BALANCE_URL: &str = "https://opencode.ai/zen/v1/balance";

fn api_key() -> Option<String> {
    config::api_key("opencode_zen", "OPENCODE_ZEN_API_KEY")
        .or_else(|| config::api_key("opencode_zen", "OPENCODE_API_KEY"))
        .or_else(super::opencode_auth::zen_key)
}

/// Parse a Zen balance response (proposed/public shape may vary).
fn parse_balance_response(body: &str) -> Option<(f64, Option<String>)> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let node = json.get("data").unwrap_or(&json);
    let balance = node.get("balance").and_then(|v| v.as_f64())?;
    let currency = node
        .get("currency")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some((balance, currency))
}

fn build_snapshot(balance: f64, currency: Option<String>) -> UsageSnapshot {
    UsageSnapshot {
        plan: Some("Zen".to_string()),
        credits: Some(Credits::from_balance(
            balance,
            Some(currency.unwrap_or_else(|| "USD".to_string())),
        )),
        fetched_at: Some(Utc::now()),
        ..Default::default()
    }
}

#[async_trait::async_trait]
impl Provider for OpenCodeZenProvider {
    fn id(&self) -> &'static str {
        "opencode_zen"
    }

    fn display_name(&self) -> &'static str {
        "OpenCode Zen"
    }

    fn is_configured(&self) -> bool {
        api_key().is_some()
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        let key = api_key().context(
            "OpenCode Zen API key not set — run `opencode` and `/connect` → OpenCode Zen, \
             or set OPENCODE_ZEN_API_KEY / [keys] opencode_zen",
        )?;

        let auth = format!("Bearer {}", key);
        let headers: &[(&str, &str)] = &[
            ("Authorization", auth.as_str()),
            ("Accept", "application/json"),
            ("X-Title", "CodexBar"),
        ];

        let (status, body) = http_get(BALANCE_URL, headers).await?;
        match status {
            200 => {}
            401 | 403 => bail!("OpenCode Zen API key rejected (HTTP {status})"),
            404 => bail!(
                "OpenCode Zen has no public balance API yet. \
                 Check balance at opencode.ai — tracking in \
                 https://github.com/anomalyco/opencode/issues/10448"
            ),
            other => bail!("OpenCode Zen balance: HTTP {other}"),
        }

        if let Some((balance, currency)) = parse_balance_response(&body) {
            debug!("opencode_zen: balance={balance:.4}");
            return Ok(build_snapshot(balance, currency));
        }

        bail!(
            "OpenCode Zen balance endpoint returned an unexpected response. \
             OpenCode does not expose account balance via API key yet — \
             use the web dashboard at opencode.ai."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_balance_flat() {
        let body = r#"{ "balance": 12.5, "currency": "USD" }"#;
        let (balance, currency) = parse_balance_response(body).unwrap();
        assert!((balance - 12.5).abs() < 1e-9);
        assert_eq!(currency.as_deref(), Some("USD"));
    }

    #[test]
    fn test_parse_balance_wrapped() {
        let body = r#"{ "data": { "balance": 3.0 } }"#;
        let (balance, currency) = parse_balance_response(body).unwrap();
        assert!((balance - 3.0).abs() < 1e-9);
        assert!(currency.is_none());
    }

    #[test]
    fn test_parse_balance_invalid() {
        assert!(parse_balance_response("not-json").is_none());
        assert!(parse_balance_response(r#"{ "usage": 1 }"#).is_none());
    }

    #[test]
    fn test_provider_metadata() {
        let p = OpenCodeZenProvider::new();
        assert_eq!(p.id(), "opencode_zen");
        assert_eq!(p.display_name(), "OpenCode Zen");
    }
}

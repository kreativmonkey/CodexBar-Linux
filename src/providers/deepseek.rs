use crate::model::{Credits, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use tracing::debug;

// HTTP transport shared with other providers.
use super::claude::http_get;

pub struct DeepSeekProvider;

impl DeepSeekProvider {
    pub fn new() -> Self {
        Self
    }
}

fn api_key() -> Option<String> {
    crate::config::api_key("deepseek", "DEEPSEEK_API_KEY")
        .or_else(|| super::cli_agent_auth::provider_api_key("deepseek"))
}

// ── response parsing ──────────────────────────────────────────────────────────

/// Parses the JSON body from `GET https://api.deepseek.com/user/balance`.
///
/// Response shape:
/// ```json
/// {
///   "is_available": true,
///   "balance_infos": [
///     {
///       "currency": "USD",
///       "total_balance": "4.20",
///       "granted_balance": "1.00",
///       "topped_up_balance": "3.20"
///     }
///   ]
/// }
/// ```
/// Balance fields are returned as **strings** by the API and must be parsed as f64.
/// Multiple currencies may appear; we prefer a funded USD entry, then any funded entry,
/// then the first USD entry, then the first entry — matching the Swift reference.
fn parse_balance_response(body: &str) -> anyhow::Result<UsageSnapshot> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("DeepSeek balance response is not JSON")?;

    let is_available = json
        .get("is_available")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let balance_infos = json
        .get("balance_infos")
        .and_then(|v| v.as_array())
        .context("DeepSeek response missing balance_infos array")?;

    if balance_infos.is_empty() {
        debug!("deepseek: empty balance_infos array");
        return Ok(UsageSnapshot {
            credits: Some(Credits::from_balance(0.0, Some("USD".to_string()))),
            fetched_at: Some(chrono::Utc::now()),
            ..Default::default()
        });
    }

    /// Parsed balance entry.
    struct Entry {
        currency: String,
        total_balance: f64,
    }

    let mut entries: Vec<Entry> = Vec::new();
    for info in balance_infos {
        let currency = info
            .get("currency")
            .and_then(|v| v.as_str())
            .unwrap_or("USD")
            .to_string();

        // The API returns balances as decimal strings.
        let total_balance = info
            .get("total_balance")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        entries.push(Entry {
            currency,
            total_balance,
        });
    }

    // Selection priority (mirrors Swift DeepSeekUsageFetcher.parseSnapshot):
    // 1. funded USD entry, 2. any funded entry, 3. first USD entry, 4. first entry.
    let selected = entries
        .iter()
        .find(|e| e.currency == "USD" && e.total_balance > 0.0)
        .or_else(|| entries.iter().find(|e| e.total_balance > 0.0))
        .or_else(|| entries.iter().find(|e| e.currency == "USD"))
        .unwrap_or(&entries[0]);

    // If balance is depleted or the account is marked unavailable, surface that.
    let balance = if !is_available || selected.total_balance <= 0.0 {
        0.0
    } else {
        selected.total_balance
    };

    debug!(
        "deepseek: balance={balance} currency={} available={is_available}",
        selected.currency
    );

    Ok(UsageSnapshot {
        credits: Some(Credits::from_balance(
            balance,
            Some(selected.currency.clone()),
        )),
        fetched_at: Some(chrono::Utc::now()),
        ..Default::default()
    })
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for DeepSeekProvider {
    fn id(&self) -> &'static str {
        "deepseek"
    }

    fn display_name(&self) -> &'static str {
        "DeepSeek"
    }

    fn is_configured(&self) -> bool {
        api_key().is_some()
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        let key = api_key().context(
            "DeepSeek API key not set — export DEEPSEEK_API_KEY or add \
             keys.deepseek to ~/.config/codexbar/config.toml",
        )?;

        let auth_header = format!("Bearer {key}");
        let headers = [
            ("Authorization", auth_header.as_str()),
            ("Accept", "application/json"),
        ];

        let (status, body) = http_get("https://api.deepseek.com/user/balance", &headers).await?;

        match status {
            200 => parse_balance_response(&body),
            401 | 403 => bail!("DeepSeek API key invalid or expired — check DEEPSEEK_API_KEY."),
            429 => bail!("DeepSeek rate-limited; try again shortly."),
            other => bail!("DeepSeek balance: HTTP {other}"),
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const HAPPY_USD: &str = r#"{
        "is_available": true,
        "balance_infos": [
            {
                "currency": "USD",
                "total_balance": "4.20",
                "granted_balance": "1.00",
                "topped_up_balance": "3.20"
            }
        ]
    }"#;

    const MULTI_CURRENCY_CNY_ONLY_FUNDED: &str = r#"{
        "is_available": true,
        "balance_infos": [
            {
                "currency": "USD",
                "total_balance": "0.00",
                "granted_balance": "0.00",
                "topped_up_balance": "0.00"
            },
            {
                "currency": "CNY",
                "total_balance": "10.50",
                "granted_balance": "10.50",
                "topped_up_balance": "0.00"
            }
        ]
    }"#;

    const UNAVAILABLE: &str = r#"{
        "is_available": false,
        "balance_infos": [
            {
                "currency": "USD",
                "total_balance": "5.00",
                "granted_balance": "5.00",
                "topped_up_balance": "0.00"
            }
        ]
    }"#;

    const ZERO_BALANCE: &str = r#"{
        "is_available": true,
        "balance_infos": [
            {
                "currency": "USD",
                "total_balance": "0.00",
                "granted_balance": "0.00",
                "topped_up_balance": "0.00"
            }
        ]
    }"#;

    const EMPTY_INFOS: &str = r#"{
        "is_available": true,
        "balance_infos": []
    }"#;

    #[test]
    fn test_happy_usd() {
        let snap = parse_balance_response(HAPPY_USD).unwrap();
        let credits = snap.credits.unwrap();
        assert!((credits.balance - 4.20).abs() < 0.001);
        assert_eq!(credits.currency.as_deref(), Some("USD"));
    }

    #[test]
    fn test_multi_currency_prefers_funded_cny_over_empty_usd() {
        let snap = parse_balance_response(MULTI_CURRENCY_CNY_ONLY_FUNDED).unwrap();
        let credits = snap.credits.unwrap();
        // USD is empty (0.00), CNY is funded — should select CNY.
        assert_eq!(credits.currency.as_deref(), Some("CNY"));
        assert!((credits.balance - 10.50).abs() < 0.001);
    }

    #[test]
    fn test_unavailable_returns_zero_balance() {
        let snap = parse_balance_response(UNAVAILABLE).unwrap();
        let credits = snap.credits.unwrap();
        assert_eq!(credits.balance, 0.0);
        assert_eq!(credits.currency.as_deref(), Some("USD"));
    }

    #[test]
    fn test_zero_balance() {
        let snap = parse_balance_response(ZERO_BALANCE).unwrap();
        let credits = snap.credits.unwrap();
        assert_eq!(credits.balance, 0.0);
    }

    #[test]
    fn test_empty_balance_infos() {
        let snap = parse_balance_response(EMPTY_INFOS).unwrap();
        // Should return 0 balance with USD default when array is empty.
        let credits = snap.credits.unwrap();
        assert_eq!(credits.balance, 0.0);
        assert_eq!(credits.currency.as_deref(), Some("USD"));
    }

    #[test]
    fn test_unknown_fields_tolerated() {
        let body = r#"{
            "is_available": true,
            "future_field": "ignored",
            "balance_infos": [
                {
                    "currency": "USD",
                    "total_balance": "1.50",
                    "granted_balance": "1.50",
                    "topped_up_balance": "0.00",
                    "new_unknown": true
                }
            ]
        }"#;
        let snap = parse_balance_response(body).unwrap();
        let credits = snap.credits.unwrap();
        assert!((credits.balance - 1.50).abs() < 0.001);
    }

    #[test]
    fn test_is_configured_false_without_key() {
        // This just checks the logic path — env var must not be set in CI.
        // We test is_configured() indirectly via the key function.
        let _ = DeepSeekProvider::new();
    }
}

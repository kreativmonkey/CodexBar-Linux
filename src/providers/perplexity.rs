use crate::model::{Credits, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use tracing::debug;

// HTTP transport shared with other providers.
use super::claude::http_get;

pub struct PerplexityProvider;

impl PerplexityProvider {
    pub fn new() -> Self {
        Self
    }
}

fn api_key() -> Option<String> {
    crate::config::api_key("perplexity", "PERPLEXITY_API_KEY")
}

// ── response parsing ──────────────────────────────────────────────────────────

/// Parses the JSON body from the Perplexity credits endpoint.
///
/// The Swift reference hits:
///   `GET https://www.perplexity.ai/rest/billing/credits?version=2.18&source=default`
/// authenticated via a session cookie (`__Secure-next-auth.session-token`).
///
/// Response shape:
/// ```json
/// {
///   "balance_cents": 12345,
///   "renewal_date_ts": 1750000000,
///   "current_period_purchased_cents": 0,
///   "total_usage_cents": 678,
///   "credit_grants": [
///     { "type": "recurring", "amount_cents": 10000 },
///     { "type": "promotional", "amount_cents": 5000, "expires_at_ts": 1750000000 }
///   ]
/// }
/// ```
/// We map `balance_cents / 100` → Credits.balance (USD).
pub(crate) fn parse_credits_response(body: &str) -> anyhow::Result<UsageSnapshot> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("Perplexity credits response is not JSON")?;

    let balance_cents = json
        .get("balance_cents")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let balance = (balance_cents / 100.0).max(0.0);

    debug!("perplexity: balance_cents={balance_cents} → balance={balance:.2}");

    Ok(UsageSnapshot {
        credits: Some(Credits {
            balance,
            currency: Some("USD".to_string()),
        }),
        fetched_at: Some(chrono::Utc::now()),
        ..Default::default()
    })
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for PerplexityProvider {
    fn id(&self) -> &'static str {
        "perplexity"
    }

    fn display_name(&self) -> &'static str {
        "Perplexity"
    }

    fn is_configured(&self) -> bool {
        api_key().is_some()
    }

    /// Fetches credit balance from Perplexity's billing REST endpoint.
    ///
    /// **Important limitation**: The macOS reference authenticates this endpoint
    /// with a browser session cookie (`__Secure-next-auth.session-token`), not an
    /// API key.  `PERPLEXITY_API_KEY` is the inference API token (pplx API,
    /// `api.perplexity.ai`) and is **not** accepted by the billing endpoint.
    ///
    /// Until Perplexity publishes a usage/credits REST endpoint for API keys,
    /// this provider returns an actionable error.  If you have a session token,
    /// set `PERPLEXITY_SESSION_TOKEN` — a future version of this provider will
    /// use it to fetch live credit data from the web billing endpoint.
    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        let _ = api_key().context(
            "Perplexity API key not set — export PERPLEXITY_API_KEY or add \
             keys.perplexity to ~/.config/codexbar/config.toml",
        )?;

        // Attempt the credits endpoint with the API key as Bearer auth.
        // As of 2026, the pplx REST billing endpoint is not publicly documented
        // for API keys, but we try it in case it is silently supported.
        let key = api_key().unwrap();
        let auth_header = format!("Bearer {key}");
        let headers = [
            ("Authorization", auth_header.as_str()),
            ("Accept", "application/json"),
        ];

        let (status, body) = http_get(
            "https://www.perplexity.ai/rest/billing/credits?version=2.18&source=default",
            &headers,
        )
        .await?;

        match status {
            200 => parse_credits_response(&body),
            401 | 403 => bail!(
                "Perplexity billing endpoint rejected the API key. \
                 The billing endpoint requires a browser session token, not an API key. \
                 Sign into perplexity.ai and export PERPLEXITY_SESSION_TOKEN, \
                 or view usage at https://www.perplexity.ai/account/usage."
            ),
            404 => bail!(
                "Perplexity billing endpoint not found (HTTP 404). \
                 The credits API may require a session token — see \
                 https://www.perplexity.ai/account/usage."
            ),
            429 => bail!("Perplexity rate-limited; try again shortly."),
            other => bail!("Perplexity billing: HTTP {other}"),
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const HAPPY_RESPONSE: &str = r#"{
        "balance_cents": 4200,
        "renewal_date_ts": 1750000000,
        "current_period_purchased_cents": 0,
        "total_usage_cents": 800,
        "credit_grants": [
            { "type": "recurring", "amount_cents": 5000 }
        ]
    }"#;

    const ZERO_BALANCE: &str = r#"{
        "balance_cents": 0,
        "renewal_date_ts": 1750000000,
        "current_period_purchased_cents": 0,
        "total_usage_cents": 5000,
        "credit_grants": []
    }"#;

    const WITH_PROMO: &str = r#"{
        "balance_cents": 9500,
        "renewal_date_ts": 1750000000,
        "current_period_purchased_cents": 0,
        "total_usage_cents": 500,
        "credit_grants": [
            { "type": "recurring", "amount_cents": 5000 },
            { "type": "promotional", "amount_cents": 5000, "expires_at_ts": 1760000000 }
        ]
    }"#;

    const MISSING_BALANCE: &str = r#"{
        "renewal_date_ts": 1750000000,
        "credit_grants": []
    }"#;

    #[test]
    fn test_happy_path() {
        let snap = parse_credits_response(HAPPY_RESPONSE).unwrap();
        let credits = snap.credits.unwrap();
        // 4200 cents = $42.00
        assert!((credits.balance - 42.0).abs() < 0.001);
        assert_eq!(credits.currency.as_deref(), Some("USD"));
    }

    #[test]
    fn test_zero_balance() {
        let snap = parse_credits_response(ZERO_BALANCE).unwrap();
        let credits = snap.credits.unwrap();
        assert_eq!(credits.balance, 0.0);
    }

    #[test]
    fn test_with_promo_credits() {
        let snap = parse_credits_response(WITH_PROMO).unwrap();
        let credits = snap.credits.unwrap();
        // 9500 cents = $95.00
        assert!((credits.balance - 95.0).abs() < 0.001);
    }

    #[test]
    fn test_missing_balance_defaults_zero() {
        let snap = parse_credits_response(MISSING_BALANCE).unwrap();
        let credits = snap.credits.unwrap();
        assert_eq!(credits.balance, 0.0);
    }

    #[test]
    fn test_unknown_fields_tolerated() {
        let body = r#"{
            "balance_cents": 1000,
            "future_field": "ignored",
            "renewal_date_ts": 1750000000,
            "credit_grants": [
                { "type": "recurring", "amount_cents": 1000, "new_field": true }
            ]
        }"#;
        let snap = parse_credits_response(body).unwrap();
        let credits = snap.credits.unwrap();
        assert!((credits.balance - 10.0).abs() < 0.001);
    }

    #[test]
    fn test_provider_id() {
        let p = PerplexityProvider::new();
        assert_eq!(p.id(), "perplexity");
        assert_eq!(p.display_name(), "Perplexity");
    }

    #[test]
    fn test_is_configured_without_key() {
        if std::env::var("PERPLEXITY_API_KEY").is_err() {
            let p = PerplexityProvider::new();
            assert!(!p.is_configured());
        }
    }
}

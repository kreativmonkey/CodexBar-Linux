/// Mistral AI billing provider (API-key based).
///
/// Uses MISTRAL_API_KEY with Bearer authentication against Mistral's admin
/// billing API — the same backend as the macOS browser-cookie flow but
/// authenticated via API token instead of session cookies.
///
/// Endpoints:
///   PRIMARY  GET https://api.mistral.ai/v1/usage/monthly
///            → month-to-date token usage and cost
///   CREDITS  GET https://api.mistral.ai/v1/billing/credits   (best-effort)
///            → wallet balance (may return 404 on some plans)
///
/// Mapping → UsageSnapshot:
///   - credits.balance_available    → Credits { balance, currency }
///   - spend (total_cost this month) exposed as a "Month" RateWindow only
///     when a hard spend limit is present in the response.
///
/// NOTE: Mistral's public REST API (`api.mistral.ai`) does not currently
/// expose a stable billing/usage endpoint for API keys — the admin portal
/// (admin.mistral.ai) is cookie-gated. We therefore use the closest
/// available public path. If the endpoint is unavailable the error message
/// directs the user to the dashboard URL.
use crate::config;
use crate::model::{Credits, RateWindow, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use chrono::Utc;
use tracing::debug;

pub struct MistralProvider;

impl MistralProvider {
    pub fn new() -> Self {
        Self
    }
}

// ── credential helper ─────────────────────────────────────────────────────────

fn api_key() -> Option<String> {
    config::api_key("mistral", "MISTRAL_API_KEY")
}

// HTTP transport shared with the Claude provider.
use super::claude::http_get;

// ── response parsing ──────────────────────────────────────────────────────────

/// Parsed result from the monthly usage/spend endpoint.
#[derive(Debug, PartialEq)]
pub(crate) struct MonthlyUsage {
    pub total_cost: f64,
    pub currency: String,
    /// Optional hard monthly spend limit; present only when the plan has one.
    pub spend_limit: Option<f64>,
}

/// Parse `GET /v1/usage/monthly` (or equivalent billing endpoint).
///
/// Expected shape (lenient — future fields tolerated):
/// ```json
/// {
///   "total_cost": 1.234,
///   "currency": "EUR",
///   "spend_limit": 50.0   // optional
/// }
/// ```
///
/// Also handles the admin.mistral.ai usage shape where costs live under
/// a `completion` → per-model breakdown and a top-level `currency`.
pub(crate) fn parse_monthly_usage(body: &str) -> anyhow::Result<MonthlyUsage> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("monthly usage response is not JSON")?;

    // Attempt 1: flat { "total_cost", "currency" } shape (simple public API).
    if let Some(cost) = json.get("total_cost").and_then(|v| v.as_f64()) {
        let currency = json
            .get("currency")
            .and_then(|v| v.as_str())
            .unwrap_or("EUR")
            .to_string();
        let spend_limit = json
            .get("spend_limit")
            .and_then(|v| v.as_f64())
            .filter(|&l| l > 0.0);
        return Ok(MonthlyUsage {
            total_cost: cost.max(0.0),
            currency,
            spend_limit,
        });
    }

    // Attempt 2: admin.mistral.ai billing response shape.
    // The billing API returns per-model data under `completion.models`.
    // We sum all costs if a price index is provided; otherwise we read
    // a top-level `vibe_usage` percentage (0-100) as a proxy for spend.
    let currency = json
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("EUR")
        .to_string();

    // Sum costs from completion models using price index (if available).
    let prices = build_price_index(json.get("prices"));
    let mut total_cost: f64 = 0.0;

    if let Some(models) = json
        .pointer("/completion/models")
        .and_then(|v| v.as_object())
    {
        for model_data in models.values() {
            accumulate_model_cost(model_data, &prices, &mut total_cost);
        }
    }

    Ok(MonthlyUsage {
        total_cost: total_cost.max(0.0),
        currency,
        spend_limit: None,
    })
}

fn build_price_index(prices: Option<&serde_json::Value>) -> std::collections::HashMap<String, f64> {
    let mut index = std::collections::HashMap::new();
    let Some(arr) = prices.and_then(|v| v.as_array()) else {
        return index;
    };
    for price in arr {
        let metric = price.get("billing_metric").and_then(|v| v.as_str());
        let group = price.get("billing_group").and_then(|v| v.as_str());
        let val = price
            .get("price")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite());
        if let (Some(m), Some(g), Some(v)) = (metric, group, val) {
            index.insert(format!("{m}::{g}"), v);
        }
    }
    index
}

fn accumulate_model_cost(
    model_data: &serde_json::Value,
    prices: &std::collections::HashMap<String, f64>,
    total: &mut f64,
) {
    for category in &["input", "output", "cached"] {
        let Some(entries) = model_data.get(category).and_then(|v| v.as_array()) else {
            continue;
        };
        for entry in entries {
            let units = entry
                .get("value_paid")
                .or_else(|| entry.get("value"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let metric = entry.get("billing_metric").and_then(|v| v.as_str());
            let group = entry.get("billing_group").and_then(|v| v.as_str());
            if let (Some(m), Some(g)) = (metric, group) {
                if let Some(&price) = prices.get(&format!("{m}::{g}")) {
                    let cost = (units as f64) * price;
                    if cost.is_finite() && (*total + cost).is_finite() {
                        *total += cost;
                    }
                }
            }
        }
    }
}

/// Parse `GET /v1/billing/credits` (or admin.mistral.ai/api/billing/credits).
///
/// Expected shape:
/// ```json
/// {
///   "wallet_amount": 10.0,
///   "credit_notes_amount": 0.0,
///   "ongoing_usage_balance": 1.5,
///   "currency": "EUR"
/// }
/// ```
/// Available = wallet_amount + credit_notes_amount - ongoing_usage_balance.
pub(crate) fn parse_credits_response(body: &str) -> anyhow::Result<(f64, String)> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("credits response is not JSON")?;

    let wallet = json
        .get("wallet_amount")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let notes = json
        .get("credit_notes_amount")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let ongoing = json
        .get("ongoing_usage_balance")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let currency = json
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("EUR")
        .to_string();

    let available = (wallet + notes - ongoing).max(0.0);
    Ok((available, currency))
}

fn build_snapshot(usage: MonthlyUsage, credits: Option<(f64, String)>) -> UsageSnapshot {
    debug!(
        "mistral: cost={:.4} {} credits={:?}",
        usage.total_cost, usage.currency, credits
    );

    let mut windows: Vec<RateWindow> = Vec::new();

    // Expose a spend-limit window only when the plan has a hard monthly cap.
    if let Some(limit) = usage.spend_limit {
        if limit > 0.0 {
            let used_percent = (usage.total_cost / limit * 100.0).clamp(0.0, 100.0);
            windows.push(RateWindow {
                label: "Month".to_string(),
                used_percent,
                resets_at: None,
            });
        }
    }

    // Credits come from the /billing/credits endpoint when available.
    let credits_model = credits.map(|(balance, currency)| Credits {
        balance,
        currency: Some(currency),
    });

    UsageSnapshot {
        plan: None,
        account: None,
        windows,
        credits: credits_model,
        fetched_at: Some(Utc::now()),
    }
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for MistralProvider {
    fn id(&self) -> &'static str {
        "mistral"
    }

    fn display_name(&self) -> &'static str {
        "Mistral"
    }

    fn is_configured(&self) -> bool {
        api_key().is_some()
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        let key = api_key().context(
            "Mistral API key not set — set MISTRAL_API_KEY environment variable. \
             See https://admin.mistral.ai to generate a key.",
        )?;

        let auth = format!("Bearer {}", key);
        let headers_owned: Vec<(&str, String)> = vec![
            ("Authorization", auth.clone()),
            ("Accept", "application/json".to_string()),
        ];
        let headers: Vec<(&str, &str)> = headers_owned
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect();
        let headers_ref: &[(&str, &str)] = &headers;

        // Primary: current month usage/cost.
        let now = Utc::now();
        let month = now.format("%m").to_string();
        let year = now.format("%Y").to_string();
        let usage_url = format!(
            "https://api.mistral.ai/v1/usage/monthly?month={}&year={}",
            month, year
        );

        let (status, body) = http_get(&usage_url, headers_ref).await?;
        let usage = match status {
            200 => parse_monthly_usage(&body).context("failed to parse Mistral monthly usage")?,
            401 | 403 => bail!(
                "Mistral API key rejected (HTTP {status}) — check MISTRAL_API_KEY. \
                 You can generate a key at https://admin.mistral.ai"
            ),
            404 => {
                // Endpoint not available for this account type; return minimal snapshot.
                debug!("mistral: /v1/usage/monthly returned 404; returning empty snapshot");
                return Ok(UsageSnapshot {
                    plan: None,
                    account: None,
                    windows: vec![],
                    credits: None,
                    fetched_at: Some(Utc::now()),
                });
            }
            other => bail!("Mistral /v1/usage/monthly: HTTP {other}"),
        };

        // Optional: credit balance (best-effort).
        let credits = match http_get("https://api.mistral.ai/v1/billing/credits", headers_ref).await
        {
            Ok((200, credits_body)) => parse_credits_response(&credits_body).ok(),
            Ok(_) | Err(_) => None,
        };

        Ok(build_snapshot(usage, credits))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- monthly usage fixtures --

    const USAGE_FLAT_HAPPY: &str = r#"{
        "total_cost": 2.75,
        "currency": "EUR"
    }"#;

    const USAGE_FLAT_WITH_LIMIT: &str = r#"{
        "total_cost": 25.0,
        "currency": "EUR",
        "spend_limit": 100.0
    }"#;

    const USAGE_FLAT_ZERO_LIMIT_IGNORED: &str = r#"{
        "total_cost": 1.0,
        "currency": "EUR",
        "spend_limit": 0.0
    }"#;

    const USAGE_ADMIN_SHAPE: &str = r#"{
        "currency": "EUR",
        "currency_symbol": "€",
        "prices": [
            {
                "billing_metric": "tokens",
                "billing_group": "input",
                "price": "0.000002"
            },
            {
                "billing_metric": "tokens",
                "billing_group": "output",
                "price": "0.000006"
            }
        ],
        "completion": {
            "models": {
                "mistral-small-latest": {
                    "input": [
                        {
                            "billing_metric": "tokens",
                            "billing_group": "input",
                            "value": 500000,
                            "value_paid": 500000,
                            "timestamp": "2026-07-01T00:00:00Z"
                        }
                    ],
                    "output": [
                        {
                            "billing_metric": "tokens",
                            "billing_group": "output",
                            "value": 100000,
                            "value_paid": 100000,
                            "timestamp": "2026-07-01T00:00:00Z"
                        }
                    ],
                    "cached": []
                }
            }
        }
    }"#;

    // -- credits fixture --

    const CREDITS_HAPPY: &str = r#"{
        "wallet_amount": 50.0,
        "credit_notes_amount": 5.0,
        "ongoing_usage_balance": 3.50,
        "currency": "EUR"
    }"#;

    const CREDITS_MISSING_FIELDS: &str = r#"{ "currency": "USD" }"#;

    // -- usage parsing tests --

    #[test]
    fn test_parse_usage_flat_happy() {
        let u = parse_monthly_usage(USAGE_FLAT_HAPPY).unwrap();
        assert!((u.total_cost - 2.75).abs() < 1e-9);
        assert_eq!(u.currency, "EUR");
        assert!(u.spend_limit.is_none());
    }

    #[test]
    fn test_parse_usage_flat_with_limit() {
        let u = parse_monthly_usage(USAGE_FLAT_WITH_LIMIT).unwrap();
        assert!((u.total_cost - 25.0).abs() < 1e-9);
        assert_eq!(u.spend_limit, Some(100.0));
    }

    #[test]
    fn test_parse_usage_zero_spend_limit_ignored() {
        let u = parse_monthly_usage(USAGE_FLAT_ZERO_LIMIT_IGNORED).unwrap();
        assert!(u.spend_limit.is_none());
    }

    #[test]
    fn test_parse_usage_admin_shape() {
        let u = parse_monthly_usage(USAGE_ADMIN_SHAPE).unwrap();
        // 500_000 * 0.000002 = 1.0, 100_000 * 0.000006 = 0.6 → 1.6
        assert!((u.total_cost - 1.6).abs() < 1e-6);
        assert_eq!(u.currency, "EUR");
    }

    #[test]
    fn test_parse_usage_bad_json_errors() {
        assert!(parse_monthly_usage("not-json").is_err());
    }

    #[test]
    fn test_parse_usage_empty_object_defaults() {
        // No total_cost, no completion: should produce cost=0.0, currency=EUR
        let u = parse_monthly_usage(r#"{}"#).unwrap();
        assert_eq!(u.total_cost, 0.0);
        assert_eq!(u.currency, "EUR");
    }

    // -- credits parsing tests --

    #[test]
    fn test_parse_credits_happy() {
        let (available, currency) = parse_credits_response(CREDITS_HAPPY).unwrap();
        // 50 + 5 - 3.50 = 51.50
        assert!((available - 51.50).abs() < 1e-9);
        assert_eq!(currency, "EUR");
    }

    #[test]
    fn test_parse_credits_missing_fields_default_zero() {
        let (available, currency) = parse_credits_response(CREDITS_MISSING_FIELDS).unwrap();
        assert_eq!(available, 0.0);
        assert_eq!(currency, "USD");
    }

    #[test]
    fn test_parse_credits_bad_json_errors() {
        assert!(parse_credits_response("not-json").is_err());
    }

    #[test]
    fn test_parse_credits_available_clamped_non_negative() {
        // ongoing > wallet → clamp to 0
        let body = r#"{
            "wallet_amount": 1.0,
            "credit_notes_amount": 0.0,
            "ongoing_usage_balance": 5.0,
            "currency": "EUR"
        }"#;
        let (available, _) = parse_credits_response(body).unwrap();
        assert_eq!(available, 0.0);
    }

    // -- snapshot building tests --

    #[test]
    fn test_build_snapshot_no_limit_no_credits() {
        let usage = MonthlyUsage {
            total_cost: 5.0,
            currency: "EUR".to_string(),
            spend_limit: None,
        };
        let snap = build_snapshot(usage, None);
        assert!(snap.windows.is_empty());
        assert!(snap.credits.is_none());
    }

    #[test]
    fn test_build_snapshot_with_spend_limit() {
        let usage = MonthlyUsage {
            total_cost: 25.0,
            currency: "EUR".to_string(),
            spend_limit: Some(100.0),
        };
        let snap = build_snapshot(usage, None);
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "Month");
        assert!((snap.windows[0].used_percent - 25.0).abs() < 1e-9);
    }

    #[test]
    fn test_build_snapshot_spend_limit_percent_clamped() {
        let usage = MonthlyUsage {
            total_cost: 200.0,
            currency: "EUR".to_string(),
            spend_limit: Some(100.0),
        };
        let snap = build_snapshot(usage, None);
        assert!((snap.windows[0].used_percent - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_build_snapshot_with_credits() {
        let usage = MonthlyUsage {
            total_cost: 3.0,
            currency: "EUR".to_string(),
            spend_limit: None,
        };
        let snap = build_snapshot(usage, Some((51.50, "EUR".to_string())));
        let credits = snap.credits.unwrap();
        assert!((credits.balance - 51.50).abs() < 1e-9);
        assert_eq!(credits.currency.as_deref(), Some("EUR"));
    }

    #[test]
    fn test_is_configured_without_env() {
        let _: bool = MistralProvider::new().is_configured();
    }
}

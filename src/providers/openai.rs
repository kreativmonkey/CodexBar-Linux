/// OpenAI platform API provider.
///
/// Key resolution order (mirroring OpenAIAPISettingsReader in the Swift ref):
///   1. OPENAI_ADMIN_KEY  — org-level admin key; grants /v1/organization/costs
///   2. OPENAI_API_KEY    — regular key; only /v1/dashboard/billing/credit_grants works
///
/// Fetch strategy:
///   A) Admin key present → fetch 30-day costs from /v1/organization/costs.
///      On auth failure fall through to B.
///   B) Any key → fetch credit-grant balance from
///      /v1/dashboard/billing/credit_grants.
///
/// The UsageSnapshot maps as follows:
///   A) month-to-date spend → Credits { balance = spend, currency = "USD" },
///      no rate windows (no hard limit in the costs API).
///   B) credit grants → Credits { balance = total_available },
///      one RateWindow "Credits" (percent of total_granted used).
use crate::config;
use crate::model::{Credits, RateWindow, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use chrono::Utc;
use tracing::debug;

pub struct OpenAIProvider;

impl OpenAIProvider {
    pub fn new() -> Self {
        Self
    }
}

// ── credential helpers ────────────────────────────────────────────────────────

fn admin_key() -> Option<String> {
    config::api_key("openai", "OPENAI_ADMIN_KEY")
}

fn any_key() -> Option<String> {
    admin_key().or_else(|| config::api_key("openai", "OPENAI_API_KEY"))
}

// HTTP transport shared with the Claude provider.
use super::claude::http_get;

// ── costs endpoint (admin key required) ───────────────────────────────────────

/// Fetch the 30-day month-to-date spend via /v1/organization/costs.
///
/// Returns total cost in USD, or an error that the caller can treat as
/// a signal to fall through to the balance endpoint.
///
/// The endpoint paginates with `has_more` / `next_page`, but for a tray
/// monitor we fetch only the first page (bucket_width=1d, last 30 days
/// fits in one request under the default limit).
async fn fetch_monthly_spend(api_key: &str) -> anyhow::Result<f64> {
    let now = Utc::now();
    let end_time = now.timestamp();
    let start_time = end_time - 30 * 24 * 3600;

    let url = format!(
        "https://api.openai.com/v1/organization/costs\
         ?start_time={start_time}&end_time={end_time}\
         &bucket_width=1d&group_by=line_item&limit=31"
    );

    let auth = format!("Bearer {}", api_key);
    let headers: &[(&str, &str)] = &[
        ("Authorization", auth.as_str()),
        ("Accept", "application/json"),
    ];

    let (status, body) = http_get(&url, headers).await?;

    match status {
        200 => {}
        401 | 403 => bail!("openai-costs-auth:{status}"), // sentinel for fallthrough
        other => bail!("OpenAI organization/costs: HTTP {other}"),
    }

    parse_costs_response(&body)
}

/// Parse `GET /v1/organization/costs` response.
///
/// Shape: `{ "data": [{ "start_time": i64, "end_time": i64,
///            "results": [{ "amount": { "value": f64, "currency": str },
///                          "line_item": str|null }] }],
///           "has_more": bool, "next_page": str|null }`
fn parse_costs_response(body: &str) -> anyhow::Result<f64> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("costs response is not JSON")?;

    let buckets = json
        .get("data")
        .and_then(|v| v.as_array())
        .context("costs: missing 'data' array")?;

    let mut total: f64 = 0.0;
    for bucket in buckets {
        let empty = vec![];
        let results = bucket
            .get("results")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        for result in results {
            if let Some(value) = result
                .get("amount")
                .and_then(|a| a.get("value"))
                .and_then(|v| v.as_f64())
            {
                if value.is_finite() {
                    total += value;
                }
            }
        }
    }

    debug!("openai: month-to-date spend ${:.4}", total);
    Ok(total)
}

// ── credit grants endpoint (any key) ─────────────────────────────────────────

/// Parse `GET /v1/dashboard/billing/credit_grants` response.
///
/// Shape: `{ "total_granted": f64, "total_used": f64, "total_available": f64,
///           "grants": { "data": [{ "grant_amount": f64, "used_amount": f64,
///                                   "expires_at": f64|null }] } }`
fn parse_credit_grants_response(body: &str) -> anyhow::Result<(f64, f64, f64)> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("credit_grants response is not JSON")?;

    let total_granted = json
        .get("total_granted")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let total_used = json
        .get("total_used")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let total_available = json
        .get("total_available")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    Ok((total_granted, total_used, total_available))
}

fn build_snapshot_from_spend(spend_usd: f64) -> UsageSnapshot {
    debug!("openai: building snapshot from spend ${:.4}", spend_usd);
    UsageSnapshot {
        plan: None,
        account: None,
        windows: vec![],
        credits: Some(Credits::from_balance(spend_usd.max(0.0), Some("USD".to_string()))),
        fetched_at: Some(Utc::now()),
    }
}

fn build_snapshot_from_grants(
    total_granted: f64,
    total_used: f64,
    total_available: f64,
) -> UsageSnapshot {
    debug!(
        "openai: grants granted={:.2} used={:.2} available={:.2}",
        total_granted, total_used, total_available
    );

    let used_percent = if total_granted > 0.0 {
        (total_used / total_granted * 100.0).clamp(0.0, 100.0)
    } else if total_available > 0.0 {
        0.0
    } else {
        100.0
    };

    let windows = vec![RateWindow {
        label: "Credits".to_string(),
        used_percent,
        resets_at: None,
    }];

    UsageSnapshot {
        plan: None,
        account: None,
        windows,
        credits: Some(Credits::from_balance(
            total_available.max(0.0),
            Some("USD".to_string()),
        )),
        fetched_at: Some(Utc::now()),
    }
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn display_name(&self) -> &'static str {
        "OpenAI"
    }

    fn is_configured(&self) -> bool {
        any_key().is_some()
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        // Try admin key + usage costs first.
        if let Some(admin) = admin_key() {
            match fetch_monthly_spend(&admin).await {
                Ok(spend) => return Ok(build_snapshot_from_spend(spend)),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.starts_with("openai-costs-auth:") {
                        // Auth rejected: key exists but doesn't have org-level access.
                        // Fall through to the credit-grants endpoint below.
                        debug!(
                            "openai: admin key rejected by costs endpoint, trying credit_grants"
                        );
                    } else {
                        return Err(e.context(
                            "OpenAI organization/costs failed — \
                             set OPENAI_ADMIN_KEY to an org Admin API key",
                        ));
                    }
                }
            }
        }

        // Fall back to credit grants (works with regular API keys).
        let key =
            any_key().context("OpenAI API key not set — set OPENAI_ADMIN_KEY or OPENAI_API_KEY")?;

        let auth = format!("Bearer {}", key);
        let headers: &[(&str, &str)] = &[
            ("Authorization", auth.as_str()),
            ("Accept", "application/json"),
        ];

        let (status, body) = http_get(
            "https://api.openai.com/v1/dashboard/billing/credit_grants",
            headers,
        )
        .await?;

        match status {
            200 => {}
            401 => bail!(
                "OpenAI key rejected (HTTP 401) — use an org Admin API key \
                 (OPENAI_ADMIN_KEY) for organization usage; project and \
                 service-account keys do not provide that access"
            ),
            403 => bail!(
                "OpenAI billing/credit_grants returned HTTP 403 — this endpoint \
                 requires a legacy user API key with billing access"
            ),
            other => bail!("OpenAI billing/credit_grants: HTTP {other}"),
        }

        let (granted, used, available) =
            parse_credit_grants_response(&body).context("failed to parse OpenAI credit_grants")?;

        Ok(build_snapshot_from_grants(granted, used, available))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- costs endpoint fixtures --

    const COSTS_HAPPY: &str = r#"{
        "data": [
            {
                "start_time": 1750000000,
                "end_time": 1750086400,
                "results": [
                    { "amount": { "value": 1.5, "currency": "USD" }, "line_item": "chat" },
                    { "amount": { "value": 0.25, "currency": "USD" }, "line_item": "embeddings" }
                ]
            },
            {
                "start_time": 1750086400,
                "end_time": 1750172800,
                "results": [
                    { "amount": { "value": 0.75, "currency": "USD" }, "line_item": "chat" }
                ]
            }
        ],
        "has_more": false,
        "next_page": null
    }"#;

    const COSTS_EMPTY: &str = r#"{
        "data": [],
        "has_more": false,
        "next_page": null
    }"#;

    const COSTS_NULL_AMOUNT: &str = r#"{
        "data": [
            {
                "start_time": 1750000000,
                "end_time": 1750086400,
                "results": [
                    { "amount": null, "line_item": "chat" },
                    { "amount": { "value": 2.0, "currency": "USD" }, "line_item": "embeddings" }
                ]
            }
        ],
        "has_more": false,
        "next_page": null
    }"#;

    // -- credit grants fixtures --

    const GRANTS_HAPPY: &str = r#"{
        "object": "credit_summary",
        "total_granted": 100.0,
        "total_used": 42.5,
        "total_available": 57.5,
        "grants": {
            "object": "list",
            "data": [
                { "grant_amount": 100.0, "used_amount": 42.5, "expires_at": 1800000000.0 }
            ]
        }
    }"#;

    const GRANTS_ZERO_GRANTED: &str = r#"{
        "total_granted": 0.0,
        "total_used": 0.0,
        "total_available": 25.0
    }"#;

    const GRANTS_EMPTY: &str = r#"{
        "total_granted": 0.0,
        "total_used": 0.0,
        "total_available": 0.0
    }"#;

    // -- costs tests --

    #[test]
    fn test_parse_costs_happy() {
        let total = parse_costs_response(COSTS_HAPPY).unwrap();
        // 1.5 + 0.25 + 0.75 = 2.50
        assert!((total - 2.5).abs() < 1e-9);
    }

    #[test]
    fn test_parse_costs_empty() {
        let total = parse_costs_response(COSTS_EMPTY).unwrap();
        assert_eq!(total, 0.0);
    }

    #[test]
    fn test_parse_costs_null_amount_skipped() {
        let total = parse_costs_response(COSTS_NULL_AMOUNT).unwrap();
        assert!((total - 2.0).abs() < 1e-9);
    }

    #[test]
    fn test_parse_costs_bad_json_errors() {
        assert!(parse_costs_response("not-json").is_err());
        assert!(parse_costs_response(r#"{"data": "wrong"}"#).is_err());
    }

    #[test]
    fn test_build_snapshot_from_spend() {
        let snap = build_snapshot_from_spend(12.34);
        assert!(snap.windows.is_empty());
        let credits = snap.credits.unwrap();
        assert!((credits.balance - 12.34).abs() < 1e-9);
        assert_eq!(credits.currency.as_deref(), Some("USD"));
    }

    #[test]
    fn test_build_snapshot_from_spend_clamped_non_negative() {
        let snap = build_snapshot_from_spend(-1.0);
        assert_eq!(snap.credits.unwrap().balance, 0.0);
    }

    // -- credit grants tests --

    #[test]
    fn test_parse_grants_happy() {
        let (granted, used, available) = parse_credit_grants_response(GRANTS_HAPPY).unwrap();
        assert!((granted - 100.0).abs() < 1e-9);
        assert!((used - 42.5).abs() < 1e-9);
        assert!((available - 57.5).abs() < 1e-9);
    }

    #[test]
    fn test_parse_grants_zero_granted() {
        let (granted, used, available) = parse_credit_grants_response(GRANTS_ZERO_GRANTED).unwrap();
        assert_eq!(granted, 0.0);
        assert_eq!(used, 0.0);
        assert!((available - 25.0).abs() < 1e-9);
    }

    #[test]
    fn test_parse_grants_empty() {
        let (granted, used, available) = parse_credit_grants_response(GRANTS_EMPTY).unwrap();
        assert_eq!(granted, 0.0);
        assert_eq!(used, 0.0);
        assert_eq!(available, 0.0);
    }

    #[test]
    fn test_parse_grants_missing_fields_defaults_to_zero() {
        let (granted, used, available) = parse_credit_grants_response(r#"{}"#).unwrap();
        assert_eq!(granted, 0.0);
        assert_eq!(used, 0.0);
        assert_eq!(available, 0.0);
    }

    #[test]
    fn test_parse_grants_bad_json_errors() {
        assert!(parse_credit_grants_response("not-json").is_err());
    }

    #[test]
    fn test_build_snapshot_from_grants_happy() {
        let snap = build_snapshot_from_grants(100.0, 42.5, 57.5);
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "Credits");
        // 42.5 / 100 * 100 = 42.5%
        assert!((snap.windows[0].used_percent - 42.5).abs() < 1e-9);
        let credits = snap.credits.unwrap();
        assert!((credits.balance - 57.5).abs() < 1e-9);
        assert_eq!(credits.currency.as_deref(), Some("USD"));
    }

    #[test]
    fn test_build_snapshot_from_grants_zero_granted_with_available() {
        // total_granted = 0, total_available > 0 → 0% used
        let snap = build_snapshot_from_grants(0.0, 0.0, 25.0);
        assert!((snap.windows[0].used_percent).abs() < 1e-9);
    }

    #[test]
    fn test_build_snapshot_from_grants_both_zero() {
        // total_granted = 0, total_available = 0 → 100% used (exhausted)
        let snap = build_snapshot_from_grants(0.0, 0.0, 0.0);
        assert!((snap.windows[0].used_percent - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_build_snapshot_from_grants_percent_clamped() {
        // used > granted → clamp to 100%
        let snap = build_snapshot_from_grants(50.0, 200.0, 0.0);
        assert!((snap.windows[0].used_percent - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_build_snapshot_from_grants_empty() {
        let snap = build_snapshot_from_grants(0.0, 0.0, 0.0);
        // balance clamped to 0
        assert_eq!(snap.credits.unwrap().balance, 0.0);
    }
}
